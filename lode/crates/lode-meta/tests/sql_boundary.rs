//! Only this crate talks to a database driver: every other server crate goes through
//! `lode_meta::Sql`, so a second database backend needs no changes outside lode-meta.
//! lode-workspace (the client's local state) and lode-ops use SQLite on their own.

use std::path::Path;

const ALLOWED: &[&str] = &["lode-meta", "lode-workspace", "lode-ops", "lode-desk"];
const DRIVERS: &[&str] = &["rusqlite", "tokio_postgres", "tokio-postgres", "postgres"];

fn scan(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            scan(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs" || x == "toml") {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            for line in text.lines() {
                let code = line.split("//").next().unwrap_or("");
                for d in DRIVERS {
                    let hit = code.match_indices(d).any(|(i, _)| {
                        let before = code[..i].chars().last();
                        let after = code[i + d.len()..].chars().next();
                        let edge = |c: Option<char>| {
                            c.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
                        };
                        edge(before) && edge(after)
                    });
                    if hit {
                        out.push(format!("{}: {}", p.display(), line.trim()));
                    }
                }
            }
        }
    }
}

#[test]
fn no_other_crate_names_a_database_driver() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut found = Vec::new();
    for e in std::fs::read_dir(crates).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ALLOWED.contains(&name.as_str()) {
            continue;
        }
        scan(&e.path(), &mut found);
    }
    assert!(
        found.is_empty(),
        "database drivers named outside lode-meta:\n{}",
        found.join("\n")
    );
}
