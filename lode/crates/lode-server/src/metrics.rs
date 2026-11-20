//! Prometheus metrics at `/metrics` (T181), off unless `[metrics] enabled = true`.
//!
//! The text format is written by hand (no new dependency). Counters live in memory and start
//! at zero with the process; gauges are read when scraped: depots and revisions from the
//! database, open shelves, held locks and webhook deliveries from the modules, sizes,
//! certificate expiry and the newest backup from disk.
//!
//! Readable with a token that has the `metrics` scope, or from an address in
//! `[metrics] allow_from`. Nothing here names a path or a user.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::{header, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::auth::Caller;
use crate::config::TlsConfig;
use crate::error::ApiError;
use crate::server::ClientInfo;
use crate::state::AppState;

/// The scope a token needs to read `/metrics`.
pub const SCOPE_METRICS: &str = "metrics";
/// Where the metrics are served.
pub const PATH: &str = "/metrics";
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Whether a sample only goes up (resets with the process) or goes up and down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Goes only up.
    Counter,
    /// Any value.
    Gauge,
}

/// One value of one metric.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Metric name, such as `lode_shelves_open`.
    pub name: &'static str,
    /// One line on what it counts.
    pub help: &'static str,
    /// Counter or gauge.
    pub kind: Kind,
    /// Label names and values.
    pub labels: Vec<(&'static str, String)>,
    /// The value.
    pub value: f64,
}

impl Sample {
    /// A gauge without labels.
    pub fn gauge(name: &'static str, help: &'static str, value: f64) -> Self {
        Self {
            name,
            help,
            kind: Kind::Gauge,
            labels: Vec::new(),
            value,
        }
    }

    /// A counter without labels.
    pub fn counter(name: &'static str, help: &'static str, value: f64) -> Self {
        Self {
            kind: Kind::Counter,
            ..Self::gauge(name, help, value)
        }
    }

    /// The same sample with one more label.
    pub fn label(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.labels.push((name, value.into()));
        self
    }
}

/// Counters the server keeps while it runs.
#[derive(Debug, Default)]
pub struct Counters {
    requests: Mutex<BTreeMap<(&'static str, u16), u64>>,
    commits: AtomicU64,
    blob_bytes_in: AtomicU64,
    blob_bytes_out: AtomicU64,
}

impl Counters {
    fn request(&self, class: &'static str, status: u16) {
        let mut map = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *map.entry((class, status)).or_default() += 1;
    }

    fn samples(&self) -> Vec<Sample> {
        let map = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut out: Vec<Sample> = map
            .into_iter()
            .map(|((class, status), n)| {
                Sample::counter(
                    "lode_http_requests_total",
                    "HTTP requests answered, by kind of route and status code.",
                    n as f64,
                )
                .label("class", class)
                .label("code", status.to_string())
            })
            .collect();
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64;
        out.push(Sample::counter(
            "lode_commits_total",
            "Commits accepted since the server started.",
            get(&self.commits),
        ));
        out.push(Sample::counter(
            "lode_blob_bytes_received_total",
            "File content bytes received in uploads (as sent, possibly compressed).",
            get(&self.blob_bytes_in),
        ));
        out.push(Sample::counter(
            "lode_blob_bytes_sent_total",
            "File content bytes sent in downloads (as sent, possibly compressed).",
            get(&self.blob_bytes_out),
        ));
        out
    }
}

/// The kind of route a path belongs to; a fixed set, so the label stays small.
fn class(method: &Method, path: &str) -> &'static str {
    let read = matches!(*method, Method::GET | Method::HEAD);
    if let Some(rest) = path.strip_prefix("/api/v1/") {
        let parts: Vec<&str> = rest.split('/').collect();
        let blob = parts.len() >= 4 && parts[0] == "depots" && parts[2] == "blobs";
        let commit = parts.len() == 5 && parts[0] == "depots" && parts[4] == "commits";
        return match (blob, commit, read) {
            (true, _, true) => "blob_download",
            (true, _, false) => "blob_upload",
            (_, true, false) => "commit",
            (_, _, true) => "api_read",
            (_, _, false) => "api_write",
        };
    }
    if path == "/admin" || path.starts_with("/admin/") {
        "admin_ui"
    } else if path == "/web" || path.starts_with("/web/") {
        "web_ui"
    } else if path == PATH {
        "metrics"
    } else {
        "other"
    }
}

