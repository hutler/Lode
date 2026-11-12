//! Glob matching over depot paths and branch names.
//!
//! Syntax is `globset`'s with `literal_separator` on: `*` and `?` never match `/`, `**`
//! matches any number of segments, `{a,b}` and `[abc]` work as usual. Matching is
//! case-sensitive, like depot paths.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::path::DepotPath;

/// A compiled list of glob patterns. Matches when any pattern matches.
#[derive(Debug, Clone)]
pub struct PathGlobSet {
    patterns: Vec<String>,
    set: GlobSet,
}

/// A pattern that does not compile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid glob pattern {pattern:?}: {reason}")]
pub struct GlobError {
    /// The pattern as given.
    pub pattern: String,
    /// The parser's explanation.
    pub reason: String,
}

impl PathGlobSet {
    /// Compiles `patterns`.
    pub fn new<I, S>(patterns: I) -> Result<Self, GlobError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let patterns: Vec<String> = patterns.into_iter().map(Into::into).collect();
        let mut builder = GlobSetBuilder::new();
        for p in &patterns {
            let glob = GlobBuilder::new(p)
                .literal_separator(true)
                .backslash_escape(true)
                .build()
                .map_err(|e| GlobError {
                    pattern: p.clone(),
                    reason: e.kind().to_string(),
                })?;
            builder.add(glob);
        }
        let set = builder.build().map_err(|e| GlobError {
            pattern: patterns.join(", "),
            reason: e.to_string(),
        })?;
        Ok(Self { patterns, set })
    }

    /// An empty set, which matches nothing.
    pub fn empty() -> Self {
        Self {
            patterns: Vec::new(),
            set: GlobSet::empty(),
        }
    }

    /// The patterns as given.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// True if there are no patterns.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// True if any pattern matches the path.
    pub fn matches(&self, path: &DepotPath) -> bool {
        self.set.is_match(path.as_str())
    }

    /// True if any pattern matches `s`, such as a branch name.
    pub fn matches_str(&self, s: &str) -> bool {
        self.set.is_match(s)
    }

    /// Fills `out` with the indices (into [`PathGlobSet::patterns`]) of every pattern that
    /// matches `s`, in ascending order. `out` is cleared first, so one buffer can be reused
    /// across many paths.
    pub fn matches_into(&self, s: &str, out: &mut Vec<usize>) {
        self.set.matches_into(s, out);
    }
}

impl PartialEq for PathGlobSet {
    fn eq(&self, other: &Self) -> bool {
        self.patterns == other.patterns
    }
}

impl Eq for PathGlobSet {}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> DepotPath {
        DepotPath::parse(s).unwrap()
    }

    #[test]
    fn spec_examples() {
        let locks = PathGlobSet::new(["**/*.psd", "Content/**/*.uasset"]).unwrap();
        assert!(locks.matches(&p("art/x.psd")));
        assert!(locks.matches(&p("x.psd")));
        assert!(locks.matches(&p("Content/Maps/a/b.uasset")));
        assert!(locks.matches(&p("Content/b.uasset")));
        assert!(!locks.matches(&p("Other/b.uasset")));
        assert!(!locks.matches(&p("x.PSD")));
    }

    #[test]
    fn star_does_not_cross_separators() {
        let g = PathGlobSet::new(["release/*", "src/*.rs"]).unwrap();
        assert!(g.matches_str("release/1.0"));
        assert!(!g.matches_str("release/1.0/hotfix"));
        assert!(g.matches(&p("src/main.rs")));
        assert!(!g.matches(&p("src/a/main.rs")));
    }

    #[test]
    fn matches_into_reports_every_matching_pattern() {
        let g = PathGlobSet::new(["**", "src/**", "*.md", "src/*.rs"]).unwrap();
        let mut out = vec![99];
        g.matches_into("src/main.rs", &mut out);
        assert_eq!(out, [0, 1, 3]);
        g.matches_into("README.md", &mut out);
        assert_eq!(out, [0, 2]);
    }

    #[test]
    fn empty_matches_nothing_and_errors_name_pattern() {
        assert!(!PathGlobSet::empty().matches(&p("a")));
        let err = PathGlobSet::new(["a/[b"]).unwrap_err();
        assert_eq!(err.pattern, "a/[b");
    }
}
