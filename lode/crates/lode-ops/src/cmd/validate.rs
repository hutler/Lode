//! Validation scripts on the client (T202): `lode validate`, and the run before `lode shelve`
//! and `lode commit` (SPEC §6.8).
//!
//! The scripts and the rule list come from the workspace's copy of the depot, so they reach
//! everyone through `lode update`. Running a script runs code on this computer, so a script
//! runs only once the person trusts it: per depot and script content, asked once in a
//! terminal, or with `lode validate --trust`, or for a whole depot in `settings.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use lode_core::{Change, LodeConfig, ValidateScript};
use lode_validate::{
    default_candidates, describe, find_python, input_files, matching, FindError, Input, Level,
    Outcome, Python, ScriptResult,
};
use lode_workspace::db::keys;
use lode_workspace::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{Exit, OpError, OpResult};
use crate::local;

/// `[validate]` in `settings.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateSettings {
    /// The Python to use: a program, or a program and its first arguments
    /// (`["py", "-3.12"]`). `LODE_PYTHON` overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSetting>,
    /// `warn` (default): without a suitable Python, shelve and commit go on with a warning,
    /// since the server checks again. `refuse`: they stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_python: Option<String>,
    /// Depots (`<server>/<depot>`) whose scripts always run, such as on a build machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust: Vec<String>,
    /// Scripts trusted so far, per depot: `<path>@<content id>`. Written by Lode.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub trusted: BTreeMap<String, Vec<String>>,
}

impl ValidateSettings {
    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A Python as `settings.toml` names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PythonSetting {
    /// One program.
    Program(String),
    /// A program and its first arguments.
    Command(Vec<String>),
}

/// `lode validate` options.
#[derive(Debug, Default, clap::Args)]
#[command(long_about = "Run the depot's validation scripts on your changes now.
\nA depot lists Python scripts in `.lodeconfig` under [validate]; `lode shelve` and
`lode commit` run them first and stop when one fails (skip that with --no-verify; the server
checks shelves again). A script runs only after you trust it: Lode asks once per script
version in a terminal, or pass --trust.
\nPython: LODE_PYTHON, then `[validate] python` in settings.toml, then `py -3`, `python`,
`python3` on Windows and `python3`, `python` elsewhere.
\nWith --json: {\"passed\":bool,\"results\":[{\"name\",\"outcome\":\"passed|failed|error|skipped\",
\"reason\"?,\"problems\":[{\"path\"?,\"line\"?,\"level\",\"message\"}],\"text\"?,\"files\",\"seconds\"}]}.
A failure exits 1 with code `validation_failed` and the same results in `details`.")]
pub struct ValidateArgs {
    /// Only changes to these files and directories.
    pub paths: Vec<PathBuf>,
    /// Only the files of this changelist.
    #[arg(short = 'c', long = "changelist")]
    pub changelist: Option<String>,
    /// Only this script.
    #[arg(long, value_name = "NAME")]
    pub script: Option<String>,
    /// Trust this depot's current scripts, so they run from now on.
    #[arg(long)]
    pub trust: bool,
}

/// Why a run happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before `lode shelve`.
    Shelve,
    /// Before `lode commit`.
    Commit,
    /// `lode validate`.
    Manual,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Shelve => "shelve",
            Phase::Commit => "commit",
            Phase::Manual => "manual",
        }
    }
}

/// `<server>/<depot>`, the key of trust settings.
fn depot_key(ws: &Workspace) -> OpResult<String> {
    let server = ws.db().config(keys::SERVER_URL)?.unwrap_or_default();
    let depot = ws.db().config(keys::DEPOT)?.unwrap_or_default();
    Ok(format!("{}/{depot}", server.trim_end_matches('/')))
}

/// `<path>@<content id>` of a script in the workspace, if it is there.
fn fingerprint(ws: &Workspace, s: &ValidateScript) -> Option<String> {
    let file = std::fs::File::open(ws.local_path(&s.run)).ok()?;
    let id = lode_core::blob_id(file).ok()?;
    Some(format!("{}@{id}", s.run))
}

