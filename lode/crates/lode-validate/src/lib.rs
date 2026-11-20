//! Validation scripts (SPEC §6.8): finding Python, running one script under the script
//! contract, and reading its answer. Shared by the client (`lode-ops`, before shelve and
//! commit) and the server (`lode-mod-validate`, on every shelf version).
//!
//! The contract, as a script sees it:
//!
//! - It is started as `<python> -X utf8 <script>` with no shell, its working directory at a
//!   root that holds the depot's files at their depot paths.
//! - The environment is cleared except for what Python needs to start (`PATH`, `SYSTEMROOT`,
//!   temporary and home directories, locale) plus `PYTHONDONTWRITEBYTECODE=1`, `PYTHONUTF8=1`
//!   and `LODE_VALIDATE=1`. No token or proxy setting reaches it.
//! - Standard input is one JSON document, [`Input`].
//! - Exit code 0 passes, 1 fails, anything else (or a timeout) is an error: the check itself
//!   broke. An unhandled exception (a Python traceback on standard error) is an error too,
//!   although Python exits 1 for it.
//! - Each standard output line that is a JSON object with a `message` is a [`Problem`]; any
//!   other line is kept as text. Output is cut at [`MAX_OUTPUT_BYTES`].

#![forbid(unsafe_code)]

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use lode_core::{Change, ChangeAction, PathGlobSet};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Version of [`Input`].
pub const INPUT_VERSION: u32 = 1;
/// Standard output and standard error are each kept up to this size.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// How long finding out a Python's version may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Environment variables passed through to a script, when set.
const PASS_ENV: &[&str] = &[
    "PATH",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

/// What a script reads on standard input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    /// Always [`INPUT_VERSION`].
    pub version: u32,
    /// `shelve`, `commit`, `manual` (`lode validate`) or `server`.
    pub phase: String,
    /// Depot name.
    pub depot: String,
    /// Branch name.
    pub branch: String,
    /// The changed files that match the script's `paths`.
    pub files: Vec<InputFile>,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFile {
    /// Depot path; the file is at this path under the working directory, unless deleted.
    pub path: String,
    /// `add`, `edit`, `delete` or `move`.
    pub action: String,
    /// For a move, where the file was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The files of a change set as scripts see them. An add copied from a path the same set
/// deletes is a move.
pub fn input_files(changes: &[Change]) -> Vec<InputFile> {
    let deleted: std::collections::HashSet<&str> = changes
        .iter()
        .filter(|c| c.action == ChangeAction::Delete)
        .map(|c| c.path.as_str())
        .collect();
    let moved_away: std::collections::HashSet<&str> = changes
        .iter()
        .filter_map(|c| c.copy_from.as_ref())
        .map(|f| f.path.as_str())
        .filter(|p| deleted.contains(p))
        .collect();
    let mut out = Vec::new();
    for c in changes {
        let (action, from) = match c.action {
            ChangeAction::Add => match &c.copy_from {
                Some(f) if moved_away.contains(f.path.as_str()) => {
                    ("move", Some(f.path.as_str().to_owned()))
                }
                _ => ("add", None),
            },
            ChangeAction::Modify => ("edit", None),
            // The delete half of a move is reported by the add.
            ChangeAction::Delete if moved_away.contains(c.path.as_str()) => continue,
            ChangeAction::Delete => ("delete", None),
        };
        out.push(InputFile {
            path: c.path.as_str().to_owned(),
            action: action.to_owned(),
            from,
        });
    }
    out
}

/// The files a script with these `paths` runs on. A move matches on either path.
pub fn matching(files: &[InputFile], paths: &PathGlobSet) -> Vec<InputFile> {
    files
        .iter()
        .filter(|f| {
            paths.matches_str(&f.path) || f.from.as_deref().is_some_and(|p| paths.matches_str(p))
        })
        .cloned()
        .collect()
}

/// How bad a problem is. Warnings never fail a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Something that must be fixed.
    #[default]
    Error,
    /// Worth a look.
    Warning,
}

/// One problem a script reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// Depot path it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Line, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// `error` (default) or `warning`.
    #[serde(default)]
    pub level: Level,
    /// What is wrong.
    pub message: String,
}

/// How a script ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Exit code 0.
    Passed,
    /// Exit code 1: the content is not valid.
    Failed,
    /// The script broke: another exit code, a timeout, or it could not start.
    Error,
    /// Not run: no matching files, or no Python.
    Skipped,
}

impl Outcome {
    /// Whether this outcome stops a shelve or commit.
    pub fn refuses(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::Error)
    }
}

