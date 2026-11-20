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

/// Longest validation script name (`[[validate.script]].name`); the check it reports is
/// `validate/<name>`.
pub const MAX_VALIDATE_NAME_LEN: usize = 64;
/// Default and largest timeout of one validation script.
pub const DEFAULT_VALIDATE_TIMEOUT_SECS: u32 = 60;
/// Largest timeout of one validation script (10 minutes).
pub const MAX_VALIDATE_TIMEOUT_SECS: u32 = 600;

/// `[validate]`: Python scripts that check content before it lands (SPEC §6.8).
#[derive(Debug, Clone, PartialEq)]
pub struct ValidateConfig {
    /// `python`: the lowest Python version the scripts need, as `(major, minor, patch)`.
    pub python: Option<(u32, u32, u32)>,
    /// `support`: files a script may import or read besides the changed files and itself.
    pub support: PathGlobSet,
    /// `[[validate.script]]`, in file order.
    pub scripts: Vec<ValidateScript>,
}

impl Default for ValidateConfig {
    fn default() -> Self {
        Self {
            python: None,
            support: PathGlobSet::empty(),
            scripts: Vec::new(),
        }
    }
}

impl ValidateConfig {
    /// Whether any script is configured.
    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }

    /// The `python` setting as text, such as `3.10`.
    pub fn python_text(&self) -> Option<String> {
        self.python.map(|(a, b, c)| {
            if c == 0 {
                format!("{a}.{b}")
            } else {
                format!("{a}.{b}.{c}")
            }
        })
    }
}

/// One `[[validate.script]]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidateScript {
    /// Name; the check is `validate/<name>`.
    pub name: String,
    /// Depot path of the script, ending in `.py`.
    pub run: DepotPath,
    /// The script runs only when a changed path matches.
    pub paths: PathGlobSet,
    /// Seconds before the script is stopped.
    pub timeout_secs: u32,
    /// Whether the server runs it too (default `true`).
    pub server: bool,
}

impl ValidateScript {
    /// The name of the check the server reports for this script.
    pub fn check_name(&self) -> String {
        format!("validate/{}", self.name)
    }
}

/// Whether `s` is a valid validation script name: 1 to 64 of `a-z`, `0-9`, `.`, `_`, `-`.
pub fn valid_validate_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_VALIDATE_NAME_LEN
        && s
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

/// Patterns merged as JSON when `[merge].json` is not set.
pub const DEFAULT_JSON_MERGE: &[&str] = &["**/*.json", "**/*.uproject", "**/*.uplugin"];

/// One `[merge.tools]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeToolRule {
    /// The pattern, as written.
    pub pattern: String,
    /// Compiled.
    pub glob: PathGlobSet,
    /// Tool name, looked up in the person's settings.
    pub tool: String,
}

fn default_json_merge() -> PathGlobSet {
    PathGlobSet::new(DEFAULT_JSON_MERGE.iter().copied()).unwrap_or_else(|_| PathGlobSet::empty())
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
    /// `[branches].shelf_only`: branches that take changes through shelf submits only.
    pub shelf_only: PathGlobSet,
    /// `[validate]`: validation scripts.
    pub validate: ValidateConfig,
    /// `[merge].json`: files merged key by key when a line merge conflicts (0.4). Defaults to
    /// [`DEFAULT_JSON_MERGE`] when the key is absent.
    pub merge_json: PathGlobSet,
    /// `[merge.tools]`: pattern → merge tool name, sorted by pattern (0.4). The command a name
    /// runs comes from the person's own settings, never from the depot.
    pub merge_tools: Vec<MergeToolRule>,
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
            shelf_only: PathGlobSet::empty(),
            validate: ValidateConfig::default(),
            merge_json: default_json_merge(),
            merge_tools: Vec::new(),
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
    /// A `[validate]` setting is wrong.
    #[error(".lodeconfig: [validate] {0}")]
    Validate(String),
}

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    locks: RawLocks,
    #[serde(default)]
    files: RawFiles,
    #[serde(default)]
    checks: RawChecks,
    #[serde(default)]
    merge: RawMerge,
    #[serde(default)]
    branches: RawBranches,
    #[serde(default)]
    validate: RawValidate,
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

