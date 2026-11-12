//! `.lodeconfig`, the versioned branch configuration (SPEC §5.6).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::glob::{GlobError, PathGlobSet};
use crate::path::DepotPath;

/// One check that must pass before a shelf is submitted, and who may report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRequirement {
    /// Check name, such as `ci/build`.
    pub name: String,
    /// Name of the principal allowed to report it.
    pub reporter: String,
}

/// Parsed `.lodeconfig`. Unknown sections and keys are ignored so that newer files still
/// parse on older servers.
#[derive(Debug, Clone, PartialEq)]
pub struct LodeConfig {
    /// `[locks].required`: paths that need a lock to be modified or deleted.
    pub lock_required: PathGlobSet,
    /// `[files].binary`: paths always handled as binary.
    pub binary: PathGlobSet,
    /// `[files].text`: paths always handled as text.
    pub text: PathGlobSet,
    /// `[checks].branches`: branches whose shelf submits are gated.
    pub checks_branches: PathGlobSet,
    /// `[checks].required`: checks that must be `success`.
    pub checks_required: Vec<CheckRequirement>,
    /// Every other top-level table, by name, for modules that define their own section.
    pub sections: BTreeMap<String, toml::Table>,
}

impl Default for LodeConfig {
    fn default() -> Self {
        Self {
            lock_required: PathGlobSet::empty(),
            binary: PathGlobSet::empty(),
            text: PathGlobSet::empty(),
            checks_branches: PathGlobSet::empty(),
            checks_required: Vec::new(),
            sections: BTreeMap::new(),
        }
    }
}

/// Why a `.lodeconfig` is invalid. An invalid file cannot be committed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Not valid UTF-8.
    #[error(".lodeconfig is not UTF-8")]
    NotUtf8,
    /// Not valid TOML, or a known key has the wrong type.
    #[error(".lodeconfig: {0}")]
    Toml(String),
    /// A pattern does not compile.
    #[error(".lodeconfig: {0}")]
    Glob(#[from] GlobError),
    /// A required check has an empty name or reporter.
    #[error(".lodeconfig: [checks].required entries need a non-empty name and reporter")]
    CheckEntry,
}

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    locks: RawLocks,
    #[serde(default)]
    files: RawFiles,
    #[serde(default)]
    checks: RawChecks,
    #[serde(flatten)]
    rest: toml::Table,
}

#[derive(Deserialize, Default)]
struct RawLocks {
    #[serde(default)]
    required: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawFiles {
    #[serde(default)]
    binary: Vec<String>,
    #[serde(default)]
    text: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawChecks {
    #[serde(default)]
    branches: Vec<String>,
    #[serde(default)]
    required: Vec<RawCheck>,
}

#[derive(Deserialize)]
struct RawCheck {
    name: String,
    // No default: an entry without a reporter makes the file invalid, because an unbound
    // check could be satisfied by anyone holding the `checks` scope.
    reporter: String,
}

impl LodeConfig {
    /// Path of the file at the branch root.
    pub const PATH: &'static str = ".lodeconfig";

    /// The depot path of the file.
    pub fn depot_path() -> DepotPath {
        // A fixed, valid path; parsing cannot fail.
        DepotPath::parse(Self::PATH).unwrap_or_else(|_| unreachable!())
    }

    /// Parses file content.
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ConfigError::NotUtf8)?;
        let raw: Raw = toml::from_str(text).map_err(|e| ConfigError::Toml(e.message().into()))?;
        let checks_required = raw
            .checks
            .required
            .into_iter()
            .map(|c| {
                if c.name.trim().is_empty() || c.reporter.trim().is_empty() {
                    Err(ConfigError::CheckEntry)
                } else {
                    Ok(CheckRequirement {
                        name: c.name,
                        reporter: c.reporter,
                    })
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            lock_required: PathGlobSet::new(raw.locks.required)?,
            binary: PathGlobSet::new(raw.files.binary)?,
            text: PathGlobSet::new(raw.files.text)?,
            checks_branches: PathGlobSet::new(raw.checks.branches)?,
            checks_required,
            sections: raw
                .rest
                .into_iter()
                .filter_map(|(k, v)| match v {
                    toml::Value::Table(t) => Some((k, t)),
                    _ => None,
                })
                .collect(),
        })
    }

    /// A module's own section, such as `[example]`, if the file has one.
    pub fn section(&self, name: &str) -> Option<&toml::Table> {
        self.sections.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> DepotPath {
        DepotPath::parse(s).unwrap()
    }

    #[test]
    fn parses_spec_example() {
        let c = LodeConfig::parse(
            br#"
[locks]
required = ["**/*.psd", "Content/**/*.uasset"]

[files]
binary = ["**/*.dat"]
text   = ["**/*.shader"]

[checks]
branches = ["main", "release/*"]
required = [{ name = "ci/build", reporter = "ci" }]
"#,
        )
        .unwrap();
        assert!(c.lock_required.matches(&p("Content/a.uasset")));
        assert!(c.binary.matches(&p("x/y.dat")));
        assert!(c.text.matches(&p("s.shader")));
        assert!(c.checks_branches.matches_str("release/2"));
        assert_eq!(
            c.checks_required,
            vec![CheckRequirement {
                name: "ci/build".into(),
                reporter: "ci".into()
            }]
        );
    }

    #[test]
    fn empty_file_and_unknown_keys() {
        assert_eq!(LodeConfig::parse(b"").unwrap(), LodeConfig::default());
        let c = LodeConfig::parse(b"[future]\nx = 1\n[locks]\nrequired=[\"*.a\"]\nmode=\"x\"\n")
            .unwrap();
        assert_eq!(c.lock_required.patterns(), ["*.a"]);
    }

    #[test]
    fn rejects_invalid_files() {
        assert_eq!(LodeConfig::parse(&[0xff]), Err(ConfigError::NotUtf8));
        assert!(matches!(
            LodeConfig::parse(b"[locks\n"),
            Err(ConfigError::Toml(_))
        ));
        assert!(matches!(
            LodeConfig::parse(b"[locks]\nrequired = \"x\"\n"),
            Err(ConfigError::Toml(_))
        ));
        assert!(matches!(
            LodeConfig::parse(b"[locks]\nrequired = [\"a/[b\"]\n"),
            Err(ConfigError::Glob(_))
        ));
        assert!(matches!(
            LodeConfig::parse(b"[checks]\nrequired = [{ name = \"ci\" }]\n"),
            Err(ConfigError::Toml(_))
        ));
        assert_eq!(
            LodeConfig::parse(b"[checks]\nrequired = [{ name = \"ci\", reporter = \"\" }]\n"),
            Err(ConfigError::CheckEntry)
        );
    }

    #[test]
    fn depot_path_is_valid() {
        assert_eq!(LodeConfig::depot_path().as_str(), ".lodeconfig");
    }
}