/// One script's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptResult {
    /// The script's name in `.lodeconfig`.
    pub name: String,
    /// How it ended.
    pub outcome: Outcome,
    /// Why it errored or was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Problems from its standard output.
    #[serde(default)]
    pub problems: Vec<Problem>,
    /// Its other output: plain standard output lines, and standard error when it broke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How many files it was given.
    pub files: usize,
    /// Wall time.
    pub seconds: f64,
}

impl ScriptResult {
    /// A script that was not started.
    pub fn skipped(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            outcome: Outcome::Skipped,
            reason: Some(reason.into()),
            problems: Vec::new(),
            text: None,
            files: 0,
            seconds: 0.0,
        }
    }

    /// A script that broke before or while it ran.
    pub fn error(name: &str, files: usize, reason: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Error,
            files,
            ..Self::skipped(name, reason)
        }
    }

    /// A short summary: the first problems, or the reason, at most `max` bytes.
    pub fn summary(&self, max: usize) -> String {
        let mut s = match self.outcome {
            Outcome::Passed => format!("passed ({} files)", self.files),
            Outcome::Skipped => format!(
                "skipped: {}",
                self.reason.as_deref().unwrap_or("nothing to check")
            ),
            Outcome::Error => format!(
                "the check itself broke: {}",
                self.reason.as_deref().unwrap_or("?")
            ),
            Outcome::Failed => {
                let errors: Vec<&Problem> = self
                    .problems
                    .iter()
                    .filter(|p| p.level == Level::Error)
                    .collect();
                if errors.is_empty() {
                    self.text
                        .as_deref()
                        .and_then(|t| t.lines().find(|l| !l.trim().is_empty()))
                        .unwrap_or("failed")
                        .to_owned()
                } else {
                    let first: Vec<String> = errors.iter().take(3).map(|p| describe(p)).collect();
                    let mut s = first.join("; ");
                    if errors.len() > 3 {
                        s.push_str(&format!(" (and {} more)", errors.len() - 3));
                    }
                    s
                }
            }
        };
        if s.len() > max {
            let mut cut = max.saturating_sub(3);
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            s.push_str("...");
        }
        s
    }
}

/// `path:line: message`, as far as known.
pub fn describe(p: &Problem) -> String {
    match (&p.path, p.line) {
        (Some(path), Some(line)) => format!("{path}:{line}: {}", p.message),
        (Some(path), None) => format!("{path}: {}", p.message),
        _ => p.message.clone(),
    }
}

/// A Python to run scripts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Python {
    /// Program and leading arguments, such as `["py", "-3"]`.
    pub command: Vec<String>,
    /// Its version.
    pub version: (u32, u32, u32),
}

impl Python {
    /// The version as `3.12.1`.
    pub fn version_text(&self) -> String {
        let (a, b, c) = self.version;
        format!("{a}.{b}.{c}")
    }

    /// Whether it is at least `min`.
    pub fn at_least(&self, min: Option<(u32, u32, u32)>) -> bool {
        min.is_none_or(|m| self.version >= m)
    }
}

/// Why no Python could be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindError {
    /// None of the commands tried runs a Python 3.
    Missing {
        /// The commands tried.
        tried: Vec<String>,
    },
    /// One was found, but it is older than needed.
    TooOld {
        /// What was found.
        found: Python,
    },
}

/// The commands tried when nothing names one: `py -3`, then `python`, then `python3` on
/// Windows; `python3`, then `python` elsewhere.
pub fn default_candidates() -> Vec<Vec<String>> {
    let c = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    if cfg!(windows) {
        vec![c(&["py", "-3"]), c(&["python"]), c(&["python3"])]
    } else {
        vec![c(&["python3"]), c(&["python"])]
    }
}

fn command(argv: &[String]) -> Option<Command> {
    let (program, args) = argv.split_first()?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for k in PASS_ENV {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd.env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("LODE_VALIDATE", "1");
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console window flashes up from Lode Desk.
        cmd.creation_flags(0x0800_0000);
    }
    Some(cmd)
}

