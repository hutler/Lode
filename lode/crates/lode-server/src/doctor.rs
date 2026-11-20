//! `loded doctor` (T178): the checks an operator would otherwise make by hand on the server,
//! in one report. It reads; it never fixes anything.
//!
//! Each check is a function of what it looks at, so tests can drive the outcomes (an expired
//! certificate, a missing backup, a full disk) without arranging the machine.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lode_meta::Reads;
use lode_proto::doctor::{certificate_check, DoctorCheck, DoctorReport, DoctorStatus};

use crate::config::{ServerConfig, TlsConfig};

/// Below this share of free space the disk check warns.
pub const DISK_WARN_PERCENT: u64 = 10;
/// With fewer days left the certificate check warns. ACME renews with a third of the
/// lifetime left (about 30 days), so a certificate this close to expiry is not renewing.
pub const CERT_WARN_DAYS: i64 = 14;

fn ok(name: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(name, DoctorStatus::Ok, detail)
}
fn warn(name: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(name, DoctorStatus::Warn, detail)
}
fn fail(name: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(name, DoctorStatus::Fail, detail)
}
fn skip(name: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(name, DoctorStatus::Skip, detail)
}

/// Runs every check against the configuration at `config_path`, as of `now` (Unix seconds).
pub async fn run(config_path: &Path, now: i64) -> DoctorReport {
    let mut checks = Vec::new();
    let cfg = match ServerConfig::load(config_path) {
        Ok(c) => {
            checks.push(ok("config", format!("{} parses", config_path.display())));
            c
        }
        Err(e) => {
            checks.push(fail("config", e.to_string()));
            return DoctorReport::new(checks);
        }
    };
    checks.push(data_dir(&cfg.data_dir));
    checks.push(match disk_space(&cfg.data_dir) {
        Ok((free, total)) => disk_check(free, total),
        Err(e) => warn("disk", format!("cannot read the free space: {e}")),
    });
    checks.push(certificate(&cfg, now).await);
    checks.push(domain(&cfg).await);
    checks.push(backups(&cfg, now).await);
    checks.extend(database_and_store(cfg).await);
    DoctorReport::new(checks)
}

/// The data directory exists and a file can be written in it.
fn data_dir(dir: &Path) -> DoctorCheck {
    // `data_dir = "."` beside loded.toml shows as `data/.`; drop the `.` parts.
    let shown: std::path::PathBuf = dir
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    let shown = shown.display();
    if !dir.is_dir() {
        return fail(
            "data_dir",
            format!("{shown} does not exist: run `loded init` or fix data_dir"),
        );
    }
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    match std::fs::write(&probe, b"lode doctor") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            ok("data_dir", format!("{shown} is writable"))
        }
        Err(e) => fail(
            "data_dir",
            format!("cannot write in {shown}: {e}; the server's user must own it"),
        ),
    }
}

/// Free and total bytes of the disk holding `dir`.
fn disk_space(dir: &Path) -> std::io::Result<(u64, u64)> {
    Ok((fs4::available_space(dir)?, fs4::total_space(dir)?))
}

/// The disk check for `free` of `total` bytes: a warning below [`DISK_WARN_PERCENT`], a
/// failure when nothing is left.
pub fn disk_check(free: u64, total: u64) -> DoctorCheck {
    let gib = |b: u64| b as f64 / f64::from(1u32 << 30);
    if total == 0 {
        return warn("disk", "the disk reports no size");
    }
    let percent = free.saturating_mul(100) / total;
    let text = format!(
        "{:.1} GiB free of {:.1} GiB ({percent}%)",
        gib(free),
        gib(total)
    );
    if free == 0 {
        fail(
            "disk",
            format!("{text}: the disk is full, commits and backups fail"),
        )
    } else if percent < DISK_WARN_PERCENT {
        warn(
            "disk",
            format!("{text}: below {DISK_WARN_PERCENT}%, make room or run `lode admin gc`"),
        )
    } else {
        ok("disk", text)
    }
}

/// The certificate the server presents, from its file or the ACME cache.
async fn certificate(cfg: &ServerConfig, now: i64) -> DoctorCheck {
    let (path, acme) = match &cfg.tls {
        None => {
            return skip(
                "certificate",
                "no [tls]: the server speaks plain HTTP (a proxy in front may add TLS)",
            )
        }
        Some(TlsConfig::Files { cert, .. }) => (cert.clone(), false),
        Some(TlsConfig::Acme(a)) => (crate::acme::cached_cert_path(a, &cfg.data_dir), true),
    };
    let pem = match tokio::fs::read(&path).await {
        Ok(p) => p,
        Err(_) if acme => {
            return warn(
                "certificate",
                "no certificate obtained yet; the server asks for one when it starts, see its \
                 log if this stays so",
            )
        }
        Err(e) => {
            return fail(
                "certificate",
                format!("cannot read {}: {e}", path.display()),
            )
        }
    };
    match crate::acme::pem_expiry(&pem) {
        Some(expires) => certificate_check("certificate", expires, now, CERT_WARN_DAYS),
        None => fail(
            "certificate",
            format!("{} holds no certificate that can be read", path.display()),
        ),
    }
}

/// Whether `ip` is an address of this machine: binding a socket to it succeeds only then.
fn is_local(ip: IpAddr) -> bool {
    std::net::UdpSocket::bind((ip, 0)).is_ok()
}

