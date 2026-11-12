//! File changes, shared by commits and shelves (ARCHITECTURE §5.1, §9.2).

use std::collections::{HashMap, HashSet};

use crate::id::ObjectId;
use crate::path::{case_fold, DepotPath};
use crate::tree::EntryKind;

/// What a change does to its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeAction {
    /// Create a path that is absent at the head.
    Add,
    /// Replace content whose head id equals `base_id`.
    Modify,
    /// Remove a path whose head id equals `base_id`.
    Delete,
}

impl ChangeAction {
    /// Single-letter form used in the history index (`A`, `M`, `D`).
    pub fn letter(self) -> char {
        match self {
            ChangeAction::Add => 'A',
            ChangeAction::Modify => 'M',
            ChangeAction::Delete => 'D',
        }
    }
}

/// The source of a copy or rename: a path at a revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CopyFrom {
    /// Source path.
    pub path: DepotPath,
    /// Source revision number.
    pub rev: u64,
}

/// One file change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Change {
    /// The path changed.
    pub path: DepotPath,
    /// What happens to it.
    pub action: ChangeAction,
    /// The id the author had for this path; required for modify and delete, absent for add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_id: Option<ObjectId>,
    /// The new content; required for add and modify, absent for delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_id: Option<ObjectId>,
    /// `file` or `symlink`; required for add and modify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntryKind>,
    /// Executable bit; files only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub executable: bool,
    /// Length of the new content; required for add and modify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Copy or rename origin; add only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_from: Option<CopyFrom>,
}

/// Why a change set is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeSetError {
    /// The same path appears twice.
    #[error("path {0} appears more than once")]
    Duplicate(DepotPath),
    /// One path is a directory prefix of another.
    #[error("{file} is both a file and the directory of {nested}")]
    FileDirConflict {
        /// The shorter path.
        file: DepotPath,
        /// A path under it.
        nested: DepotPath,
    },
    /// Two paths, or their directories, differ only in letter case.
    #[error("{0} and {1} differ only in case")]
    CaseCollision(String, String),
    /// A single change is inconsistent with its action.
    #[error("change to {path}: {reason}")]
    Invalid {
        /// The change's path.
        path: DepotPath,
        /// What is wrong.
        reason: &'static str,
    },
}

impl Change {
    /// Checks the fields required or forbidden by the action.
    pub fn validate(&self) -> Result<(), ChangeSetError> {
        let bad = |reason| {
            Err(ChangeSetError::Invalid {
                path: self.path.clone(),
                reason,
            })
        };
        match self.action {
            ChangeAction::Add | ChangeAction::Modify => {
                if self.new_id.is_none() {
                    return bad("new_id is required");
                }
                if self.size.is_none() {
                    return bad("size is required");
                }
                match self.kind {
                    None => return bad("kind is required"),
                    Some(EntryKind::Dir) => return bad("kind must be file or symlink"),
                    Some(EntryKind::Symlink) if self.executable => {
                        return bad("a symlink cannot be executable")
                    }
                    _ => {}
                }
            }
            ChangeAction::Delete => {
                if self.new_id.is_some() || self.size.is_some() || self.executable {
                    return bad("a delete carries no new content");
                }
            }
        }
        match self.action {
            ChangeAction::Add if self.base_id.is_some() => bad("an add has no base_id"),
            ChangeAction::Modify | ChangeAction::Delete if self.base_id.is_none() => {
                bad("base_id is required")
            }
            ChangeAction::Modify | ChangeAction::Delete if self.copy_from.is_some() => {
                bad("copy_from is only allowed on an add")
            }
            _ => Ok(()),
        }
    }
}

