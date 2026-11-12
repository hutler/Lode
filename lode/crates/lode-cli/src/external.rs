//! External subcommands: `lode foo …` runs `lode-foo …` from `PATH` when `foo` is not a
//! built-in command, the way git and cargo do.
//!
//! The child gets the context in its environment: `LODE_SERVER`, `LODE_DEPOT`,
//! `LODE_WORKSPACE`, `LODE_TOKEN_FILE` (a path, never the token itself) and `LODE_JSON=1` with
//! `--json`. It inherits stdin, stdout and stderr, and its exit code becomes `lode`'s.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lode_workspace::db::keys;

use crate::context::Ctx;
use crate::error::{CliError, CliResult};

/// The executable for `lode-<name>` on `PATH`.
fn find(name: &str) -> Option<PathBuf> {
    let file = format!("lode-{name}");
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).find_map(|dir| {
        exts.iter()
            .map(|ext| dir.join(format!("{file}{ext}")))
            .find(|p| is_executable(p))
    })
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Runs `lode-<args[0]>` and returns its exit code.
pub fn run(ctx: &Ctx, args: &[OsString]) -> CliResult<i32> {
    let Some(name) = args.first().and_then(|a| a.to_str()) else {
        return Err(CliError::usage("no command given; see `lode --help`"));
    };
    let Some(exe) = find(name) else {
        return Err(CliError::usage(format!(
            "unknown command `{name}`; see `lode --help` (no `lode-{name}` on PATH either)"
        )));
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&args[1..]);
    let ws = ctx.workspace().ok();
    if let Some(ws) = &ws {
        cmd.env("LODE_WORKSPACE", ws.root());
        if let Ok(Some(depot)) = ws.db().config(keys::DEPOT) {
            cmd.env("LODE_DEPOT", depot);
        }
    }
    if let Ok(server) = ctx.server_url(ws.as_ref()) {
        cmd.env("LODE_SERVER", server.as_str());
        let inherited = std::env::var_os("LODE_TOKEN_FILE").filter(|f| !f.is_empty());
        if inherited.is_none() {
            let file = ctx.credentials().token_file(&server);
            if file.is_file() {
                cmd.env("LODE_TOKEN_FILE", file);
            }
        }
    }
    if ctx.g.json {
        cmd.env("LODE_JSON", "1");
    }
    let status = cmd
        .status()
        .map_err(|e| CliError::failed("external", format!("cannot run {}: {e}", exe.display())))?;
    Ok(status.code().unwrap_or(1))
}
