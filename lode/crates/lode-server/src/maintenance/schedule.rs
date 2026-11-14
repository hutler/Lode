//! Scheduled backups (T082): `[backup] every`, `keep` and `dir`.
//!
//! Each backup goes to `<dir>/lode-backup-<UTC time>`, written first under a `.partial` name
//! and renamed when complete, so an interrupted backup never counts. The time of the newest
//! complete backup decides when the next one is due, so the schedule survives restarts.
//! After each backup, complete backups beyond `keep` are removed, oldest first; nothing else
//! in the directory is touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

use super::backup::{backup, BackupError, BackupReport};
use crate::module::Stop;
use crate::state::AppState;

/// Name prefix of scheduled backup directories.
pub const BACKUP_PREFIX: &str = "lode-backup-";
const PARTIAL: &str = ".partial";

/// When and where scheduled backups go, from `[backup]`.
#[derive(Debug, Clone)]
pub struct BackupSchedule {
    /// Time between backups.
    pub every: Duration,
    /// Complete backups to keep.
    pub keep: usize,
    /// Directory holding the backups.
    pub dir: PathBuf,
}

/// What one check did.
#[derive(Debug)]
pub enum BackupTick {
    /// The newest backup is recent; the next is due after this long.
    NotDue(Duration),
    /// A backup was made.
    Done {
        /// The new backup.
        path: PathBuf,
        /// What it copied.
        report: BackupReport,
        /// Old backups removed.
        removed: Vec<PathBuf>,
    },
}

fn io_err(context: String) -> impl FnOnce(std::io::Error) -> BackupError {
    move |source| BackupError::Io { context, source }
}

fn backup_time(name: &str) -> Option<OffsetDateTime> {
    let stamp = name.strip_prefix(BACKUP_PREFIX)?;
    let format = format_description!("[year][month][day]T[hour][minute][second]Z");
    PrimitiveDateTime::parse(stamp, format)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
}

fn backup_name(at: OffsetDateTime) -> String {
    let t = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{BACKUP_PREFIX}{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Complete backups with their times, oldest first.
type Complete = Vec<(OffsetDateTime, PathBuf)>;

/// Complete backups in `dir`, and leftover partial ones.
fn list(dir: &Path) -> Result<(Complete, Vec<PathBuf>), BackupError> {
    let mut complete = Vec::new();
    let mut partial = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((complete, partial)),
        Err(e) => return Err(io_err(format!("cannot read {}", dir.display()))(e)),
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if let Some(base) = name.strip_suffix(PARTIAL) {
            if backup_time(base).is_some() {
                partial.push(entry.path());
            }
        } else if let Some(at) = backup_time(&name) {
            complete.push((at, entry.path()));
        }
    }
    complete.sort();
    Ok((complete, partial))
}

/// The newest complete backup in `dir`, with its time.
pub fn newest_backup(dir: &Path) -> Option<(OffsetDateTime, PathBuf)> {
    list(dir)
        .ok()
        .and_then(|(complete, _)| complete.into_iter().last())
}

/// Makes a backup if one is due by the server clock, then removes backups beyond `keep`.
pub async fn backup_tick(
    state: &AppState,
    schedule: &BackupSchedule,
) -> Result<BackupTick, BackupError> {
    let now = state.clock().now();
    let (complete, partial) = list(&schedule.dir)?;
    for p in partial {
        // An interrupted earlier backup.
        let _ = std::fs::remove_dir_all(&p);
    }
    if let Some((last, _)) = complete.last() {
        let since = now - *last;
        let every = time::Duration::try_from(schedule.every).unwrap_or(time::Duration::MAX);
        if since < every {
            let left = Duration::try_from(every - since).unwrap_or(schedule.every);
            return Ok(BackupTick::NotDue(left));
        }
    }
    let name = backup_name(now);
    let path = schedule.dir.join(&name);
    if path.exists() {
        // Two checks within one second of a clock that went backwards.
        return Ok(BackupTick::NotDue(Duration::from_secs(1)));
    }
    let partial = schedule.dir.join(format!("{name}{PARTIAL}"));
    let report = match backup(state, &partial).await {
        Ok(report) => report,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(e);
        }
    };
    std::fs::rename(&partial, &path)
        .map_err(io_err(format!("cannot rename {}", partial.display())))?;
    let (complete, _) = list(&schedule.dir)?;
    let mut removed = Vec::new();
    let extra = complete.len().saturating_sub(schedule.keep.max(1));
    for (_, old) in complete.into_iter().take(extra) {
        std::fs::remove_dir_all(&old)
            .map_err(io_err(format!("cannot remove {}", old.display())))?;
        removed.push(old);
    }
    Ok(BackupTick::Done {
        path,
        report,
        removed,
    })
}

/// Runs scheduled backups until `stop` fires. Checks at least once an hour, so a changed
/// clock is noticed; a failed backup is retried after an hour or `every`, whichever is
/// shorter.
pub async fn run_backups(state: AppState, schedule: BackupSchedule, mut stop: Stop) {
    const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
    loop {
        let wait = match backup_tick(&state, &schedule).await {
            Ok(BackupTick::NotDue(left)) => left,
            Ok(BackupTick::Done {
                path,
                report,
                removed,
            }) => {
                state.record_backup(crate::state::BackupRun {
                    at: state.clock().now_ms(),
                    outcome: Ok(report.bytes),
                });
                tracing::info!(
                    path = %path.display(),
                    objects = report.objects,
                    bytes = report.bytes,
                    content_in_bucket = report.content_in_bucket,
                    removed = removed.len(),
                    "scheduled backup done"
                );
                schedule.every
            }
            Err(e) => {
                state.record_backup(crate::state::BackupRun {
                    at: state.clock().now_ms(),
                    outcome: Err(e.to_string()),
                });
                tracing::error!(error = %e, dir = %schedule.dir.display(), "scheduled backup failed");
                schedule.every
            }
        };
        let wait = wait.clamp(Duration::from_secs(1), MAX_WAIT);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = stop.wait() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn names_round_trip() {
        let at = OffsetDateTime::from_unix_timestamp(1_793_502_245).unwrap();
        let name = backup_name(at);
        assert_eq!(name, "lode-backup-20261101T030405Z");
        assert_eq!(backup_time(&name), Some(at));
        assert!(backup_time("lode-backup-yesterday").is_none());
        assert!(backup_time("other-20261101T030405Z").is_none());
    }
}
