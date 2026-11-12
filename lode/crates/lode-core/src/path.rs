//! Depot paths and the path rules of SPEC §5.7.

use std::fmt;
use std::str::FromStr;

use unicode_normalization::UnicodeNormalization;

/// A validated path relative to the depot root: UTF-8, NFC-normalized, `/`-separated, with
/// at least one segment. Construct one with [`DepotPath::parse`]; every value of this type
/// satisfies SPEC §5.7.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DepotPath(String);

/// The rule a path or path segment breaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The path is the empty string.
    #[error("path is empty")]
    Empty,
    /// Two separators in a row, or a leading or trailing separator.
    #[error("path has an empty segment")]
    EmptySegment,
    /// A segment is `.` or `..`.
    #[error("path segment {0:?} is not allowed")]
    DotSegment(String),
    /// A control character (U+0000–U+001F or U+007F–U+009F).
    #[error("path contains a control character")]
    ControlCharacter,
    /// A backslash anywhere in the path.
    #[error("path contains a backslash")]
    Backslash,
    /// A segment ends in `.` or a space, which Windows silently strips.
    #[error("path segment {0:?} ends in a dot or a space")]
    TrailingDotOrSpace(String),
    /// A segment is a Windows reserved device name, with or without an extension.
    #[error("path segment {0:?} is a reserved Windows device name")]
    ReservedName(String),
    /// A segment contains `/` (only when validating a single segment).
    #[error("path segment {0:?} contains a separator")]
    Separator(String),
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Checks one path segment (a file or directory name). Expects NFC input.
pub fn validate_segment(seg: &str) -> Result<(), PathError> {
    if seg.is_empty() {
        return Err(PathError::EmptySegment);
    }
    if seg.contains('/') {
        return Err(PathError::Separator(seg.to_owned()));
    }
    if seg == "." || seg == ".." {
        return Err(PathError::DotSegment(seg.to_owned()));
    }
    if seg.contains('\\') {
        return Err(PathError::Backslash);
    }
    if seg.chars().any(char::is_control) {
        return Err(PathError::ControlCharacter);
    }
    if seg.ends_with('.') || seg.ends_with(' ') {
        return Err(PathError::TrailingDotOrSpace(seg.to_owned()));
    }
    // Windows treats "con.txt" and "CON .tar.gz" as the device, so compare the part before
    // the first dot with trailing spaces removed.
    let stem = seg.split('.').next().unwrap_or(seg).trim_end_matches(' ');
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(PathError::ReservedName(seg.to_owned()));
    }
    Ok(())
}

impl DepotPath {
    /// Normalizes `s` to NFC and checks every rule of SPEC §5.7.
    pub fn parse(s: &str) -> Result<Self, PathError> {
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        let normalized: String = s.nfc().collect();
        for seg in normalized.split('/') {
            validate_segment(seg)?;
        }
        Ok(Self(normalized))
    }

    /// Wraps a proper ancestor string of an existing `DepotPath`, which is valid by
    /// construction.
    pub(crate) fn from_valid_ancestor(s: &str) -> Self {
        Self(s.to_owned())
    }

    /// The path as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The segments, root first.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The last segment.
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// The containing directory, or `None` for a path directly under the root.
    pub fn parent(&self) -> Option<DepotPath> {
        self.0.rfind('/').map(|i| Self(self.0[..i].to_owned()))
    }

    /// Every proper ancestor, nearest first (`a/b/c` gives `a/b`, then `a`).
    pub fn ancestors(&self) -> impl Iterator<Item = &str> {
        let s = self.0.as_str();
        s.match_indices('/').rev().map(move |(i, _)| &s[..i])
    }

    /// Appends one segment, validating it.
    pub fn join(&self, segment: &str) -> Result<DepotPath, PathError> {
        let seg: String = segment.nfc().collect();
        validate_segment(&seg)?;
        Ok(Self(format!("{}/{}", self.0, seg)))
    }

    /// True if `self` is `other` or lies under it.
    pub fn starts_with(&self, other: &DepotPath) -> bool {
        self.0 == other.0
            || (self.0.len() > other.0.len()
                && self.0.starts_with(&other.0)
                && self.0.as_bytes()[other.0.len()] == b'/')
    }