/// The version of the Python that `argv` starts, if it starts a Python 3.
pub async fn probe(argv: &[String]) -> Option<(u32, u32, u32)> {
    let mut cmd = command(argv)?;
    cmd.args(["-c", "import sys; print(*sys.version_info[:3])"]);
    let out = tokio::time::timeout(PROBE_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split_whitespace().map(|p| p.parse::<u32>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    (v.0 == 3).then_some(v)
}

/// The first of `candidates` that runs Python 3, checked against `min`.
pub async fn find_python(
    candidates: &[Vec<String>],
    min: Option<(u32, u32, u32)>,
) -> Result<Python, FindError> {
    let mut too_old = None;
    for c in candidates {
        if let Some(version) = probe(c).await {
            let found = Python {
                command: c.clone(),
                version,
            };
            if found.at_least(min) {
                return Ok(found);
            }
            too_old.get_or_insert(found);
        }
    }
    match too_old {
        Some(found) => Err(FindError::TooOld { found }),
        None => Err(FindError::Missing {
            tried: candidates.iter().map(|c| c.join(" ")).collect(),
        }),
    }
}

/// Reads up to `cap` bytes and drains the rest, so the child never blocks on a full pipe.
async fn read_capped(mut r: impl AsyncRead + Unpin, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut cut = false;
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                if n > room {
                    cut = true;
                }
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    (kept, cut)
}

/// Splits standard output into problems and text.
pub fn parse_output(stdout: &str) -> (Vec<Problem>, String) {
    let mut problems = Vec::new();
    let mut text = String::new();
    for line in stdout.lines() {
        let t = line.trim();
        if t.starts_with('{') {
            if let Ok(p) = serde_json::from_str::<Problem>(t) {
                problems.push(p);
                continue;
            }
        }
        if !t.is_empty() || !text.is_empty() {
            text.push_str(line.trim_end());
            text.push('\n');
        }
    }
    (problems, text.trim_end().to_owned())
}

/// Runs one script and reads its answer. `root` is the working directory; `script` is the
/// script's file. Dropping the future stops the script.
pub async fn run(
    python: &Python,
    name: &str,
    root: &Path,
    script: &Path,
    input: &Input,
    timeout: Duration,
) -> ScriptResult {
    let started = Instant::now();
    let files = input.files.len();
    let Some(mut cmd) = command(&python.command) else {
        return ScriptResult::error(name, files, "no Python command");
    };
    cmd.arg("-X")
        .arg("utf8")
        .arg(script)
        .current_dir(root)
        .stdin(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ScriptResult::error(
                name,
                files,
                format!("could not start {}: {e}", python.command.join(" ")),
            )
        }
    };
    let stdin_json = serde_json::to_vec(input).unwrap_or_default();
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let work = async {
        let feed = async {
            if let Some(mut w) = stdin.take() {
                // A script that does not read its input may exit first; that is fine.
                let _ = w.write_all(&stdin_json).await;
                let _ = w.shutdown().await;
            }
        };
        let out = async {
            match stdout {
                Some(r) => read_capped(r, MAX_OUTPUT_BYTES).await,
                None => (Vec::new(), false),
            }
        };
        let err = async {
            match stderr {
                Some(r) => read_capped(r, MAX_OUTPUT_BYTES).await,
                None => (Vec::new(), false),
            }
        };
        let ((), out, err) = tokio::join!(feed, out, err);
        let status = child.wait().await;
        (status, out, err)
    };
    let (status, (out, out_cut), (err, err_cut)) = match tokio::time::timeout(timeout, work).await {
        Ok(r) => r,
        Err(_) => {
            return ScriptResult {
                seconds: started.elapsed().as_secs_f64(),
                ..ScriptResult::error(
                    name,
                    files,
                    format!("timed out after {} s", timeout.as_secs()),
                )
            };
        }
    };
    let (problems, mut text) = parse_output(&String::from_utf8_lossy(&out));
    if out_cut {
        text.push_str(&format!(
            "\n(output cut at {} KiB)",
            MAX_OUTPUT_BYTES / 1024
        ));
    }
    let code = status.ok().and_then(|s| s.code());
    // Python exits 1 on an unhandled exception too; that is a broken check, not bad content.
    let traceback = String::from_utf8_lossy(&err).contains("Traceback (most recent call last)");
    let (outcome, reason) = match code {
        Some(0) => (Outcome::Passed, None),
        Some(1) if traceback => (
            Outcome::Error,
            Some("the script stopped with an exception".to_owned()),
        ),
        Some(1) => (Outcome::Failed, None),
        Some(n) => (Outcome::Error, Some(format!("exit code {n}"))),
        None => (Outcome::Error, Some("stopped by a signal".to_owned())),
    };
    if outcome == Outcome::Error || (outcome == Outcome::Failed && problems.is_empty()) {
        let e = String::from_utf8_lossy(&err);
        let e = e.trim_end();
        if !e.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(e);
            if err_cut {
                text.push_str(&format!(
                    "\n(error output cut at {} KiB)",
                    MAX_OUTPUT_BYTES / 1024
                ));
            }
        }
    }
    ScriptResult {
        name: name.to_owned(),
        outcome,
        reason,
        problems,
        text: (!text.is_empty()).then_some(text),
        files,
        seconds: started.elapsed().as_secs_f64(),
    }
}
