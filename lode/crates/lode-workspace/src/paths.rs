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
/// the rule it breaks. Either slash works on Windows, where the root is also matched without
/// regard to letter case, as the file system does; the part below the root keeps the case
/// it was given.
pub fn to_depot(root: &Path, cwd: &Path, local: &Path) -> Result<Option<DepotPath>> {
    let joined = if local.is_absolute() {
        local.to_path_buf()
    } else {
        cwd.join(local)
    };
    let abs = normalize(&joined);
    let root = normalize(root);
    let rel = strip_root(&abs, &root, cfg!(windows))
        .ok_or_else(|| WorkspaceError::OutsideWorkspace(local.to_path_buf()))?;
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

/// `abs` below `root`, comparing components exactly or, with `ignore_case`, without regard
/// to letter case.
fn strip_root(abs: &Path, root: &Path, ignore_case: bool) -> Option<PathBuf> {
    if !ignore_case {
        return abs.strip_prefix(root).ok().map(Path::to_path_buf);
    }
    let mut rest = abs.components();
    for want in root.components() {
        let got = rest.next()?;
        let same = got.as_os_str() == want.as_os_str()
            || match (got.as_os_str().to_str(), want.as_os_str().to_str()) {
                (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
                _ => false,
            };
        if !same {
            return None;
        }
    }
    Some(rest.as_path().to_path_buf())
}

/// `path` without the Windows verbatim prefix that `canonicalize` adds (`\\?\C:\x` becomes
/// `C:\x`), so that it compares equal to paths typed by the user or returned by
/// `current_dir`. Unchanged elsewhere, and for verbatim forms with no plain spelling.
pub fn simplify(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        if let (Some(Component::Prefix(pre)), Some(text)) =
            (path.components().next(), path.to_str())
        {
            match pre.kind() {
                Prefix::VerbatimDisk(_) => return PathBuf::from(&text[4..]),
                Prefix::VerbatimUNC(..) => return PathBuf::from(format!(r"\\{}", &text[8..])),
                _ => {}
            }
        }
    }
    path
}

/// The canonical form of `path` (symlinks resolved), [`simplify`]-ed.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplify)
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

    #[test]
    fn root_matched_without_case_where_the_file_system_ignores_it() {
        let root = Path::new("/Dev/Game");
        let p = |l: &str, ic| strip_root(Path::new(l), root, ic);
        assert_eq!(
            p("/dev/GAME/Content/A.uasset", true),
            Some(PathBuf::from("Content/A.uasset"))
        );
        assert_eq!(p("/dev/GAME/Content/A.uasset", false), None);
        assert_eq!(p("/Dev/Game/x", false), Some(PathBuf::from("x")));
        assert_eq!(p("/Dev/Gamex/x", true), None);
        assert_eq!(p("/Dev", true), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_with_either_slash_and_any_case() {
        let root = Path::new(r"E:\DEV\Game");
        let cwd = Path::new(r"E:\DEV\Game");
        let d = |l: &str| to_depot(root, cwd, Path::new(l)).unwrap().unwrap();
        assert_eq!(
            d("E:/DEV/Game/Content/Hero.uasset").as_str(),
            "Content/Hero.uasset"
        );
        assert_eq!(
            d(r"e:\dev\game\Content\Hero.uasset").as_str(),
            "Content/Hero.uasset"
        );
        assert_eq!(
            d("e:/Dev/GAME/Content/Hero.uasset").as_str(),
            "Content/Hero.uasset"
        );
    }
}
