//! `loded doctor` and `lode doctor` (T178): a list of checks, each `ok`, `warn`, `fail` or
//! `skip`, with one line of detail. The JSON shape is documented in `docs/CLI-JSON.md`.

use serde::{Deserialize, Serialize};

/// How one check, or the whole report, came out. Ordered from best to worst, `skip` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    /// Not applicable here, such as certificate checks on a server without TLS.
    Skip,
    /// Fine.
    Ok,
    /// Works, but needs attention soon.
    Warn,
    /// Broken.
    Fail,
}

impl DoctorStatus {
    /// The status as the reports print it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// Short stable name, such as `certificate` or `backups`.
    pub name: String,
    /// The outcome.
    pub status: DoctorStatus,
    /// What was found, in one sentence; what to do when it is not `ok`.
    pub detail: String,
}

impl DoctorCheck {
    /// A check with its outcome.
    pub fn new(name: &str, status: DoctorStatus, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            status,
            detail: detail.into(),
        }
    }
}

/// Every check, and the worst outcome among them (`ok` when all were skipped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// The worst outcome.
    pub status: DoctorStatus,
    /// The checks in the order they ran.
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// A report of `checks`.
    pub fn new(checks: Vec<DoctorCheck>) -> Self {
        let status = checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(DoctorStatus::Ok)
            .max(DoctorStatus::Ok);
        Self { status, checks }
    }

    /// The process exit code: 0 when nothing is wrong, 1 on a failure, 2 on warnings only.
    pub fn exit_code(&self) -> u8 {
        match self.status {
            DoctorStatus::Skip | DoctorStatus::Ok => 0,
            DoctorStatus::Warn => 2,
            DoctorStatus::Fail => 1,
        }
    }

    /// The report as lines of text: `ok    name  detail`.
    pub fn lines(&self) -> Vec<String> {
        let width = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
        self.checks
            .iter()
            .map(|c| format!("{:<5} {:<width$}  {}", c.status.as_str(), c.name, c.detail))
            .collect()
    }
}

/// Days left until `expires` (Unix seconds) at `now`, and the check of a certificate with
/// that expiry: failed once expired, a warning with fewer than `warn_days` left.
pub fn certificate_check(name: &str, expires: i64, now: i64, warn_days: i64) -> DoctorCheck {
    let left = expires - now;
    let days = left.div_euclid(86_400);
    let when = time::OffsetDateTime::from_unix_timestamp(expires)
        .ok()
        .and_then(|t| {
            t.format(time::macros::format_description!("[year]-[month]-[day]"))
                .ok()
        })
        .unwrap_or_else(|| expires.to_string());
    if left <= 0 {
        DoctorCheck::new(
            name,
            DoctorStatus::Fail,
            format!("the certificate expired on {when}"),
        )
    } else if days < warn_days {
        DoctorCheck::new(
            name,
            DoctorStatus::Warn,
            format!("the certificate expires on {when}, in {days} days"),
        )
    } else {
        DoctorCheck::new(
            name,
            DoctorStatus::Ok,
            format!("valid until {when} ({days} days)"),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_worst_check_decides() {
        let ok = DoctorCheck::new("a", DoctorStatus::Ok, "");
        let skip = DoctorCheck::new("b", DoctorStatus::Skip, "");
        let warn = DoctorCheck::new("c", DoctorStatus::Warn, "");
        let fail = DoctorCheck::new("d", DoctorStatus::Fail, "");
        assert_eq!(DoctorReport::new(vec![skip.clone()]).exit_code(), 0);
        assert_eq!(
            DoctorReport::new(vec![ok.clone(), warn.clone()]).exit_code(),
            2
        );
        assert_eq!(DoctorReport::new(vec![warn, fail, ok]).exit_code(), 1);
        assert_eq!(
            serde_json::to_value(DoctorReport::new(vec![skip])).unwrap(),
            serde_json::json!({ "status": "ok", "checks": [
                { "name": "b", "status": "skip", "detail": "" } ] })
        );
    }

    #[test]
    fn certificates_by_days_left() {
        let now = 1_800_000_000;
        assert_eq!(
            certificate_check("c", now - 1, now, 14).status,
            DoctorStatus::Fail
        );
        assert_eq!(
            certificate_check("c", now + 3 * 86_400, now, 14).status,
            DoctorStatus::Warn
        );
        assert_eq!(
            certificate_check("c", now + 90 * 86_400, now, 14).status,
            DoctorStatus::Ok
        );
    }
}