    /// Key for case-insensitive collision checks: two paths that may not coexist in one
    /// tree have equal keys.
    pub fn case_fold_key(&self) -> String {
        case_fold(&self.0)
    }
}

/// Case-folds a path or segment for collision checks.
pub fn case_fold(s: &str) -> String {
    s.to_lowercase().nfc().collect()
}

impl fmt::Display for DepotPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for DepotPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DepotPath({:?})", self.0)
    }
}

impl FromStr for DepotPath {
    type Err = PathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl AsRef<str> for DepotPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl serde::Serialize for DepotPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for DepotPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_paths() {
        for p in [
            "a",
            "a/b/c.txt",
            "Content/Maps/Level 1.umap",
            ".lodeconfig",
            ".hidden/x",
            "a..b",
            "CONSOLE.txt",
            "COM10",
            "LPT0",
            "x/con_file",
            "日本語/ファイル",
            "COM",
        ] {
            assert!(DepotPath::parse(p).is_ok(), "{p} should be valid");
        }
    }

    #[test]
    fn rejects_each_rule() {
        use PathError::*;
        let cases: &[(&str, PathError)] = &[
            ("", Empty),
            ("/a", EmptySegment),
            ("a/", EmptySegment),
            ("a//b", EmptySegment),
            (".", DotSegment(".".into())),
            ("a/../b", DotSegment("..".into())),
            ("a/./b", DotSegment(".".into())),
            ("a\\b", Backslash),
            ("a\u{0}b", ControlCharacter),
            ("a\nb", ControlCharacter),
            ("a\u{7f}", ControlCharacter),
            ("a\u{85}b", ControlCharacter),
            ("file.", TrailingDotOrSpace("file.".into())),
            ("dir /x", TrailingDotOrSpace("dir ".into())),
            ("CON", ReservedName("CON".into())),
            ("con", ReservedName("con".into())),
            ("NUL.txt", ReservedName("NUL.txt".into())),
            ("a/nul.tar.gz", ReservedName("nul.tar.gz".into())),
            ("COM1", ReservedName("COM1".into())),
            ("com1.log", ReservedName("com1.log".into())),
            ("LPT9", ReservedName("LPT9".into())),
            ("Aux", ReservedName("Aux".into())),
            ("PRN.x", ReservedName("PRN.x".into())),
            ("CON .txt", ReservedName("CON .txt".into())),
        ];
        for (input, want) in cases {
            assert_eq!(DepotPath::parse(input).as_ref(), Err(want), "{input:?}");
        }
    }

    #[test]
    fn normalizes_to_nfc() {
        // "é" as e + combining acute (NFD) becomes the single code point U+00E9.
        let p = DepotPath::parse("caf\u{65}\u{301}/x").unwrap();
        assert_eq!(p.as_str(), "caf\u{e9}/x");
        assert_eq!(p, DepotPath::parse("caf\u{e9}/x").unwrap());
    }

    #[test]
    fn navigation() {
        let p = DepotPath::parse("a/b/c").unwrap();
        assert_eq!(p.segments().collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(p.name(), "c");
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(DepotPath::parse("a").unwrap().parent(), None);
        assert_eq!(p.ancestors().collect::<Vec<_>>(), ["a/b", "a"]);
        assert_eq!(p.join("d").unwrap().as_str(), "a/b/c/d");
        assert!(p.join("..").is_err());
        assert!(p.join("x/y").is_err());
        let ab = DepotPath::parse("a/b").unwrap();
        assert!(p.starts_with(&ab));
        assert!(!DepotPath::parse("a/bc").unwrap().starts_with(&ab));
    }

    #[test]
    fn case_fold_key_matches_case_variants() {
        let a = DepotPath::parse("Dir/File.TXT").unwrap();
        let b = DepotPath::parse("dir/file.txt").unwrap();
        assert_eq!(a.case_fold_key(), b.case_fold_key());
    }

    #[test]
    fn serde_validates() {
        let p: DepotPath = serde_json::from_str("\"a/b\"").unwrap();
        assert_eq!(p.as_str(), "a/b");
        assert!(serde_json::from_str::<DepotPath>("\"../x\"").is_err());
    }
}
