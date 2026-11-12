//! Local path ↔ depot path.

use std::path::{Component, Path, PathBuf};

use lode_core::DepotPath;

use crate::error::{Result, WorkspaceError};

/// The local path of `path` under `root`.
pub fn to_local(root: &Path, path: &DepotPath) -> PathBuf {
    let mut out = root.to_path_buf();
    for seg in path.segments() {
        out.push(seg);
    }
    out
}

/// The depot path of `local`, which is absolute or relative to `cwd`, inside `root`. `None`
/// for the root itself. A name that breaks SPEC §5.7 is [`WorkspaceError::InvalidName`] with
/// the rule it breaks.
pub fn to_depot(root: &Path, cwd: &Path, local: &Path) -> Result<Option<DepotPath>> {
    let joined = if local.is_absolute() {
        local.to_path_buf()
    } else {
        cwd.join(local)
    };
    let abs = normalize(&joined);
    let root = normalize(root);
    let rel = abs
        .strip_prefix(&root)
        .map_err(|_| WorkspaceError::OutsideWorkspace(local.to_path_buf()))?;
    let mut segs = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => match s.to_str() {
                Some(s) => segs.push(s.to_owned()),
                None => {
                    return Err(WorkspaceError::InvalidName {
                        local: local.to_path_buf(),
                        reason: "the name is not valid UTF-8".into(),
                    })
                }
            },
            _ => return Err(WorkspaceError::OutsideWorkspace(local.to_path_buf())),
        }
    }
    if segs.is_empty() {
        return Ok(None);
    }
    if segs[0] == crate::layout::DOT_LODE {
        return Err(WorkspaceError::InvalidName {
            local: local.to_path_buf(),
            reason: "`.lode` holds the workspace's own state".into(),
        });
    }
    DepotPath::parse(&segs.join("/"))
        .map(Some)
        .map_err(|e| WorkspaceError::InvalidName {
            local: local.to_path_buf(),
            reason: e.to_string(),
        })
}

/// Removes `.` and resolves `..` lexically, without touching the file system (symlinks in
/// the workspace are content, never followed).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn conversions() {
        let root = Path::new("/ws");
        let cwd = Path::new("/ws/src/ui");
        let d = |l: &str| to_depot(root, cwd, Path::new(l));
        assert_eq!(d("a.txt").unwrap().unwrap().as_str(), "src/ui/a.txt");
        assert_eq!(d("../b/c.txt").unwrap().unwrap().as_str(), "src/b/c.txt");
        assert_eq!(d("/ws/x").unwrap().unwrap().as_str(), "x");
        assert_eq!(d("../..").unwrap(), None);
        assert!(matches!(
            d("../../.."),
            Err(WorkspaceError::OutsideWorkspace(_))
        ));
        assert!(matches!(
            d("/elsewhere/x"),
            Err(WorkspaceError::OutsideWorkspace(_))
        ));
        let err = d("what?.txt").unwrap_err();
        assert!(err.to_string().contains("Windows"), "{err}");
        assert!(matches!(
            d("/ws/.lode/x"),
            Err(WorkspaceError::InvalidName { .. })
        ));
        assert_eq!(
            to_local(root, &DepotPath::parse("a/b.txt").unwrap()),
            Path::new("/ws").join("a").join("b.txt")
        );
    }
}