/// The ACME domain resolves, to an address of this machine.
async fn domain(cfg: &ServerConfig) -> DoctorCheck {
    let Some(TlsConfig::Acme(a)) = &cfg.tls else {
        return skip("domain", "no automatic certificate, so no domain to check");
    };
    let lookup = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((a.domain.as_str(), 443)),
    )
    .await;
    let addrs: Vec<IpAddr> = match lookup {
        Ok(Ok(it)) => it.map(|s| s.ip()).collect(),
        Ok(Err(e)) => {
            return fail(
                "domain",
                format!(
                    "{} does not resolve ({e}); point its DNS record at this server, or use a \
                     name like <ip>.sslip.io",
                    a.domain
                ),
            )
        }
        Err(_) => {
            return fail(
                "domain",
                format!("no DNS answer for {} within 10 s", a.domain),
            )
        }
    };
    let shown = addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if addrs.iter().any(|ip| is_local(*ip)) {
        ok(
            "domain",
            format!("{} resolves to {shown}, this machine", a.domain),
        )
    } else {
        warn(
            "domain",
            format!(
                "{} resolves to {shown}, which is not an address of this machine: fine if a \
                 router forwards port 443 here, otherwise certificates cannot be obtained",
                a.domain
            ),
        )
    }
}

/// The newest complete backup, against the schedule.
async fn backups(cfg: &ServerConfig, now: i64) -> DoctorCheck {
    let Some(schedule) = cfg.backup.schedule() else {
        return warn(
            "backups",
            "scheduled backups are off: set `every` under [backup], or run `loded backup` \
             from a timer",
        );
    };
    let dir = schedule.dir.clone();
    let newest = tokio::task::spawn_blocking(move || crate::maintenance::newest_backup(&dir))
        .await
        .ok()
        .flatten();
    backup_check(
        newest.map(|(t, _)| t.unix_timestamp()),
        schedule.every,
        &schedule.dir,
        now,
    )
}

/// The backup check: the newest complete backup was made at `newest` (Unix seconds) with one
/// due every `every`. Missing is a warning (a new server has none yet); more than two
/// intervals old is a failure (backups have stopped).
pub fn backup_check(newest: Option<i64>, every: Duration, dir: &Path, now: i64) -> DoctorCheck {
    let every_s = i64::try_from(every.as_secs()).unwrap_or(i64::MAX).max(1);
    let Some(at) = newest else {
        return warn(
            "backups",
            format!(
                "no complete backup in {} yet; the first is due within {}",
                dir.display(),
                hours(every_s)
            ),
        );
    };
    let age = (now - at).max(0);
    let text = format!(
        "the newest backup is {} old (one every {})",
        hours(age),
        hours(every_s)
    );
    if age > every_s.saturating_mul(2) {
        fail(
            "backups",
            format!("{text}: backups have stopped, see the server log"),
        )
    } else {
        ok("backups", format!("{text}, in {}", dir.display()))
    }
}

fn hours(seconds: i64) -> String {
    if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else if seconds < 2 * 86_400 {
        format!("{} h", seconds / 3600)
    } else {
        format!("{} days", seconds / 86_400)
    }
}

/// The database opens and answers, and the store answers for a depot.
async fn database_and_store(cfg: ServerConfig) -> Vec<DoctorCheck> {
    let kind = match &cfg.database.kind {
        crate::config::DatabaseKind::Sqlite => "SQLite (meta.db)",
        _ => "Postgres",
    };
    let s3 = cfg.storage.s3_in_use().is_some();
    let state = match crate::open_state(cfg, Arc::new(crate::SystemClock), Vec::new()).await {
        Ok(s) => s,
        Err(e) => {
            return vec![
                fail("database", format!("cannot open the {kind} database: {e}")),
                skip("store", "not checked without the database"),
            ]
        }
    };
    let depots = match state.meta().read(|tx| tx.depots()).await {
        Ok(d) => d,
        Err(e) => {
            return vec![
                fail(
                    "database",
                    format!("the {kind} database does not answer: {e}"),
                ),
                skip("store", "not checked without the database"),
            ]
        }
    };
    let mut out = vec![ok(
        "database",
        format!("{kind} answers, {} depot(s)", depots.len()),
    )];
    let where_ = if s3 {
        "the S3 bucket"
    } else {
        "the depots folder"
    };
    out.push(match depots.first() {
        None => skip("store", "no depots yet"),
        Some(d) => {
            let probe = lode_core::blob_id(&b""[..]);
            match probe {
                Ok(id) => match state.blobs().contains(d.id, &id).await {
                    Ok(_) => ok("store", format!("{where_} answers")),
                    Err(e) => fail("store", format!("{where_} does not answer: {e}")),
                },
                Err(_) => skip("store", "not checked"),
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_disk_warns_then_fails() {
        let gib = 1u64 << 30;
        assert_eq!(disk_check(50 * gib, 100 * gib).status, DoctorStatus::Ok);
        let low = disk_check(5 * gib, 100 * gib);
        assert_eq!(low.status, DoctorStatus::Warn);
        assert!(low.detail.contains("(5%)"), "{}", low.detail);
        assert_eq!(disk_check(0, 100 * gib).status, DoctorStatus::Fail);
    }

    #[test]
    fn backups_by_age() {
        let day = Duration::from_secs(86_400);
        let dir = Path::new("/b");
        let now = 1_800_000_000;
        assert_eq!(backup_check(None, day, dir, now).status, DoctorStatus::Warn);
        assert_eq!(
            backup_check(Some(now - 3600), day, dir, now).status,
            DoctorStatus::Ok
        );
        assert_eq!(
            backup_check(Some(now - 3 * 86_400), day, dir, now).status,
            DoctorStatus::Fail
        );
    }

    #[test]
    fn loopback_is_local() {
        assert!(is_local(
            "127.0.0.1".parse().unwrap_or(IpAddr::from([127, 0, 0, 1]))
        ));
        assert!(!is_local(IpAddr::from([192, 0, 2, 1])));
    }
}
