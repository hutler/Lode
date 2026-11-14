//! The workspace view's patterns for the view editor (T134): whether a folder is in the view,
//! and the patterns after ticking a folder in or out. Patterns match the way the server
//! matches them (`lode_core`'s globs).

use lode_core::PathGlobSet;

/// Include and exclude patterns. Without includes everything is included; excludes win.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spec {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Spec {
    /// The `view` object of `Desk::view` and `Desk::view_apply`.
    pub fn from_json(v: &serde_json::Value) -> Self {
        let list = |k: &str| -> Vec<String> {
            v[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            include: list("include"),
            exclude: list("exclude"),
        }
    }
}

/// Whether `path` matches the glob `pattern`; a pattern that does not compile matches nothing.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    PathGlobSet::new([pattern]).is_ok_and(|g| g.matches_str(path))
}

/// Whether the view takes `path`.
pub fn in_view(spec: &Spec, path: &str) -> bool {
    let included = spec.include.is_empty() || spec.include.iter().any(|p| glob_match(p, path));
    included && !spec.exclude.iter().any(|p| glob_match(p, path))
}

/// A name no real pattern targets, standing for "anything in this folder".
const PROBE: &str = "\u{2063}any";

/// How the view treats a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderState {
    /// Files in the folder are in the view.
    pub on: bool,
    /// Patterns go deeper into it, so some of it may differ.
    pub mixed: bool,
    /// An exclude pattern above the folder keeps it out; ticking cannot bring it back.
    pub locked_by: Option<String>,
}

/// How the view treats the folder `dir`.
pub fn folder_state(spec: &Spec, dir: &str) -> FolderState {
    let probe = format!("{dir}/{PROBE}");
    let own = format!("{dir}/**");
    let inside = format!("{dir}/");
    let on = in_view(spec, &probe);
    let mixed = spec
        .include
        .iter()
        .chain(&spec.exclude)
        .any(|p| p.starts_with(&inside) && *p != own);
    let locked_by = if on {
        None
    } else {
        spec.exclude
            .iter()
            .find(|p| **p != own && glob_match(p, &probe))
            .cloned()
    };
    FolderState {
        on,
        mixed,
        locked_by,
    }
}

/// The view with folder `dir` ticked in or out.
pub fn tick_folder(spec: &Spec, dir: &str, on: bool) -> Spec {
    let own = format!("{dir}/**");
    let probe = format!("{dir}/{PROBE}");
    let inside_prefix = format!("{dir}/");
    // Patterns inside the folder no longer mean anything once the whole folder is decided.
    let inside = |p: &String| p.starts_with(&inside_prefix) && *p != own;
    let mut include: Vec<String> = spec
        .include
        .iter()
        .filter(|p| !inside(p))
        .cloned()
        .collect();
    let mut exclude: Vec<String> = spec
        .exclude
        .iter()
        .filter(|p| !inside(p))
        .cloned()
        .collect();
    if on {
        exclude.retain(|p| *p != own);
        let now = Spec {
            include: include.clone(),
            exclude: exclude.clone(),
        };
        if !in_view(&now, &probe) && !include.is_empty() {
            include.push(own);
        }
    } else {
        let others: Vec<String> = include.iter().filter(|p| **p != own).cloned().collect();
        let without = Spec {
            include: others.clone(),
            exclude: exclude.clone(),
        };
        if include.contains(&own) && !others.is_empty() && !in_view(&without, &probe) {
            include = others;
        } else if !exclude.contains(&own) {
            exclude.push(own);
        }
    }
    Spec { include, exclude }
}

/// Patterns as typed, one per line, without blanks.
pub fn parse_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(include: &[&str], exclude: &[&str]) -> Spec {
        Spec {
            include: include.iter().map(|s| (*s).to_owned()).collect(),
            exclude: exclude.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn globs_match_like_the_server() {
        assert!(glob_match("Content/**", "Content/Maps/a.umap"));
        assert!(glob_match("*.txt", "a.txt"));
        assert!(!glob_match("*.txt", "d/a.txt"));
        assert!(glob_match("**/*.txt", "d/a.txt"));
        assert!(!glob_match("[", "x"), "a bad pattern matches nothing");
    }

    #[test]
    fn folder_states() {
        let s = spec(&[], &["Content/Cinematics/**"]);
        assert!(folder_state(&s, "Content").on);
        assert!(folder_state(&s, "Content").mixed);
        assert!(!folder_state(&s, "Content/Cinematics").on);
        let deep = folder_state(&s, "Content/Cinematics/Intro");
        assert_eq!(deep.locked_by.as_deref(), Some("Content/Cinematics/**"));
    }

    #[test]
    fn ticking_folders() {
        let all = Spec::default();
        let out = tick_folder(&all, "Content/Cinematics", false);
        assert_eq!(out, spec(&[], &["Content/Cinematics/**"]));
        assert_eq!(tick_folder(&out, "Content/Cinematics", true), all);
        // Under includes, ticking a folder adds its own include, and unticking it drops it.
        let some = spec(&["Source/**"], &[]);
        let more = tick_folder(&some, "Content", true);
        assert_eq!(more, spec(&["Source/**", "Content/**"], &[]));
        assert_eq!(tick_folder(&more, "Content", false), some);
        // Deciding a folder drops the patterns inside it.
        let inner = spec(&[], &["Content/Maps/**"]);
        assert_eq!(
            tick_folder(&inner, "Content", false),
            spec(&[], &["Content/**"])
        );
    }

    #[test]
    fn patterns_from_text() {
        assert_eq!(parse_patterns(" a/** \r\n\n b\n"), ["a/**", "b"]);
    }
}