/// Validates each change and the set as a whole: no duplicate paths, no path that is both a
/// file and a directory prefix of another, and no two paths (or their directories) that
/// differ only in case.
pub fn validate_changes(changes: &[Change]) -> Result<(), ChangeSetError> {
    let mut paths: HashSet<&str> = HashSet::with_capacity(changes.len());
    for c in changes {
        c.validate()?;
        if !paths.insert(c.path.as_str()) {
            return Err(ChangeSetError::Duplicate(c.path.clone()));
        }
    }
    let mut folded: HashMap<String, &str> = HashMap::with_capacity(changes.len() * 2);
    for c in changes {
        for anc in c.path.ancestors() {
            if paths.contains(anc) {
                return Err(ChangeSetError::FileDirConflict {
                    file: DepotPath::from_valid_ancestor(anc),
                    nested: c.path.clone(),
                });
            }
        }
        for name in std::iter::once(c.path.as_str()).chain(c.path.ancestors()) {
            if let Some(prev) = folded.insert(case_fold(name), name) {
                if prev != name {
                    return Err(ChangeSetError::CaseCollision(
                        prev.to_owned(),
                        name.to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> ObjectId {
        ObjectId::from_bytes([b; 32])
    }

    fn add(p: &str) -> Change {
        Change {
            path: DepotPath::parse(p).unwrap(),
            action: ChangeAction::Add,
            base_id: None,
            new_id: Some(id(1)),
            kind: Some(EntryKind::File),
            executable: false,
            size: Some(1),
            copy_from: None,
        }
    }

    fn delete(p: &str) -> Change {
        Change {
            path: DepotPath::parse(p).unwrap(),
            action: ChangeAction::Delete,
            base_id: Some(id(2)),
            new_id: None,
            kind: None,
            executable: false,
            size: None,
            copy_from: None,
        }
    }

    #[test]
    fn accepts_a_normal_set() {
        let mut rename = add("a/new.png");
        rename.copy_from = Some(CopyFrom {
            path: DepotPath::parse("a/old.png").unwrap(),
            rev: 41,
        });
        assert_eq!(
            validate_changes(&[add("a/b.txt"), rename, delete("a/old.png")]),
            Ok(())
        );
    }

    #[test]
    fn rejects_duplicates() {
        assert!(matches!(
            validate_changes(&[add("x"), delete("x")]),
            Err(ChangeSetError::Duplicate(_))
        ));
    }

    #[test]
    fn rejects_file_and_directory_prefix() {
        let err = validate_changes(&[delete("a"), add("a/b/c")]).unwrap_err();
        assert!(
            matches!(err, ChangeSetError::FileDirConflict { .. }),
            "{err}"
        );
        let err = validate_changes(&[add("a/b/c"), delete("a/b")]).unwrap_err();
        assert!(
            matches!(err, ChangeSetError::FileDirConflict { .. }),
            "{err}"
        );
    }

    #[test]
    fn rejects_case_collisions() {
        assert!(matches!(
            validate_changes(&[add("README"), add("readme")]),
            Err(ChangeSetError::CaseCollision(..))
        ));
        assert!(matches!(
            validate_changes(&[add("Dir/a"), add("dir/b")]),
            Err(ChangeSetError::CaseCollision(..))
        ));
        assert_eq!(validate_changes(&[add("dir/a"), add("dir/b")]), Ok(()));
    }

    #[test]
    fn rejects_inconsistent_fields() {
        let mut c = add("x");
        c.base_id = Some(id(3));
        assert!(validate_changes(&[c]).is_err());
        let mut c = add("x");
        c.kind = Some(EntryKind::Dir);
        assert!(validate_changes(&[c]).is_err());
        let mut c = delete("x");
        c.base_id = None;
        assert!(validate_changes(&[c]).is_err());
        let mut c = delete("x");
        c.new_id = Some(id(1));
        assert!(validate_changes(&[c]).is_err());
        let mut c = delete("x");
        c.copy_from = Some(CopyFrom {
            path: DepotPath::parse("y").unwrap(),
            rev: 1,
        });
        assert!(validate_changes(&[c]).is_err());
    }

    #[test]
    fn wire_form() {
        let json = serde_json::to_value(add("a/b.txt")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "path": "a/b.txt", "action": "add", "new_id": id(1).to_hex(),
                "kind": "file", "size": 1
            })
        );
    }
}
