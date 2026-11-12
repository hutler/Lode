//! The workspace view: which depot paths are checked out (SPEC §5.2).

use lode_core::{DepotPath, PathGlobSet};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Include and exclude globs, as stored in workspace config and sent to the server. Without
/// includes everything is included; an exclude wins over an include.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSpec {
    /// Paths to check out; empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Paths to leave out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl ViewSpec {
    /// Compiles the patterns.
    pub fn compile(&self) -> Result<View> {
        Ok(View {
            include: PathGlobSet::new(self.include.iter().cloned())?,
            exclude: PathGlobSet::new(self.exclude.iter().cloned())?,
        })
    }
}

/// A compiled view.
#[derive(Debug, Clone)]
pub struct View {
    include: PathGlobSet,
    exclude: PathGlobSet,
}

impl Default for View {
    fn default() -> Self {
        Self {
            include: PathGlobSet::empty(),
            exclude: PathGlobSet::empty(),
        }
    }
}

impl View {
    /// Whether `path` is in the view.
    pub fn contains(&self, path: &DepotPath) -> bool {
        (self.include.is_empty() || self.include.matches(path)) && !self.exclude.matches(path)
    }

    /// Whether every path is in the view.
    pub fn is_everything(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn include_then_exclude() {
        let v = ViewSpec {
            include: vec!["src/**".into(), "README.md".into()],
            exclude: vec!["src/gen/**".into()],
        }
        .compile()
        .unwrap();
        let p = |s: &str| DepotPath::parse(s).unwrap();
        assert!(v.contains(&p("src/a.rs")));
        assert!(v.contains(&p("README.md")));
        assert!(!v.contains(&p("src/gen/x.rs")));
        assert!(!v.contains(&p("docs/a.md")));
        assert!(ViewSpec::default().compile().unwrap().contains(&p("x")));
        let json = serde_json::to_string(&ViewSpec::default()).unwrap();
        assert_eq!(json, "{}");
    }
}
