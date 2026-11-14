//! Times, durations and sizes as the admin pages show them, and directory sizes measured
//! now and then.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use time::macros::format_description;

use super::html::Html;
use crate::html;

/// A time in milliseconds as `2026-11-14 09:30 UTC`, in a `<time>` element.
pub(crate) fn when(ms: i64) -> Html {
    let t = crate::auth::ms_to_time(ms);
    let shown = t
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute] UTC"
        ))
        .unwrap_or_default();
    let iso = t
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    html!("<time datetime=\"{}\">{}</time>", iso, shown)
}

/// A timestamp from the API, like [`when`].
pub(crate) fn at(t: time::OffsetDateTime) -> Html {
    when(crate::auth::time_to_ms(t))
}

/// A count with its noun: `1 object`, `3 objects`.
pub(crate) fn count(n: impl Into<u64>, one: &str) -> String {
    let n = n.into();
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// A length of time such as `3 days 4 hours` or `12 minutes`.
pub(crate) fn span(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    let unit = |n: i64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    if d > 0 {
        if h > 0 {
            format!("{} {}", unit(d, "day"), unit(h, "hour"))
        } else {
            unit(d, "day")
        }
    } else if h > 0 {
        if m > 0 {
            format!("{} {}", unit(h, "hour"), unit(m, "minute"))
        } else {
            unit(h, "hour")
        }
    } else if m > 0 {
        unit(m, "minute")
    } else {
        unit(s, "second")
    }
}

/// A size such as `1.5 GiB`.
pub(crate) fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = UNITS[0];
    for u in &UNITS[1..] {
        if v < 1024.0 {
            break;
        }
        v /= 1024.0;
        unit = u;
    }
    if v < 10.0 {
        format!("{v:.1} {unit}")
    } else {
        format!("{v:.0} {unit}")
    }
}

/// How long a measured directory size is reused.
const SIZE_TTL_MS: i64 = 10 * 60 * 1000;

/// Directory sizes, measured at most every ten minutes, since a store can hold millions of
/// files.
#[derive(Default)]
pub(crate) struct SizeCache {
    sizes: Mutex<HashMap<PathBuf, (i64, u64)>>,
}

impl SizeCache {
    /// The total size of the files under `dir`, measured at most every ten minutes.
    pub(crate) async fn size(&self, dir: &Path, now: i64) -> Option<u64> {
        if let Some((at, size)) = self.lock().get(dir) {
            if now - at < SIZE_TTL_MS {
                return Some(*size);
            }
        }
        let owned = dir.to_owned();
        let size = tokio::task::spawn_blocking(move || dir_size(&owned))
            .await
            .ok()
            .flatten()?;
        self.lock().insert(dir.to_owned(), (now, size));
        Some(size)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, (i64, u64)>> {
        self.sizes.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The total size of the files under `dir`; `None` when it cannot be read.
fn dir_size(dir: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![dir.to_owned()];
    let mut first = true;
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) if !first => continue,
            Err(_) => return None,
        };
        first = false;
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_sizes() {
        assert_eq!(span(5_000), "5 seconds");
        assert_eq!(span(61_000), "1 minute");
        assert_eq!(span(2 * 3_600_000 + 60_000), "2 hours 1 minute");
        assert_eq!(span(86_400_000 * 3 + 3_600_000), "3 days 1 hour");
        assert_eq!(bytes(10), "10 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(300 * 1024 * 1024), "300 MiB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn times_are_utc() {
        assert_eq!(
            when(1_793_502_245_000).as_str(),
            "<time datetime=\"2026-11-01T03:04:05Z\">2026-11-01 03:04 UTC</time>"
        );
    }

    #[test]
    fn directory_sizes() {
        let dir = std::env::temp_dir().join(format!("lode-admin-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).ok();
        std::fs::write(dir.join("x"), [0u8; 10]).ok();
        std::fs::write(dir.join("a/b/y"), [0u8; 5]).ok();
        assert_eq!(dir_size(&dir), Some(15));
        assert_eq!(dir_size(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
