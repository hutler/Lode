//! Directory sizes for the admin pages and `GET /depots` (T179), measured at most every ten
//! minutes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lode_core::DepotId;

use crate::state::AppState;

/// How long a measured directory size is reused.
const SIZE_TTL_MS: i64 = 10 * 60 * 1000;

/// Directory sizes, measured at most every ten minutes, since a store can hold millions of
/// files.
#[derive(Default)]
pub struct SizeCache {
    sizes: Mutex<HashMap<PathBuf, (i64, u64)>>,
}

impl SizeCache {
    /// The total size of the files under `dir`, measured at most every ten minutes.
    pub async fn size(&self, dir: &Path, now: i64) -> Option<u64> {
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

/// The size of a depot's stored content, when it is kept in files (`None` with S3 storage).
pub(crate) async fn depot_size(state: &AppState, id: DepotId) -> Option<u64> {
    if state.config().storage.s3_in_use().is_some() {
        return None;
    }
    let dir = state
        .config()
        .data_dir
        .join(crate::server::DEPOTS_DIR)
        .join(id.0.to_string());
    state.sizes().size(&dir, state.clock().now_ms()).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
