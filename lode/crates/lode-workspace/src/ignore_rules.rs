//! `.lodeignore` rules: gitignore syntax, one file per directory, deeper files first.

use std::path::Path;
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::layout::IGNORE_FILE;

/// The ignore files that apply in one directory: its own and its ancestors'.
#[derive(Debug, Default)]
pub struct IgnoreStack {
    parent: Option<Arc<IgnoreStack>>,
    rules: Option<Gitignore>,
}

impl IgnoreStack {
    /// No rules.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The stack for directory `dir`, below `parent`: adds `dir/.lodeignore` when it exists.
    /// Problems with the file (an unreadable file, a bad pattern) are returned as text; the
    /// valid patterns still apply.
    pub fn enter(
        parent: &Arc<Self>,
        dir: &Path,
        has_ignore_file: bool,
    ) -> (Arc<Self>, Option<String>) {
        if !has_ignore_file {
            return (parent.clone(), None);
        }
        let file = dir.join(IGNORE_FILE);
        let mut builder = GitignoreBuilder::new(dir);
        let mut problem = builder
            .add(&file)
            .map(|e| format!("{}: {e}", file.display()));
        let rules = match builder.build() {
            Ok(g) => Some(g),
            Err(e) => {
                problem = Some(format!("{}: {e}", file.display()));
                None
            }
        };
        (
            Arc::new(Self {
                parent: Some(parent.clone()),
                rules,
            }),
            problem,
        )
    }

    /// Whether `path` (absolute, under the directory of this stack) is ignored. The deepest
    /// file with a matching pattern decides, so a nested `!pattern` can re-include.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut level = Some(self);
        while let Some(s) = level {
            if let Some(rules) = &s.rules {
                let m = rules.matched(path, is_dir);
                if m.is_ignore() {
                    return true;
                }
                if m.is_whitelist() {
                    return false;
                }
            }
            level = s.parent.as_deref();
        }
        false
    }
}