/// `body` with each chunk's length added to `counter` as it passes.
fn counted(body: axum::body::Body, counter: &'static str, state: &AppState) -> axum::body::Body {
    use futures::StreamExt as _;
    let c = state.clone();
    axum::body::Body::from_stream(body.into_data_stream().inspect(move |chunk| {
        if let Ok(bytes) = chunk {
            let n = bytes.len() as u64;
            let counters = c.counters();
            let target = if counter == "in" {
                &counters.blob_bytes_in
            } else {
                &counters.blob_bytes_out
            };
            target.fetch_add(n, Ordering::Relaxed);
        }
    }))
}

/// Middleware: counts each request by class and status, commits, and blob bytes as they
/// stream.
pub async fn count(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let class = class(req.method(), req.uri().path());
    let req = if class == "blob_upload" {
        req.map(|b| counted(b, "in", &state))
    } else {
        req
    };
    let resp = next.run(req).await;
    let status = resp.status();
    state.counters().request(class, status.as_u16());
    if status.is_success() && class == "commit" {
        state.counters().commits.fetch_add(1, Ordering::Relaxed);
    }
    if class == "blob_download" && status.is_success() {
        resp.map(|b| counted(b, "out", &state))
    } else {
        resp
    }
}

/// The `/metrics` route.
pub fn routes(state: AppState) -> Router {
    Router::new().route(PATH, get(serve)).with_state(state)
}

async fn serve(State(state): State<AppState>, req: Request) -> Response {
    let (mut parts, _) = req.into_parts();
    let ip = parts
        .extensions
        .get::<ConnectInfo<ClientInfo>>()
        .map(|c| c.0 .0.ip());
    let allowed = ip.is_some_and(|ip| {
        state
            .config()
            .metrics
            .allow_from
            .iter()
            .any(|n| n.contains(ip))
    });
    if !allowed {
        match Caller::from_request_parts(&mut parts, &state).await {
            Ok(caller) => {
                if let Err(e) = caller.require_scope(SCOPE_METRICS) {
                    return e.into_response();
                }
            }
            Err(e) => return e.into_response(),
        }
    }
    match gather(&state).await {
        Ok(samples) => {
            let mut resp = render(&samples).into_response();
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE));
            resp
        }
        Err(e) => e.into_response(),
    }
}

