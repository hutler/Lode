//! Watches a workspace folder and reports a burst of changes as one call, so the window
//! refreshes its status once per burst rather than once per file.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// How long the folder must be quiet before a burst is reported.
pub const QUIET: Duration = Duration::from_millis(300);

/// A running watch; dropping it stops the watch.
pub struct Watcher {
    _inner: RecommendedWatcher,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watcher")
    }
}

/// Whether an event path is the workspace's own bookkeeping (`.lode/`), which changes on
/// every operation and is not a change the user made.
fn is_internal(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .is_some_and(|c| c.as_os_str().eq_ignore_ascii_case(".lode"))
}

impl Watcher {
    /// Watches `root` recursively and calls `on_change` once after each burst of changes,
    /// when nothing has changed for `quiet`.
    pub fn new(
        root: &Path,
        quiet: Duration,
        on_change: impl Fn() + Send + 'static,
    ) -> notify::Result<Self> {
        let (tx, rx) = channel::<PathBuf>();
        let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Reading a file (as `lode status` does) is not a change.
            if let Some(event) = res.ok().filter(|e| !e.kind.is_access()) {
                for p in event.paths {
                    let _ = tx.send(p);
                }
            }
        })?;
        inner.watch(root, RecursiveMode::Recursive)?;
        // Paths arrive as the OS reports them: on Windows the plain form of the root given,
        // elsewhere sometimes the canonical one (a symlinked temp dir on macOS). Compare against
        // both; canonicalizing first would turn both into `\\?\C:\…` on Windows.
        let raw_root = root.to_path_buf();
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        std::thread::Builder::new()
            .name("lode-watch".into())
            .spawn(move || {
                let relevant = |p: &Path| !is_internal(&root, p) && !is_internal(&raw_root, p);
                // Ends when the watcher, and with it the sender, is dropped.
                while let Ok(first) = rx.recv() {
                    if !relevant(&first) {
                        continue;
                    }
                    loop {
                        match rx.recv_timeout(quiet) {
                            Ok(_) => continue,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    on_change();
                }
            })
            .map_err(notify::Error::io)?;
        Ok(Self { _inner: inner })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    fn wait_for(count: &AtomicUsize, n: usize) {
        let until = Instant::now() + Duration::from_secs(10);
        while count.load(Ordering::SeqCst) < n && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_burst_of_changes_is_one_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        // The quiet time is long enough that a slow CI machine (Windows runners have paused
        // for over 200 ms between two of these writes) still sees the writes as one burst.
        let _w = Watcher::new(dir.path(), Duration::from_millis(1000), move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        for i in 0..50 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        wait_for(&count, 1);
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // A later change is a second burst.
        std::fs::write(dir.path().join("f0.txt"), "y").unwrap();
        wait_for(&count, 2);
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn workspace_bookkeeping_is_not_a_change() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".lode")).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let _w = Watcher::new(dir.path(), Duration::from_millis(100), move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(".lode").join(format!("db{i}")), "x").unwrap();
        }
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
