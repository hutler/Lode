//! Where an operation's human output, progress and questions go.
//!
//! Operations never touch the terminal. They report through [`Out`], which hands lines,
//! warnings, notes and progress to a [`Ui`]: the `lode` binary prints them, the desktop app
//! turns them into events. With `--json` (`Out::json`) human lines and notes are dropped, so
//! the caller's one JSON document is all that reaches standard output.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use crate::error::{OpError, OpResult};

/// One progress update: `done` of `total` items, `bytes` so far.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProgressEvent<'a> {
    /// What is being counted, such as `Fetching` or `Uploading`.
    pub label: &'a str,
    /// Items finished.
    pub done: u64,
    /// Items in all.
    pub total: u64,
    /// Bytes of the finished items.
    pub bytes: u64,
    /// The last event of this counter.
    pub finished: bool,
}

/// The receiving side of an operation's output and questions.
///
/// Only the first three methods are required. The question methods default to "nobody to
/// ask": [`Ui::interactive`] is false, and an operation that would otherwise ask fails with
/// a usage error that says which option to pass instead.
pub trait Ui: Send + Sync {
    /// A line of human output (stdout for the CLI).
    fn line(&self, text: &str);
    /// A warning (stderr for the CLI, with a `warning:` prefix).
    fn warn(&self, text: &str);
    /// A progress update.
    fn progress(&self, event: &ProgressEvent<'_>);
    /// Commentary that is not the command's result (stderr for the CLI).
    fn note(&self, text: &str) {
        let _ = text;
    }
    /// Raw human output, such as a patch (stdout for the CLI).
    fn raw(&self, bytes: &[u8]) -> OpResult<()> {
        let _ = bytes;
        Ok(())
    }
    /// A stream for content too large to hold, such as `lode cat` of a big file; none when
    /// this UI has nowhere to put it.
    fn stream(&self) -> Option<Box<dyn tokio::io::AsyncWrite + Unpin + Send>> {
        None
    }
    /// Whether a person can answer questions right now.
    fn interactive(&self) -> bool {
        false
    }
    /// Asks for one line of text; `secret` hides what is typed.
    fn prompt(&self, label: &str, secret: bool) -> OpResult<String> {
        let _ = secret;
        Err(OpError::usage(format!(
            "{} needed but there is nobody to ask; pass it as an option",
            label.trim_end_matches(": ")
        )))
    }
    /// Asks a yes or no question; anything but yes is no.
    fn confirm(&self, question: &str) -> OpResult<bool> {
        let _ = question;
        Ok(false)
    }
    /// Opens `file` in `editor` and waits; true when the editor exited successfully.
    fn edit(&self, editor: &str, file: &Path) -> OpResult<bool> {
        let _ = (editor, file);
        Err(OpError::usage("no editor available here"))
    }
    /// Reads all of the input stream (`-F -` and similar).
    fn read_all(&self) -> OpResult<String> {
        Err(OpError::usage("there is no input stream to read from here"))
    }
    /// Reads one line of the input stream, with its line ending removed.
    fn read_line(&self) -> OpResult<String> {
        Err(OpError::usage("there is no input stream to read from here"))
    }
    /// Opens a web address in the user's browser; false when this UI cannot (the caller
    /// then shows the address instead).
    fn open_url(&self, url: &str) -> bool {
        let _ = url;
        false
    }
}

/// Opens an `http(s)` address in the default browser with the platform's opener
/// (`open` on macOS, `xdg-open` elsewhere on Unix, the URL handler on Windows). The address
/// is passed as one argument, never through a shell.
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only http and https addresses are opened",
        ));
    }
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // The opener returns at once (or hands over to the browser); it is not waited for, so
    // a reaper thread collects it.
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// A [`Ui`] that drops everything and asks nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Silent;

impl Ui for Silent {
    fn line(&self, _text: &str) {}
    fn warn(&self, _text: &str) {}
    fn progress(&self, _event: &ProgressEvent<'_>) {}
}

/// An operation's output.
#[derive(Clone)]
pub struct Out {
    /// `--json` mode: human lines and notes are dropped.
    pub json: bool,
    ui: Arc<dyn Ui>,
}

impl fmt::Debug for Out {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Out").field("json", &self.json).finish()
    }
}

impl Out {
    /// Output to `ui`.
    pub fn new(json: bool, ui: Arc<dyn Ui>) -> Self {
        Self { json, ui }
    }

    /// The receiver, for questions.
    pub fn ui(&self) -> &dyn Ui {
        self.ui.as_ref()
    }

    /// A line of human output (dropped with `--json`).
    pub fn line(&self, text: impl AsRef<str>) {
        if !self.json {
            self.ui.line(text.as_ref());
        }
    }

    /// A warning.
    pub fn warn(&self, text: impl AsRef<str>) {
        self.ui.warn(text.as_ref());
    }

    /// Commentary beside the result (dropped with `--json`).
    pub fn note(&self, text: impl AsRef<str>) {
        if !self.json {
            self.ui.note(text.as_ref());
        }
    }

    /// Raw human output (dropped with `--json`).
    pub fn raw(&self, bytes: &[u8]) -> OpResult<()> {
        if self.json {
            return Ok(());
        }
        self.ui.raw(bytes)
    }

    /// The UI's stream for large content, if it has one.
    pub fn stream(&self) -> Option<Box<dyn tokio::io::AsyncWrite + Unpin + Send>> {
        self.ui.stream()
    }

    /// A counter of `total` items.
    pub fn progress(&self, label: &str, total: u64) -> Progress {
        Progress {
            ui: Arc::clone(&self.ui),
            label: label.to_owned(),
            total,
            done: 0,
            bytes: 0,
            finished: false,
        }
    }
}

/// A counter reported to the [`Ui`] as it advances.
pub struct Progress {
    ui: Arc<dyn Ui>,
    label: String,
    total: u64,
    done: u64,
    bytes: u64,
    finished: bool,
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Progress")
            .field("label", &self.label)
            .field("done", &self.done)
            .field("total", &self.total)
            .finish()
    }
}

impl Progress {
    fn event(&self) -> ProgressEvent<'_> {
        ProgressEvent {
            label: &self.label,
            done: self.done,
            total: self.total,
            bytes: self.bytes,
            finished: self.finished,
        }
    }

    /// Counts one finished item of `bytes`.
    pub fn step(&mut self, bytes: u64) {
        self.done += 1;
        self.bytes += bytes;
        if !self.finished {
            self.ui.progress(&self.event());
        }
    }

    /// Ends the counter.
    pub fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            self.ui.progress(&self.event());
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

/// `12.3 MB` style sizes.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