fn python_candidates(settings: &ValidateSettings) -> Vec<Vec<String>> {
    if let Some(p) = std::env::var_os("LODE_PYTHON").filter(|p| !p.is_empty()) {
        return vec![vec![p.to_string_lossy().into_owned()]];
    }
    match &settings.python {
        Some(PythonSetting::Program(p)) => vec![vec![p.clone()]],
        Some(PythonSetting::Command(c)) if !c.is_empty() => vec![c.clone()],
        _ => default_candidates(),
    }
}

fn untrusted_error(key: &str, scripts: &[&ValidateScript]) -> OpError {
    let names: Vec<String> = scripts.iter().map(|s| s.run.to_string()).collect();
    OpError::failed(
        "validation_untrusted",
        format!(
            "this depot wants to run {} before shelve and commit, and you have not trusted {} yet: \
             read {}, then run `lode validate --trust` (or pass --no-verify to go on without {})",
            names.join(", "),
            if names.len() == 1 { "it" } else { "them" },
            if names.len() == 1 { "it" } else { "them" },
            if names.len() == 1 { "it" } else { "them" },
        ),
    )
    .with_details(json!({ "depot": key, "scripts": names }))
}

/// Makes sure every script in `run` is trusted: from settings, by asking, or with `trust`.
/// Saying yes trusts every script the depot lists (`all`), not only the ones about to run.
fn ensure_trusted(
    ctx: &Ctx,
    ws: &Workspace,
    all: &[ValidateScript],
    run: &[&ValidateScript],
    trust: bool,
) -> OpResult<()> {
    let key = depot_key(ws)?;
    let mut settings = ctx.settings()?;
    if settings.validate.trust.iter().any(|d| d == &key) {
        return Ok(());
    }
    let known = settings
        .validate
        .trusted
        .get(&key)
        .cloned()
        .unwrap_or_default();
    let mut untrusted = Vec::new();
    for s in run {
        // A missing script cannot run, and fails on its own.
        let Some(fp) = fingerprint(ws, s) else {
            continue;
        };
        if !known.contains(&fp) {
            untrusted.push(*s);
        }
    }
    if untrusted.is_empty()
        && !(trust
            && all
                .iter()
                .any(|s| fingerprint(ws, s).is_some_and(|f| !known.contains(&f))))
    {
        return Ok(());
    }
    let ok = trust || {
        let ui = ctx.out.ui();
        if ctx.g.json || !ui.interactive() {
            false
        } else {
            let list: Vec<String> = untrusted.iter().map(|s| s.run.to_string()).collect();
            ui.confirm(&format!(
                "This depot wants to run {} on this computer before shelve and commit. Run {}?",
                list.join(", "),
                if list.len() == 1 { "it" } else { "them" }
            ))?
        }
    };
    if !ok {
        return Err(untrusted_error(&key, &untrusted));
    }
    // Keep only the current version of each script, so the list does not grow forever.
    let entry = settings.validate.trusted.entry(key).or_default();
    for fp in all.iter().filter_map(|s| fingerprint(ws, s)) {
        let path = fp
            .rsplit_once('@')
            .map(|(p, _)| p.to_owned())
            .unwrap_or_default();
        entry.retain(|e| e.rsplit_once('@').map(|(p, _)| p) != Some(path.as_str()));
        entry.push(fp);
    }
    entry.sort();
    ctx.save_settings(&settings)
}

/// What a run found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// One result per configured script (skipped ones included).
    pub results: Vec<ScriptResult>,
}

impl Report {
    /// Whether nothing stops a shelve or commit.
    pub fn passed(&self) -> bool {
        !self.results.iter().any(|r| r.outcome.refuses())
    }

