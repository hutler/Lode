//! The terminal side of [`lode_ops::output::Ui`]: human output to stdout, diagnostics and
//! progress to stderr, questions on the terminal.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lode_ops::error::OpResult;
use lode_ops::output::{human_bytes, ProgressEvent, Ui};

/// Prints to the terminal. Progress is drawn only on a terminal and without `--json`.
#[derive(Debug)]
pub struct Terminal {
    progress_enabled: bool,
    /// When the progress line was last drawn, if it has been in this counter.
    last: Mutex<Option<Instant>>,
}

impl Terminal {
    /// A terminal UI; `json` turns progress off.
    pub fn new(json: bool) -> Self {
        Self {
            progress_enabled: !json && std::io::stderr().is_terminal(),
            last: Mutex::new(None),
        }
    }
}

impl Ui for Terminal {
    fn line(&self, text: &str) {
        println!("{text}");
    }

    fn warn(&self, text: &str) {
        eprintln!("warning: {text}");
    }

    fn note(&self, text: &str) {
        eprintln!("{text}");
    }

    fn open_url(&self, url: &str) -> bool {
        lode_ops::output::open_in_browser(url).is_ok()
    }

    fn progress(&self, e: &ProgressEvent<'_>) {
        if !self.progress_enabled {
            return;
        }
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        if e.finished {
            if last.take().is_some() {
                eprintln!();
            }
            return;
        }
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(100))
            && e.done < e.total
        {
            return;
        }
        *last = Some(now);
        let mut err = std::io::stderr().lock();
        let _ = write!(
            err,
            "\r{} {}/{} ({})",
            e.label,
            e.done,
            e.total,
            human_bytes(e.bytes)
        );
        let _ = err.flush();
    }

    fn raw(&self, bytes: &[u8]) -> OpResult<()> {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()?;
        Ok(())
    }

    fn stream(&self) -> Option<Box<dyn tokio::io::AsyncWrite + Unpin + Send>> {
        Some(Box::new(tokio::io::stdout()))
    }

    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn prompt(&self, label: &str, secret: bool) -> OpResult<String> {
        if secret {
            return Ok(rpassword::prompt_password(label)?);
        }
        eprint!("{label}");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim().to_owned())
    }

    fn confirm(&self, question: &str) -> OpResult<bool> {
        eprint!("{question} [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
    }

    fn edit(&self, editor: &str, file: &Path) -> OpResult<bool> {
        Ok(editor_command(editor, file).status()?.success())
    }

    fn read_all(&self) -> OpResult<String> {
        Ok(std::io::read_to_string(std::io::stdin())?)
    }

    fn read_line(&self) -> OpResult<String> {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    }
}

/// Runs `$EDITOR file` through the shell, so an editor with arguments works.
fn editor_command(editor: &str, file: &Path) -> std::process::Command {
    if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(format!("{editor} \"{}\"", file.display()));
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(file);
        c
    }
}

/// The UI of `lode mcp`, whose standard input and output carry the protocol: warnings go to
/// stderr, everything else is dropped, and nobody is asked anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct Protocol;

impl Ui for Protocol {
    fn line(&self, _text: &str) {}

    fn warn(&self, text: &str) {
        eprintln!("warning: {text}");
    }

    fn progress(&self, _event: &ProgressEvent<'_>) {}
}
