//! `lode doctor [URL]` (T178): what a client sees of a server, in one report. Each step
//! names what failed: the name does not resolve, the TLS handshake or certificate, `/info`,
//! the versions, the sign-in.

use std::time::Duration;

use lode_proto::doctor::{certificate_check, DoctorCheck, DoctorReport, DoctorStatus};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::OpResult;

/// With fewer days left the certificate check warns (as `loded doctor`).
pub const CERT_WARN_DAYS: i64 = 14;

/// Arguments of `lode doctor`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DoctorArgs {
    /// Server URL; the workspace's or the signed-in one when not given.
    pub url: Option<String>,
}

fn check(name: &str, status: DoctorStatus, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(name, status, detail)
}

/// An error with its causes, which carry the useful part of a connection failure (the
/// certificate problem, the refused port).
fn chain(e: &dyn std::error::Error) -> String {
    // The top level only says that sending failed; the causes say why.
    let (mut text, mut source) = match e.source() {
        Some(s) => (s.to_string(), s.source()),
        None => (e.to_string(), None),
    };
    while let Some(s) = source {
        let part = s.to_string();
        if !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        source = s.source();
    }
    text
}

/// Runs the checks against `url` or the configured server. The JSON is the report
/// (`{"status","checks"}`) with `"server"`; [`exit_code`] turns it into the exit code.
pub async fn doctor(ctx: &Ctx, args: DoctorArgs) -> OpResult<Value> {
    let server = match &args.url {
        Some(u) => ctx.parse_server(u)?,
        None => ctx.server_url(ctx.workspace().ok().as_ref())?,
    };
    let mut checks = Vec::new();
    let skip_rest = |checks: &mut Vec<DoctorCheck>, from: &[&str]| {
        for name in from {
            checks.push(check(name, DoctorStatus::Skip, "not checked"));
        }
    };

    // DNS.
    let host = server.host_str().unwrap_or_default().to_owned();
    let port = server.port_or_known_default().unwrap_or(443);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<std::net::IpAddr>().is_ok() {
        checks.push(check(
            "dns",
            DoctorStatus::Skip,
            format!("{host} is an address"),
        ));
    } else {
        let lookup = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::lookup_host((bare, port)),
        )
        .await;
        match lookup {
            Ok(Ok(addrs)) => {
                let mut shown: Vec<String> = addrs.map(|a| a.ip().to_string()).collect();
                shown.dedup();
                checks.push(check(
                    "dns",
                    DoctorStatus::Ok,
                    format!("{host} resolves to {}", shown.join(", ")),
                ));
            }
            Ok(Err(e)) => {
                checks.push(check(
                    "dns",
                    DoctorStatus::Fail,
                    format!("{host} does not resolve: {e}"),
                ));
                skip_rest(&mut checks, &["tls", "info", "versions", "identity"]);
                return Ok(report_json(&server, checks));
            }
            Err(_) => {
                checks.push(check(
                    "dns",
                    DoctorStatus::Fail,
                    format!("no DNS answer for {host} within 10 s"),
                ));
                skip_rest(&mut checks, &["tls", "info", "versions", "identity"]);
                return Ok(report_json(&server, checks));
            }
        }
    }

    // TLS and /info, in one request.
    let https = server.scheme() == "https";
    let anon = ctx.anonymous_client(&server)?;
    let (info, cert) = match anon.info_with_certificate().await {
        Ok(pair) => pair,
        Err(lode_client::Error::Transport(e)) => {
            let text = chain(&e);
            let tls_problem = https
                && [
                    "certificate",
                    "tls",
                    "handshake",
                    "UnknownIssuer",
                    "expired",
                ]
                .iter()
                .any(|w| text.to_lowercase().contains(&w.to_lowercase()));
            if tls_problem {
                checks.push(check(
                    "tls",
                    DoctorStatus::Fail,
                    format!("the TLS handshake with {host} failed: {text}"),
                ));
            } else {
                if https {
                    checks.push(check("tls", DoctorStatus::Skip, "no connection"));
                } else {
                    checks.push(check("tls", DoctorStatus::Skip, "plain http"));
                }
                checks.push(check(
                    "info",
                    DoctorStatus::Fail,
                    format!("cannot reach {server}: {text}"),
                ));
            }
            let rest: &[&str] = if tls_problem {
                &["info", "versions", "identity"]
            } else {
                &["versions", "identity"]
            };
            skip_rest(&mut checks, rest);
            return Ok(report_json(&server, checks));
        }
        Err(e) => {
            checks.push(check(
                "tls",
                if https {
                    DoctorStatus::Ok
                } else {
                    DoctorStatus::Skip
                },
                if https { "connected" } else { "plain http" },
            ));
            checks.push(check(
                "info",
                DoctorStatus::Fail,
                format!("{server}/api/v1/info does not answer as a Lode server: {e}"),
            ));
            skip_rest(&mut checks, &["versions", "identity"]);
            return Ok(report_json(&server, checks));
        }
    };
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    checks.push(match (https, cert) {
        (false, _) if is_loopback(bare) => {
            check("tls", DoctorStatus::Skip, "plain http to this machine")
        }
        (false, _) => check(
            "tls",
            DoctorStatus::Warn,
            "plain http: tokens and content travel unencrypted",
        ),
        (true, Some(der)) => match lode_proto::x509::validity(&der) {
            Some((_, not_after)) => certificate_check("tls", not_after, now, CERT_WARN_DAYS),
            None => check(
                "tls",
                DoctorStatus::Warn,
                "connected, but the certificate's validity cannot be read",
            ),
        },
        (true, None) => check("tls", DoctorStatus::Ok, "connected"),
    });
    checks.push(check(
        "info",
        DoctorStatus::Ok,
        format!("{server} answers as a Lode server"),
    ));

    // Versions.
    let client_version = lode_build::VERSION;
    let api = lode_proto::consts::API_VERSION;
    checks.push(if info.api_version != api {
        check(
            "versions",
            DoctorStatus::Fail,
            format!(
                "the server speaks API {} and this client {api}: update the older one",
                info.api_version
            ),
        )
    } else if !same_minor(&info.server_version, client_version) {
        check(
            "versions",
            DoctorStatus::Warn,
            format!(
                "server {}, this client {client_version}: they work together, but updating \
                 the older one is advised",
                info.server_version
            ),
        )
    } else {
        check(
            "versions",
            DoctorStatus::Ok,
            format!(
                "server {}, this client {client_version}",
                info.server_version
            ),
        )
    });

    // Identity.
    checks.push(match ctx.token(&server) {
        Ok(None) => check(
            "identity",
            DoctorStatus::Warn,
            format!("not signed in: run `lode login {server}`"),
        ),
        Err(e) => check("identity", DoctorStatus::Fail, e.message),
        Ok(Some(token)) => match anon.with_token(token).whoami().await {
            Ok(me) => check(
                "identity",
                DoctorStatus::Ok,
                format!(
                    "signed in as {}; scopes: {}",
                    me.principal.name,
                    me.scopes.join(", ")
                ),
            ),
            Err(lode_client::Error::Api(e)) if e.status == 401 => check(
                "identity",
                DoctorStatus::Fail,
                format!("the stored token is not accepted: run `lode login {server}`"),
            ),
            Err(e) => check("identity", DoctorStatus::Fail, e.to_string()),
        },
    });
    Ok(report_json(&server, checks))
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Whether two versions share major and minor (`0.3.1` and `0.3.4+abc`).
fn same_minor(a: &str, b: &str) -> bool {
    let key = |v: &str| {
        let mut parts = v.trim_start_matches('v').split(['.', '-', '+']);
        (
            parts.next().unwrap_or_default().to_owned(),
            parts.next().unwrap_or_default().to_owned(),
        )
    };
    key(a) == key(b)
}

fn report_json(server: &lode_client::ServerUrl, checks: Vec<DoctorCheck>) -> Value {
    let report = DoctorReport::new(checks);
    let mut v = serde_json::to_value(&report).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("server".into(), json!(server.as_str()));
    }
    v
}

/// The report's text lines, for the terminal.
pub fn lines(v: &Value) -> Vec<String> {
    serde_json::from_value::<DoctorReport>(v.clone())
        .map(|r| r.lines())
        .unwrap_or_default()
}

/// 0 when all is well, 2 on warnings only, 1 on a failure (as `loded doctor`).
pub fn exit_code(v: &Value) -> u8 {
    serde_json::from_value::<DoctorReport>(v.clone()).map_or(1, |r| r.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_minor() {
        assert!(same_minor("0.3.1", "0.3.4+abc"));
        assert!(same_minor("v1.2.0", "1.2.9-rc1"));
        assert!(!same_minor("0.2.0", "0.3.0"));
    }
}
