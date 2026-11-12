//! Human output to stdout, diagnostics and progress to stderr; with `--json` stdout carries
//! only the one JSON document.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

/// Where output goes.
#[derive(Debug, Clone, Copy)]
pub struct Out {
    /// `--json` mode.
    pub json: bool,
}

impl Out {
    /// A line of human output (suppressed with `--json`).
    pub fn line(&self, text: impl AsRef<str>) {
        if !self.json {
            println!("{}", text.as_ref());
        }
    }

    /// A warning on stderr.
    pub fn warn(&self, text: impl AsRef<str>) {
        eprintln!("warning: {}", text.as_ref());
    }

    /// A progress display, active only on a terminal and without `--json`.
    pub fn progress(&self, label: &str, total: u64) -> Progress {
        Progress {
            enabled: !self.json && std::io::stderr().is_terminal(),
            label: label.to_owned(),
            total,
            done: 0,
            bytes: 0,
            last: None,
        }
    }
}

/// A one-line counter redrawn on stderr.
#[derive(Debug)]
pub struct Progress {
    enabled: bool,
    label: String,
    total: u64,
    done: u64,
    bytes: u64,
    last: Option<Instant>,
}

impl Progress {
    /// Counts one finished item of `bytes`.
    pub fn step(&mut self, bytes: u64) {
        self.done += 1;
        self.bytes += bytes;
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(100))
            && self.done < self.total
        {
            return;
        }
        self.last = Some(now);
        let mut err = std::io::stderr().lock();
        let _ = write!(
            err,
            "\r{} {}/{} ({})",
            self.label,
            self.done,
            self.total,
            human_bytes(self.bytes)
        );
        let _ = err.flush();
    }

    /// Ends the line.
    pub fn finish(&mut self) {
        if self.enabled && self.last.is_some() {
            eprintln!();
        }
        self.enabled = false;
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