/// Every sample, in a stable order.
pub async fn gather(state: &AppState) -> Result<Vec<Sample>, ApiError> {
    let now_ms = state.clock().now_ms();
    let mut out = vec![
        Sample::gauge("lode_build_info", "The server's version.", 1.0)
            .label("version", lode_build::VERSION),
        Sample::gauge(
            "lode_start_time_seconds",
            "When the server started, Unix seconds.",
            (state.started_at() / 1000) as f64,
        ),
    ];
    out.extend(state.counters().samples());

    let modules = state.modules().to_vec();
    let (depots, module_samples) = state
        .meta()
        .read(move |tx| {
            use lode_meta::Reads as _;
            let depots = tx.depots()?;
            let mut samples = Vec::new();
            for m in &modules {
                samples.extend(m.metrics(tx)?);
            }
            Ok((depots, samples))
        })
        .await?;
    out.push(Sample::gauge("lode_depots", "Depots.", depots.len() as f64));
    for d in &depots {
        out.push(
            Sample::gauge(
                "lode_depot_head_revision",
                "The newest revision number of each depot.",
                d.next_rev.saturating_sub(1) as f64,
            )
            .label("depot", d.name.clone()),
        );
    }
    out.extend(module_samples);

    let cfg = state.config();
    if cfg.storage.s3_in_use().is_none() {
        let dir = cfg.data_dir.join(crate::server::DEPOTS_DIR);
        if let Some(n) = state.sizes().size(&dir, now_ms).await {
            out.push(Sample::gauge(
                "lode_store_bytes",
                "Bytes of file content on disk (measured at most every few minutes).",
                n as f64,
            ));
        }
    }
    if state.meta().dialect() == lode_meta::Dialect::Sqlite {
        let db = cfg.data_dir.join(crate::server::META_DB_FILE);
        let len = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).ok();
        let mut wal = db.clone().into_os_string();
        wal.push("-wal");
        if let Some(n) = len(&db) {
            let total = n + len(std::path::Path::new(&wal)).unwrap_or(0);
            out.push(Sample::gauge(
                "lode_database_bytes",
                "Bytes of the SQLite database and its write-ahead log.",
                total as f64,
            ));
        }
    }
    let cert = match &cfg.tls {
        Some(TlsConfig::Files { cert, .. }) => Some(cert.clone()),
        Some(TlsConfig::Acme(a)) => Some(crate::acme::cached_cert_path(a, &cfg.data_dir)),
        None => None,
    };
    if let Some(path) = cert {
        if let Some(expires) = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|pem| crate::acme::pem_expiry(&pem))
        {
            out.push(Sample::gauge(
                "lode_certificate_expiry_timestamp_seconds",
                "When the served certificate expires, Unix seconds.",
                expires as f64,
            ));
        }
    }
    if let Some(schedule) = cfg.backup.schedule() {
        let dir = schedule.dir.clone();
        let newest = tokio::task::spawn_blocking(move || crate::maintenance::newest_backup(&dir))
            .await
            .ok()
            .flatten();
        if let Some((at, _)) = newest {
            out.push(Sample::gauge(
                "lode_backup_last_success_timestamp_seconds",
                "When the newest complete backup was made, Unix seconds.",
                at.unix_timestamp() as f64,
            ));
        }
        if let Some(run) = state.last_backup() {
            out.push(Sample::gauge(
                "lode_backup_last_run_ok",
                "1 when the last scheduled backup of this process succeeded, else 0.",
                if run.outcome.is_ok() { 1.0 } else { 0.0 },
            ));
        }
    }
    Ok(out)
}

fn escape(value: &str) -> String {
    let mut s = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            c => s.push(c),
        }
    }
    s
}

fn number(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The samples in the Prometheus text format, grouped by name in first-seen order, each
/// group with its `# HELP` and `# TYPE` lines.
pub fn render(samples: &[Sample]) -> String {
    let mut order: Vec<&'static str> = Vec::new();
    for s in samples {
        if !order.contains(&s.name) {
            order.push(s.name);
        }
    }
    let mut out = String::new();
    for name in order {
        let group: Vec<&Sample> = samples.iter().filter(|s| s.name == name).collect();
        let first = group[0];
        let kind = match first.kind {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
        };
        let _ = writeln!(out, "# HELP {name} {}", first.help);
        let _ = writeln!(out, "# TYPE {name} {kind}");
        for s in group {
            out.push_str(name);
            if !s.labels.is_empty() {
                let labels: Vec<String> = s
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
                    .collect();
                let _ = write!(out, "{{{}}}", labels.join(","));
            }
            let _ = writeln!(out, " {}", number(s.value));
        }
    }
    out
}

/// Whether `text` is in the Prometheus text format as [`render`] writes it: comment lines,
/// and `name{labels} value` lines with a declared type. For tests and `loded doctor`-style
/// checks; not a full parser.
pub fn check_text_format(text: &str) -> Result<usize, String> {
    let mut typed = std::collections::HashSet::new();
    let mut samples = 0;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let mut it = rest.split(' ');
            let name = it.next().unwrap_or_default();
            match it.next() {
                Some("counter" | "gauge" | "histogram" | "summary" | "untyped") => {}
                other => return Err(format!("line {n}: bad type {other:?}")),
            }
            typed.insert(name.to_owned());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (series, value) = line
            .rsplit_once(' ')
            .ok_or_else(|| format!("line {n}: no value"))?;
        value
            .parse::<f64>()
            .map_err(|_| format!("line {n}: bad value {value:?}"))?;
        let name = match series.split_once('{') {
            Some((name, labels)) => {
                let labels = labels
                    .strip_suffix('}')
                    .ok_or_else(|| format!("line {n}: unclosed labels"))?;
                for pair in split_labels(labels) {
                    let (k, v) = pair
                        .split_once('=')
                        .ok_or_else(|| format!("line {n}: bad label {pair:?}"))?;
                    if !valid_name(k) || !(v.starts_with('"') && v.ends_with('"') && v.len() >= 2) {
                        return Err(format!("line {n}: bad label {pair:?}"));
                    }
                }
                name
            }
            None => series,
        };
        if !valid_name(name) {
            return Err(format!("line {n}: bad metric name {name:?}"));
        }
        if !typed.contains(name) {
            return Err(format!("line {n}: {name} has no # TYPE line before it"));
        }
        samples += 1;
    }
    Ok(samples)
}

