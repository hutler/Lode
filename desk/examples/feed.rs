//! Writes the Lode Desk update feed (`latest.json`) from a folder of release files and their
//! `.sig` signatures (T135, before in `packaging/feed.mjs`):
//!
//! ```text
//! cargo run --example feed -- DIR VERSION BASE_URL [NOTES_FILE] > latest.json
//! ```
//!
//! DIR holds files named `lode-desk-VERSION-PLATFORM.EXT` (see `update::BUNDLES`), with
//! `FILE.sig` next to each one that can update an installed app. BASE_URL is where those files
//! are downloaded from, the release's download folder. A bundle without a signature is left out.

use std::collections::BTreeMap;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, version, base, rest @ ..] = args.as_slice() else {
        eprintln!("usage: feed DIR VERSION BASE_URL [NOTES_FILE]");
        return ExitCode::from(2);
    };
    match run(dir, version, base, rest.first()) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &str, version: &str, base: &str, notes: Option<&String>) -> Result<String, String> {
    let notes = match notes {
        Some(f) => std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?,
        None => String::new(),
    };
    let names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{dir}: {e}"))?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    let mut files = BTreeMap::new();
    for name in &names {
        if name.ends_with(".sig") {
            continue;
        }
        let sig = format!("{name}.sig");
        let signature = if names.contains(&sig) {
            let path = std::path::Path::new(dir).join(&sig);
            Some(std::fs::read_to_string(&path).map_err(|e| format!("{sig}: {e}"))?)
        } else {
            None
        };
        files.insert(name.clone(), signature);
    }
    let date = time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .map_err(|e| e.to_string())?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| e.to_string())?;
    let feed = lode_desk::update::write_feed(version, base, notes.trim(), &date, &files)?;
    serde_json::to_string_pretty(&feed).map_err(|e| e.to_string())
}