#[derive(Deserialize, Default)]
struct RawMerge {
    json: Option<Vec<String>>,
    #[serde(default)]
    tools: toml::Table,
}

#[derive(Deserialize, Default)]
struct RawBranches {
    #[serde(default)]
    shelf_only: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawValidate {
    python: Option<String>,
    #[serde(default)]
    support: Vec<String>,
    #[serde(default)]
    script: Vec<RawScript>,
}

#[derive(Deserialize)]
struct RawScript {
    name: String,
    run: String,
    paths: Vec<String>,
    timeout: Option<toml::Value>,
    server: Option<bool>,
}

fn parse_python_version(s: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if !(2..=3).contains(&parts.len()) || parts[0] != "3" {
        return None;
    }
    let num = |p: &str| -> Option<u32> {
        if p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    Some((
        num(parts[0])?,
        num(parts[1])?,
        match parts.get(2) {
            Some(p) => num(p)?,
            None => 0,
        },
    ))
}

/// `60`, `"60"`, `"60s"`, `"2m"`: seconds.
fn parse_timeout(v: &toml::Value) -> Option<u32> {
    match v {
        toml::Value::Integer(n) => u32::try_from(*n).ok(),
        toml::Value::String(s) => {
            let s = s.trim();
            let (num, mul) = if let Some(n) = s.strip_suffix('s') {
                (n, 1)
            } else if let Some(n) = s.strip_suffix('m') {
                (n, 60)
            } else {
                (s, 1)
            };
            num.trim().parse::<u32>().ok()?.checked_mul(mul)
        }
        _ => None,
    }
}

fn parse_validate(raw: RawValidate) -> Result<ValidateConfig, ConfigError> {
    let bad = |m: String| ConfigError::Validate(m);
    let python = match raw.python {
        Some(p) => Some(parse_python_version(&p).ok_or_else(|| {
            bad(format!(
                "python = \"{p}\" is not a Python 3 version such as \"3.10\""
            ))
        })?),
        None => None,
    };
    let mut scripts: Vec<ValidateScript> = Vec::new();
    for s in raw.script {
        if !valid_validate_name(&s.name) {
            return Err(bad(format!(
                "script name \"{}\" must be 1 to {MAX_VALIDATE_NAME_LEN} of a-z, 0-9, `.`, `_` and `-`",
                s.name
            )));
        }
        if scripts.iter().any(|o| o.name == s.name) {
            return Err(bad(format!("script name \"{}\" is used twice", s.name)));
        }
        let run = DepotPath::parse(&s.run)
            .ok()
            .filter(|p| p.as_str().ends_with(".py"))
            .ok_or_else(|| {
                bad(format!(
                    "script \"{}\": run = \"{}\" must be a depot path ending in .py",
                    s.name, s.run
                ))
            })?;
        if s.paths.is_empty() {
            return Err(bad(format!(
                "script \"{}\": paths must list at least one pattern",
                s.name
            )));
        }
        let timeout_secs = match &s.timeout {
            None => DEFAULT_VALIDATE_TIMEOUT_SECS,
            Some(v) => parse_timeout(v)
                .filter(|t| (1..=MAX_VALIDATE_TIMEOUT_SECS).contains(t))
                .ok_or_else(|| {
                    bad(format!(
                        "script \"{}\": timeout must be 1 s to 10 min, such as \"60s\" or \"5m\"",
                        s.name
                    ))
                })?,
        };
        scripts.push(ValidateScript {
            name: s.name,
            run,
            paths: PathGlobSet::new(s.paths)?,
            timeout_secs,
            server: s.server.unwrap_or(true),
        });
    }
    Ok(ValidateConfig {
        python,
        support: PathGlobSet::new(raw.support)?,
        scripts,
    })
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
        let merge_json = match raw.merge.json {
            Some(p) => PathGlobSet::new(p)?,
            None => default_json_merge(),
        };
        let merge_tools = raw
            .merge
            .tools
            .into_iter()
            .map(|(pattern, tool)| {
                let toml::Value::String(tool) = tool else {
                    return Err(ConfigError::Toml(format!(
                        "[merge.tools] \"{pattern}\" must name a tool as a string"
                    )));
                };
                Ok(MergeToolRule {
                    glob: PathGlobSet::new([pattern.clone()])?,
                    pattern,
                    tool,
                })
            })
            .collect::<Result<_, ConfigError>>()?;
        Ok(Self {
            lock_required: PathGlobSet::new(raw.locks.required)?,
            binary: PathGlobSet::new(raw.files.binary)?,
            text: PathGlobSet::new(raw.files.text)?,
            checks_branches: PathGlobSet::new(raw.checks.branches)?,
            checks_required,
            shelf_only: PathGlobSet::new(raw.branches.shelf_only)?,
            validate: parse_validate(raw.validate)?,
            merge_json,
            merge_tools,
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

    /// The merge tool named for `path`: of the `[merge.tools]` patterns that match, the
    /// longest (the most specific, usually).
    pub fn merge_tool(&self, path: &DepotPath) -> Option<&MergeToolRule> {
        self.merge_tools
            .iter()
            .filter(|r| r.glob.matches(path))
            .max_by_key(|r| r.pattern.len())
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
    fn merge_section() {
        let d = LodeConfig::parse(b"").unwrap();
        assert!(d.merge_json.matches(&p("Game.uproject")));
        assert!(d.merge_json.matches(&p("a/b.json")));
        assert!(d.merge_tools.is_empty());
        let c = LodeConfig::parse(
            b"[merge]\njson = [\"cfg/**/*.json\"]\n[merge.tools]\n\"**/*.uasset\" = \"unreal\"\n\"Maps/**/*.uasset\" = \"maps\"\n",
        )
        .unwrap();
        assert!(c.merge_json.matches(&p("cfg/x/a.json")));
        assert!(!c.merge_json.matches(&p("a.json")));
        assert_eq!(c.merge_tool(&p("A/b.uasset")).unwrap().tool, "unreal");
        assert_eq!(c.merge_tool(&p("Maps/m.uasset")).unwrap().tool, "maps");
        assert!(c.merge_tool(&p("a.txt")).is_none());
        assert!(c.section("merge").is_none());
        assert!(matches!(
            LodeConfig::parse(b"[merge.tools]\n\"*.x\" = 3\n"),
            Err(ConfigError::Toml(_))
        ));
    }

    #[test]
    fn branches_section() {
        let c = LodeConfig::parse(b"[branches]\nshelf_only = [\"main\", \"release/*\"]\n").unwrap();
        assert!(c.shelf_only.matches_str("main"));
        assert!(c.shelf_only.matches_str("release/1.0"));
        assert!(!c.shelf_only.matches_str("dev"));
        assert!(LodeConfig::parse(b"").unwrap().shelf_only.is_empty());
        assert!(c.section("branches").is_none());
        assert!(matches!(
            LodeConfig::parse(b"[branches]\nshelf_only = [\"a/[b\"]\n"),
            Err(ConfigError::Glob(_))
        ));
    }

    #[test]
    fn validate_section() {
        let c = LodeConfig::parse(
            br#"
[validate]
python  = "3.10"
support = ["tools/validate/**"]

[[validate.script]]
name    = "json"
run     = "tools/validate/json_valid.py"
paths   = ["Data/**/*.json"]
timeout = "90s"

[[validate.script]]
name    = "naming"
run     = "tools/validate/asset_names.py"
paths   = ["Content/**"]
timeout = "2m"
server  = false

[[validate.script]]
name  = "size"
run   = "tools/validate/max_size.py"
paths = ["**"]
timeout = 5
"#,
        )
        .unwrap();
        let v = &c.validate;
        assert_eq!(v.python, Some((3, 10, 0)));
        assert_eq!(v.python_text().as_deref(), Some("3.10"));
        assert!(v.support.matches(&p("tools/validate/lib/x.py")));
        assert_eq!(v.scripts.len(), 3);
        assert_eq!(v.scripts[0].check_name(), "validate/json");
        assert_eq!(v.scripts[0].run.as_str(), "tools/validate/json_valid.py");
        assert!(v.scripts[0].paths.matches(&p("Data/a/b.json")));
        assert_eq!(v.scripts[0].timeout_secs, 90);
        assert!(v.scripts[0].server);
        assert_eq!(v.scripts[1].timeout_secs, 120);
        assert!(!v.scripts[1].server);
        assert_eq!(v.scripts[2].timeout_secs, 5);
        assert!(c.section("validate").is_none());
        let d = LodeConfig::parse(b"").unwrap();
        assert!(d.validate.is_empty());
        assert_eq!(d.validate.python, None);
    }

    #[test]
    fn validate_rejects() {
        let one = |extra: &str| {
            LodeConfig::parse(
                format!("[[validate.script]]\nname = \"a\"\nrun = \"t/a.py\"\npaths = [\"**\"]\n{extra}")
                    .as_bytes(),
            )
        };
        assert!(one("").is_ok());
        assert!(matches!(one("timeout = \"0s\""), Err(ConfigError::Validate(_))));
        assert!(matches!(one("timeout = \"11m\""), Err(ConfigError::Validate(_))));
        assert!(matches!(one("timeout = \"soon\""), Err(ConfigError::Validate(_))));
        assert!(matches!(one("timeout = -1"), Err(ConfigError::Validate(_))));
        assert!(matches!(
            one("[[validate.script]]\nname = \"a\"\nrun = \"t/b.py\"\npaths = [\"**\"]"),
            Err(ConfigError::Validate(_))
        ));
        let script = |name: &str, run: &str, paths: &str| {
            LodeConfig::parse(
                format!("[[validate.script]]\nname = \"{name}\"\nrun = \"{run}\"\npaths = {paths}\n")
                    .as_bytes(),
            )
        };
        assert!(matches!(script("A", "t/a.py", "[\"**\"]"), Err(ConfigError::Validate(_))));
        assert!(matches!(script("a/b", "t/a.py", "[\"**\"]"), Err(ConfigError::Validate(_))));
        assert!(matches!(script("", "t/a.py", "[\"**\"]"), Err(ConfigError::Validate(_))));
        assert!(matches!(script("a", "t/a.sh", "[\"**\"]"), Err(ConfigError::Validate(_))));
        assert!(matches!(script("a", "../a.py", "[\"**\"]"), Err(ConfigError::Validate(_))));
        assert!(matches!(script("a", "t/a.py", "[]"), Err(ConfigError::Validate(_))));
        assert!(matches!(script("a", "t/a.py", "[\"a/[b\"]"), Err(ConfigError::Glob(_))));
        assert!(matches!(
            LodeConfig::parse(b"[[validate.script]]\nname = \"a\"\npaths = [\"**\"]\n"),
            Err(ConfigError::Toml(_))
        ));
        for bad in ["2.7", "3", "3.x", "python3", "3.10.1.2", ""] {
            assert!(
                matches!(
                    LodeConfig::parse(format!("[validate]\npython = \"{bad}\"\n").as_bytes()),
                    Err(ConfigError::Validate(_))
                ),
                "{bad}"
            );
        }
        assert_eq!(
            LodeConfig::parse(b"[validate]\npython = \"3.9.2\"\n").unwrap().validate.python,
            Some((3, 9, 2))
        );
    }

    #[test]
    fn depot_path_is_valid() {
        assert_eq!(LodeConfig::depot_path().as_str(), ".lodeconfig");
    }
}