    /// The results as JSON.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(&self.results).unwrap_or(Value::Null)
    }

    /// Problems grouped by file, for people.
    pub fn human(&self) -> String {
        let mut out = String::new();
        for r in &self.results {
            match r.outcome {
                Outcome::Passed | Outcome::Skipped => continue,
                Outcome::Failed => out.push_str(&format!("{} failed:\n", r.name)),
                Outcome::Error => out.push_str(&format!(
                    "{}: the check itself broke ({}):\n",
                    r.name,
                    r.reason.as_deref().unwrap_or("?")
                )),
            }
            let mut by_file: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            for p in &r.problems {
                let line = match p.line {
                    Some(l) => format!("line {l}: {}", p.message),
                    None => p.message.clone(),
                };
                let line = if p.level == Level::Warning {
                    format!("warning: {line}")
                } else {
                    line
                };
                by_file
                    .entry(p.path.as_deref().unwrap_or(""))
                    .or_default()
                    .push(line);
            }
            for (file, lines) in by_file {
                if file.is_empty() {
                    for l in lines {
                        out.push_str(&format!("  {l}\n"));
                    }
                } else {
                    out.push_str(&format!("  {file}\n"));
                    for l in lines {
                        out.push_str(&format!("    {l}\n"));
                    }
                }
            }
            if let Some(t) = &r.text {
                for l in t.lines() {
                    out.push_str(&format!("  | {l}\n"));
                }
            }
        }
        out.trim_end().to_owned()
    }
}

fn failed(report: &Report, phase: Phase) -> OpError {
    let what = match phase {
        Phase::Shelve => "nothing was shelved; fix these and shelve again, or pass --no-verify (the server checks shelves again)",
        Phase::Commit => "nothing was committed; fix these and commit again, or pass --no-verify",
        Phase::Manual => "fix these before you shelve or commit",
    };
    OpError::new(
        Exit::Failed,
        "validation_failed",
        format!("validation failed:\n{}\n{what}", report.human()),
    )
    .with_details(json!({ "results": report.to_json() }))
}

/// Runs the scripts that `changes` touch. `only` limits the run to one script.
async fn run_scripts(
    ctx: &Ctx,
    ws: &Workspace,
    config: &LodeConfig,
    phase: Phase,
    changes: &[Change],
    only: Option<&str>,
    trust: bool,
) -> OpResult<Report> {
    let files = input_files(changes);
    let scripts: Vec<&ValidateScript> = config
        .validate
        .scripts
        .iter()
        .filter(|s| only.is_none_or(|n| s.name == n))
        .collect();
    if let Some(n) = only {
        if scripts.is_empty() {
            return Err(OpError::usage(format!(
                "this depot has no validation script named `{n}`"
            )));
        }
    }
    let mut report = Report::default();
    let mut todo = Vec::new();
    for s in scripts {
        let m = matching(&files, &s.paths);
        if m.is_empty() {
            report
                .results
                .push(ScriptResult::skipped(&s.name, "no matching files"));
        } else {
            todo.push((s, m));
        }
    }
    if todo.is_empty() {
        if trust {
            ensure_trusted(ctx, ws, &config.validate.scripts, &[], true)?;
        }
        return Ok(report);
    }
    let run: Vec<&ValidateScript> = todo.iter().map(|(s, _)| *s).collect();
    ensure_trusted(ctx, ws, &config.validate.scripts, &run, trust)?;

    let settings = ctx.settings()?.validate;
    let python = match find_python(&python_candidates(&settings), config.validate.python).await {
        Ok(p) => p,
        Err(e) => return python_missing(ctx, report, &todo, config, &settings, e),
    };
    let depot = ws.db().config(keys::DEPOT)?.unwrap_or_default();
    let branch = ws.db().config(keys::BRANCH)?.unwrap_or_default();
    for (s, files) in todo {
        let n = files.len();
        ctx.out.note(format!(
            "Checking {} ({n} file{})...",
            s.name,
            if n == 1 { "" } else { "s" }
        ));
        let input = Input {
            version: lode_validate::INPUT_VERSION,
            phase: phase.as_str().to_owned(),
            depot: depot.clone(),
            branch: branch.clone(),
            files,
        };
        let r = lode_validate::run(
            &python,
            &s.name,
            ws.root(),
            &ws.local_path(&s.run),
            &input,
            Duration::from_secs(u64::from(s.timeout_secs)),
        )
        .await;
        report.results.push(r);
    }
    Ok(report)
}

