//! A validation run belongs to one workspace operation. Cancellation is accepted only
//! while the core is waiting for a validator, before it starts sending content.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use lode_ops::output::{ProgressEvent, Ui};
use serde_json::Value;
use tokio::sync::Notify;

/// The operation whose exact selection and options are retained for a retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Inspect without sending.
    Check,
    /// Create a revision.
    Commit,
    /// Create or update a shelf.
    Shelve,
}

/// Events from the operation thread, scoped to the originating request.
pub enum Event {
    /// Current script and matching file count.
    Checking(String),
    /// The check phase has finished.
    Sending,
    /// Transfer progress after local checks pass.
    Progress(String),
    /// Ask the owner before running depot code.
    Trust(String),
    /// The client skipped checks because Python was unavailable.
    Python(String),
}

/// The window can stop a running check and answer a pending trust question.
#[derive(Default)]
pub struct Control {
    checking: AtomicBool,
    cancel: Notify,
    consent: Mutex<Option<mpsc::Sender<bool>>>,
}

impl Control {
    /// Request cancellation while a script is running.
    pub fn cancel(&self) {
        if self.checking.load(Ordering::Acquire) {
            self.cancel.notify_one();
        }
    }

    /// Answer the current question, if the worker is still waiting.
    pub fn answer(&self, yes: bool) {
        if let Ok(mut slot) = self.consent.lock() {
            if let Some(sender) = slot.take() {
                let _ = sender.send(yes);
            }
        }
    }

    pub(crate) async fn during<T>(
        &self,
        operation: impl std::future::Future<Output = lode_ops::error::OpResult<T>>,
    ) -> lode_ops::error::OpResult<T> {
        tokio::pin!(operation);
        tokio::select! {
            biased;
            _ = self.cancel.notified() => {
                if self.checking.load(Ordering::Acquire) {
                    Err(lode_ops::error::OpError::failed("cancelled", "Validation cancelled."))
                } else {
                    operation.await
                }
            }
            result = &mut operation => result,
        }
    }
}

pub(crate) struct ValidationUi {
    pub(crate) control: Arc<Control>,
    pub(crate) emit: Box<dyn Fn(Event) + Send + Sync>,
    pub(crate) parent: Arc<dyn Ui>,
}

impl Ui for ValidationUi {
    fn line(&self, text: &str) {
        self.parent.line(text);
    }
    fn progress(&self, event: &ProgressEvent<'_>) {
        let (_, text) = crate::app::progress_view(event);
        (self.emit)(Event::Progress(text));
        self.parent.progress(event);
    }
    fn warn(&self, text: &str) {
        if text.starts_with("this depot's validation scripts need ") {
            (self.emit)(Event::Python(text.into()));
        }
        self.parent.warn(text);
    }
    fn note(&self, text: &str) {
        if text.starts_with("Checking ") {
            self.control.checking.store(true, Ordering::Release);
            (self.emit)(Event::Checking(text.into()));
        } else if text.starts_with("Validation passed:") {
            self.control.checking.store(false, Ordering::Release);
            (self.emit)(Event::Sending);
        }
        self.parent.note(text);
    }
    fn interactive(&self) -> bool {
        true
    }
    fn confirm(&self, question: &str) -> lode_ops::error::OpResult<bool> {
        if !question.starts_with("This depot wants to run ") {
            return Ok(false);
        }
        let (tx, rx) = mpsc::channel();
        if let Ok(mut slot) = self.control.consent.lock() {
            *slot = Some(tx);
        } else {
            return Ok(false);
        }
        (self.emit)(Event::Trust(question.into()));
        Ok(rx.recv().unwrap_or(false))
    }
}

/// One script, file group, diagnostic or captured output in the results list.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    /// Visual role.
    pub kind: &'static str,
    /// Display text.
    pub label: String,
    /// Working-copy file to select for a diagnostic.
    pub path: String,
    /// One-based working-copy line, or zero when absent.
    pub line: i32,
}

impl Row {
    fn label(kind: &'static str, label: String) -> Self {
        Self {
            kind,
            label,
            path: String::new(),
            line: 0,
        }
    }
}

/// Groups diagnostics within each script without losing their path or source line.
pub fn rows(results: &Value) -> Vec<Row> {
    let mut rows = Vec::new();
    for script in results.as_array().into_iter().flatten() {
        let outcome = match script["outcome"].as_str() {
            Some("passed") => "Passed",
            Some("failed") => "Failed",
            Some("error") => "Check broke",
            Some("skipped") => "Skipped",
            _ => "Unknown outcome",
        };
        let name = script["name"].as_str().unwrap_or("Check");
        rows.push(Row::label("script", format!("{name} · {outcome}")));
        if let Some(reason) = script["reason"].as_str() {
            rows.push(Row::label("detail", reason.into()));
        }
        let mut groups = BTreeMap::<&str, Vec<&Value>>::new();
        for p in script["problems"].as_array().into_iter().flatten() {
            groups
                .entry(p["path"].as_str().unwrap_or(""))
                .or_default()
                .push(p);
        }
        for (path, problems) in groups {
            if !path.is_empty() {
                rows.push(Row::label("file", path.into()));
            }
            for p in problems {
                let line = p["line"]
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap_or(0);
                let level = p["level"].as_str().unwrap_or("error");
                let message = p["message"].as_str().unwrap_or("");
                let label = if line > 0 {
                    format!("Line {line} · {level}: {message}")
                } else {
                    format!("{level}: {message}")
                };
                rows.push(Row {
                    kind: "problem",
                    label,
                    path: path.into(),
                    line,
                });
            }
        }
        if let Some(text) = script["text"].as_str().filter(|s| !s.is_empty()) {
            rows.push(Row::label("output", text.into()));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_preserve_file_groups_source_lines_and_broken_script_output() {
        let results = serde_json::json!([
            {"name":"text","outcome":"failed","problems":[
                {"path":"b.txt","line":9,"level":"error","message":"bad value"},
                {"path":"a.txt","line":2,"level":"warning","message":"old value"},
                {"path":"b.txt","message":"missing header"}
            ]},
            {"name":"metadata","outcome":"error","text":"invalid encoding on stderr"},
            {"name":"images","outcome":"skipped","reason":"no matching files"}
        ]);
        let rows = rows(&results);
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "text · Failed",
                "a.txt",
                "Line 2 · warning: old value",
                "b.txt",
                "Line 9 · error: bad value",
                "error: missing header",
                "metadata · Check broke",
                "invalid encoding on stderr",
                "images · Skipped",
                "no matching files"
            ]
        );
        assert_eq!((rows[2].path.as_str(), rows[2].line), ("a.txt", 2));
        assert_eq!((rows[5].path.as_str(), rows[5].line), ("b.txt", 0));
        assert_eq!(rows[7].kind, "output");
    }
}