fn split_labels(labels: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in labels.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(&labels[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < labels.len() {
        out.push(&labels[start..]);
    }
    out
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_fall_into_a_few_classes() {
        let (g, p) = (Method::GET, Method::PUT);
        assert_eq!(class(&g, "/api/v1/depots/game/blobs/abc"), "blob_download");
        assert_eq!(
            class(&p, "/api/v1/depots/game/blobs/abc/chunks"),
            "blob_upload"
        );
        assert_eq!(
            class(&Method::POST, "/api/v1/depots/game/branches/main/commits"),
            "commit"
        );
        assert_eq!(class(&g, "/api/v1/depots/game/tree"), "api_read");
        assert_eq!(class(&Method::POST, "/api/v1/tokens"), "api_write");
        assert_eq!(class(&g, "/admin/"), "admin_ui");
        assert_eq!(class(&g, "/web/depots/game"), "web_ui");
        assert_eq!(class(&g, "/metrics"), "metrics");
        assert_eq!(class(&g, "/favicon.ico"), "other");
    }

    #[test]
    fn renders_the_text_format() {
        let samples = vec![
            Sample::gauge("lode_depots", "Depots.", 2.0),
            Sample::gauge("lode_x", "X.", 1.5).label("depot", "a\"b\\c"),
            Sample::gauge("lode_x", "X.", 3.0).label("depot", "plain"),
        ];
        let text = render(&samples);
        assert_eq!(
            text,
            "# HELP lode_depots Depots.\n# TYPE lode_depots gauge\nlode_depots 2\n\
             # HELP lode_x X.\n# TYPE lode_x gauge\nlode_x{depot=\"a\\\"b\\\\c\"} 1.5\n\
             lode_x{depot=\"plain\"} 3\n"
        );
        assert_eq!(check_text_format(&text), Ok(3));
        assert!(check_text_format("lode_y 1\n").is_err());
        assert!(check_text_format("# TYPE lode_y gauge\nlode_y one\n").is_err());
    }

    #[test]
    fn networks_contain_addresses() {
        let net: crate::config::IpNet = "10.0.0.0/8".parse().unwrap_or_else(|e| panic!("{e}"));
        assert!(net.contains("10.1.2.3".parse().unwrap_or_else(|_| unreachable!())));
        assert!(!net.contains("11.0.0.1".parse().unwrap_or_else(|_| unreachable!())));
        assert!(net.contains("::ffff:10.0.0.1".parse().unwrap_or_else(|_| unreachable!())));
        let one: crate::config::IpNet = "::1".parse().unwrap_or_else(|e| panic!("{e}"));
        assert!(one.contains("::1".parse().unwrap_or_else(|_| unreachable!())));
        assert!(!one.contains("127.0.0.1".parse().unwrap_or_else(|_| unreachable!())));
        assert!("10.0.0.0/33".parse::<crate::config::IpNet>().is_err());
        assert!("nope".parse::<crate::config::IpNet>().is_err());
    }
}