fn python_missing(
    ctx: &Ctx,
    mut report: Report,
    todo: &[(&ValidateScript, Vec<lode_validate::InputFile>)],
    config: &LodeConfig,
    settings: &ValidateSettings,
    e: FindError,
) -> OpResult<Report> {
    let need = config
        .validate
        .python_text()
        .map(|v| format!("Python {v} or newer"))
        .unwrap_or_else(|| "Python 3".to_owned());
    let found = match &e {
        FindError::Missing { tried } => format!("none found (tried {})", tried.join(", ")),
        FindError::TooOld { found } => format!(
            "{} is {}",
            found.command.join(" "),
            Python::version_text(found)
        ),
    };
    let msg = format!(
        "this depot's validation scripts need {need}: {found}. Install it, or point Lode at one with \
         LODE_PYTHON or `[validate] python = \"...\"` in settings.toml"
    );
    if settings.missing_python.as_deref() == Some("refuse") {
        return Err(OpError::failed("python_missing", msg)
            .with_details(json!({ "need": need, "found": found })));
    }
    ctx.out.warn(format!(
        "{msg}; going on without them (the server checks shelves again)"
    ));
    for (s, _) in todo {
        report
            .results
            .push(ScriptResult::skipped(&s.name, format!("needs {need}")));
    }
    Ok(report)
}

/// The check before `lode shelve` and `lode commit`: runs the scripts the changes touch and
/// refuses when one fails. Answers the results to report, if any script was configured.
pub(crate) async fn before_send(
    ctx: &Ctx,
    ws: &Workspace,
    phase: Phase,
    changes: &[Change],
    no_verify: bool,
) -> OpResult<Option<Value>> {
    let Some(config) = crate::cmd::update::local_config(ws) else {
        return Ok(None);
    };
    if config.validate.is_empty() {
        return Ok(None);
    }
    if no_verify {
        ctx.out.note("Skipping validation (--no-verify).");
        return Ok(None);
    }
    let report = run_scripts(ctx, ws, &config, phase, changes, None, false).await?;
    if !report.passed() {
        return Err(failed(&report, phase));
    }
    let ran: Vec<&str> = report
        .results
        .iter()
        .filter(|r| r.outcome == Outcome::Passed)
        .map(|r| r.name.as_str())
        .collect();
    if !ran.is_empty() {
        ctx.out
            .note(format!("Validation passed: {}.", ran.join(", ")));
    }
    for r in &report.results {
        for p in r.problems.iter().filter(|p| p.level == Level::Warning) {
            ctx.out.warn(format!("{}: {}", r.name, describe(p)));
        }
    }
    Ok(Some(report.to_json()))
}

/// Runs `lode validate`.
pub async fn validate(ctx: &Ctx, args: ValidateArgs) -> OpResult<Value> {
    let mut ws = ctx.workspace()?;
    let _lock = ws.lock()?;
    let config = crate::cmd::update::local_config(&ws).unwrap_or_default();
    if config.validate.is_empty() {
        ctx.out
            .line("This depot has no validation scripts ([validate] in .lodeconfig).");
        return Ok(json!({ "passed": true, "results": [] }));
    }
    let paths = local::depot_paths(ctx, &ws, &args.paths)?;
    let list = args
        .changelist
        .as_deref()
        .map(|n| local::changelist(&ws, n))
        .transpose()?;
    let list_id = list.as_ref().map(|l| l.as_ref().map(|c| c.id));
    let (out, _) = crate::cmd::commit::gather(ctx, &mut ws, &paths, list_id)?;
    let changes: Vec<Change> = out.iter().map(|o| o.change.clone()).collect();
    let report = run_scripts(
        ctx,
        &ws,
        &config,
        Phase::Manual,
        &changes,
        args.script.as_deref(),
        args.trust,
    )
    .await?;
    if !report.passed() {
        return Err(failed(&report, Phase::Manual));
    }
    for r in &report.results {
        let line = match r.outcome {
            Outcome::Skipped => format!(
                "{}: skipped ({})",
                r.name,
                r.reason.as_deref().unwrap_or("")
            ),
            _ => format!(
                "{}: passed ({} file{})",
                r.name,
                r.files,
                if r.files == 1 { "" } else { "s" }
            ),
        };
        ctx.out.line(line);
        for p in r.problems.iter().filter(|p| p.level == Level::Warning) {
            ctx.out.line(format!("  warning: {}", describe(p)));
        }
    }
    Ok(json!({ "passed": true, "results": report.to_json() }))
}
