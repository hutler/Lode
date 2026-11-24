//! A manual backup can run in a different process from the service deleting objects.
//! The file stays in place so all callers always lock the same inode.

use std::fs::{File, OpenOptions};
use std::{io, path::Path};

use fs4::fs_std::FileExt;
use lode_core::ErrorCode;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) fn for_backup(data: &Path) -> io::Result<File> {
    let file = open(data)?;
    if FileExt::try_lock_exclusive(&file)? {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another backup or object removal is running; retry the backup",
        ))
    }
}

/// Deletions can overlap under their normal GC/depot rules, but never during a local backup.
pub(crate) fn for_removal(state: &AppState) -> ApiResult<Option<File>> {
    if state.config().storage.s3_in_use().is_some() {
        return Ok(None);
    }
    let failure = |e: io::Error| {
        tracing::error!(error = %e, "cannot lock object store for removal");
        ApiError::internal()
    };
    let file = open(&state.config().data_dir).map_err(failure)?;
    if FileExt::try_lock_shared(&file).map_err(failure)? {
        Ok(Some(file))
    } else {
        Err(ApiError::new(
            ErrorCode::Conflict,
            "a filesystem backup is running; retry after it finishes",
        ))
    }
}

fn open(data: &Path) -> io::Result<File> {
    let path = data.join("backup.lock");
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .or_else(|error| {
            // A previous CLI backup may have created the marker as a different user.
            if error.kind() == io::ErrorKind::PermissionDenied {
                File::open(path)
            } else {
                Err(error)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_retries_after_an_existing_deletion_releases_its_lock() {
        let data = tempfile::tempdir().unwrap();
        let remover = open(data.path()).unwrap();
        assert!(FileExt::try_lock_shared(&remover).unwrap());
        assert_eq!(
            for_backup(data.path()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(remover);
        let _backup = for_backup(data.path()).unwrap();
        assert_eq!(
            for_backup(data.path()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
