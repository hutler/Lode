//! `--paths-from FILE`: path arguments read from a file (or standard input), for callers such
//! as editor plugins whose lists would not fit on a command line (about 32,000 characters on
//! Windows).
//!
//! The paths are appended to the arguments after `--` before they are parsed, so every command
//! that takes a list of paths gets them exactly as if they had been typed.

use std::ffi::OsString;
use std::io::Read;

use crate::error::{CliError, CliResult};

/// Commands that take a list of paths (`changelist` only as `changelist move`).
const COMMANDS: &[&str] = &[
    "status", "add", "rm", "revert", "diff", "commit", "lock", "unlock", "shelve", "clean",
];

/// Global options that take a value, so their value is not mistaken for the command.
const VALUED: &[&str] = &["--server", "-C", "--paths-from"];

/// `argv` with the paths from `--paths-from` appended, or unchanged without it.
pub fn expand(argv: Vec<OsString>) -> CliResult<Vec<OsString>> {
    let mut source: Option<OsString> = None;
    let mut words: Vec<String> = Vec::new();
    let mut has_separator = false;
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].to_string_lossy();
        if a == "--" {
            has_separator = true;
            break;
        }
        if let Some(v) = a.strip_prefix("--paths-from=") {
            source = Some(OsString::from(v));
        } else if VALUED.contains(&a.as_ref()) {
            if a == "--paths-from" {
                source = Some(argv.get(i + 1).cloned().ok_or_else(|| {
                    CliError::usage("--paths-from needs a file name, or - for standard input")
                })?);
            }
            i += 2;
            continue;
        } else if !a.starts_with('-') && words.len() < 2 {
            words.push(a.into_owned());
        }
        i += 1;
    }
    let Some(source) = source else {
        return Ok(argv);
    };
    let allowed = match words.first().map(String::as_str) {
        Some("changelist") => words.get(1).map(String::as_str) == Some("move"),
        Some(c) => COMMANDS.contains(&c),
        None => false,
    };
    if !allowed {
        return Err(CliError::usage(format!(
            "--paths-from works only with {} and changelist move",
            COMMANDS.join(", ")
        )));
    }
    let text = read(&source)?;
    let paths = parse(&text);
    if paths.is_empty() {
        // An empty list must not turn into "everything" for commands such as commit.
        return Err(CliError::usage(format!(
            "{} names no paths",
            source.to_string_lossy()
        )));
    }
    let mut out = argv;
    if !has_separator {
        out.push(OsString::from("--"));
    }
    out.extend(paths.into_iter().map(OsString::from));
    Ok(out)
}

fn read(source: &OsString) -> CliResult<String> {
    let bytes = if source == "-" {
        let mut buf = Vec::new();
        std::io::stdin().lock().read_to_end(&mut buf)?;
        buf
    } else {
        std::fs::read(source).map_err(|e| {
            CliError::usage(format!(
                "cannot read --paths-from {}: {e}",
                source.to_string_lossy()
            ))
        })?
    };
    String::from_utf8(bytes).map_err(|_| CliError::usage("the --paths-from list is not UTF-8 text"))
}

/// One path per line; a byte-order mark, `\r\n` line ends and blank lines are allowed.
fn parse(text: &str) -> Vec<String> {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn lines() {
        assert_eq!(
            parse("\u{feff}a.txt\r\n\r\ndir/b c.txt\n  \nlast"),
            vec!["a.txt", "dir/b c.txt", "last"]
        );
    }

    #[test]
    fn appends_after_a_separator() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("list");
        std::fs::write(&list, "x\n-y\n").unwrap();
        let l = list.to_str().unwrap();
        let out = expand(args(&[
            "lode",
            "--json",
            "commit",
            "-m",
            "msg",
            "--paths-from",
            l,
        ]))
        .unwrap();
        assert_eq!(out[out.len() - 3..], args(&["--", "x", "-y"])[..]);
        let out = expand(args(&[
            "lode",
            "-C",
            "w",
            "changelist",
            "move",
            "ui",
            "--paths-from",
            l,
        ]))
        .unwrap();
        assert_eq!(out.last().unwrap(), "-y");
        let out = expand(args(&[
            "lode",
            "add",
            &format!("--paths-from={l}"),
            "--",
            "z",
        ]))
        .unwrap();
        assert_eq!(out[out.len() - 3..], args(&["z", "x", "-y"])[..]);
        // No flag: untouched.
        assert_eq!(
            expand(args(&["lode", "log"])).unwrap(),
            args(&["lode", "log"])
        );
    }

    #[test]
    fn refusals() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "\n\n").unwrap();
        let e = empty.to_str().unwrap();
        assert!(expand(args(&["lode", "log", "--paths-from", e])).is_err());
        assert!(expand(args(&["lode", "changelist", "list", "--paths-from", e])).is_err());
        assert!(expand(args(&["lode", "commit", "--paths-from", e])).is_err());
        assert!(expand(args(&["lode", "commit", "--paths-from"])).is_err());
        let missing = dir.path().join("missing");
        assert!(expand(args(&[
            "lode",
            "add",
            "--paths-from",
            missing.to_str().unwrap()
        ]))
        .is_err());
    }
}
