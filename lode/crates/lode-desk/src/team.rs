//! Locks, shelves and checks for the Slint window (T133): the rows of the locks and shelves
//! views, one shelf's details and diff, and the badges and lists of check results. The same
//! rules as the Tauri window's `LocksView`, `ShelvesView`, `Checks` and `checks.ts`.

use serde_json::Value;
use slint::SharedString;

use crate::app::{CheckBadgeInfo, CheckRow, DiffRow, LockRow, ShelfDetail, ShelfFileRow, ShelfRow};
use crate::diffs::{self, Layout};
use crate::history::{format_date, summary};

fn text(v: &Value) -> SharedString {
    v.as_str().unwrap_or_default().into()
}

fn int(v: &Value) -> i32 {
    v.as_u64().and_then(|n| i32::try_from(n).ok()).unwrap_or(0)
}

// --- Checks --------------------------------------------------------------------------------

/// How bad each state is; a badge shows the worst.
fn rank(state: &str) -> u8 {
    match state {
        "success" => 0,
        "pending" => 1,
        "cancelled" => 2,
        "error" => 3,
        "failure" => 4,
        _ => 1,
    }
}

fn word(state: &str) -> &'static str {
    match state {
        "success" => "passed",
        "pending" => "pending",
        "cancelled" => "cancelled",
        "error" => "error",
        "failure" => "failed",
        _ => "reported",
    }
}

fn mark(state: &str) -> &'static str {
    match state {
        "success" => "✓",
        "cancelled" => "⊘",
        "error" => "!",
        "failure" => "✕",
        _ => "●",
    }
}

/// One badge for the checks of a revision or shelf: the worst state, with a count. Hidden
/// when there are none.
pub fn badge(checks: &[Value]) -> CheckBadgeInfo {
    if checks.is_empty() {
        return CheckBadgeInfo::default();
    }
    let state = checks
        .iter()
        .map(|c| c["state"].as_str().unwrap_or("pending"))
        .max_by_key(|s| rank(s))
        .unwrap_or("pending");
    let total = checks.len();
    let count = checks
        .iter()
        .filter(|c| c["state"].as_str() == Some(state))
        .count();
    let s = if total == 1 { "" } else { "s" };
    CheckBadgeInfo {
        shown: true,
        state: state.into(),
        text: if state == "success" {
            format!("{} {total}", mark(state))
        } else {
            format!("{} {count}/{total}", mark(state))
        }
        .into(),
        label: if state == "success" {
            format!("{total} check{s} passed")
        } else {
            format!("{count} of {total} check{s} {}", word(state))
        }
        .into(),
    }
}

/// Each check, by name.
pub fn check_rows(checks: &[Value]) -> Vec<CheckRow> {
    let mut rows: Vec<CheckRow> = checks
        .iter()
        .map(|c| {
            let state = c["state"].as_str().unwrap_or("pending");
            let r = &c["reporter"];
            let by = match (r["kind"].as_str(), r["owner"].as_str()) {
                (Some("agent"), Some(o)) => format!("{} for {o}", r["name"].as_str().unwrap_or("")),
                _ => r["name"].as_str().unwrap_or("").to_owned(),
            };
            CheckRow {
                name: text(&c["name"]),
                state: state.into(),
                word: word(state).into(),
                mark: mark(state).into(),
                summary: text(&c["summary"]),
                url: text(&c["url"]),
                by: format!(
                    "{by}, {}",
                    format_date(c["updated_at"].as_str().unwrap_or(""))
                )
                .into(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// The checks `lode checks` reported for `target` in an answer of `Desk::checks`.
pub fn checks_of<'a>(answer: &'a Value, target: &str) -> &'a [Value] {
    answer["checks"][target]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Why a missing check holds a submit back, one line per check.
pub fn missing_checks(details: &Value) -> String {
    details["missing"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .map(|m| {
                    let name = m["name"].as_str().unwrap_or("");
                    let reporter = m["reporter"].as_str().unwrap_or("");
                    let state = m["state"].as_str();
                    let why = match m["reason"].as_str() {
                        Some("absent") => format!("not reported yet (expected from {reporter})"),
                        Some("wrong_reporter") => format!(
                            "{}, but by {}, not {reporter}",
                            state.unwrap_or("reported"),
                            m["reported_by"].as_str().unwrap_or("someone else")
                        ),
                        _ => state.unwrap_or("not passed").to_owned(),
                    };
                    format!("{name}: {why}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

// --- Locks ---------------------------------------------------------------------------------

/// The rows of `lode locks --json`: mine first, then by path, those matching `filter` (part
/// of the path, any case) and, with `mine_only`, mine alone.
pub fn lock_rows(locks: &[Value], filter: &str, mine_only: bool) -> Vec<LockRow> {
    let needle = filter.trim().to_lowercase();
    let mut shown: Vec<&Value> = locks
        .iter()
        .filter(|l| !mine_only || l["mine"].as_bool() == Some(true))
        .filter(|l| {
            needle.is_empty()
                || l["path"]
                    .as_str()
                    .is_some_and(|p| p.to_lowercase().contains(&needle))
        })
        .collect();
    shown.sort_by(|a, b| {
        let mine = |l: &Value| l["mine"].as_bool().unwrap_or(false);
        mine(b)
            .cmp(&mine(a))
            .then_with(|| a["path"].as_str().cmp(&b["path"].as_str()))
    });
    shown
        .into_iter()
        .map(|l| LockRow {
            path: text(&l["path"]),
            owner: text(&l["owner"]["name"]),
            agent: l["owner"]["kind"].as_str() == Some("agent"),
            mine: l["mine"].as_bool().unwrap_or(false),
            workspace: text(&l["workspace"]),
            since: format_date(l["created_at"].as_str().unwrap_or("")).into(),
            comment: text(&l["comment"]),
        })
        .collect()
}

/// Who holds the files a lock refusal names, one line each.
pub fn holders(locks: &[Value], named: &[String]) -> String {
    locks
        .iter()
        .filter(|l| {
            l["path"]
                .as_str()
                .is_some_and(|p| named.iter().any(|n| n == p))
        })
        .map(|l| {
            let mut line = format!(
                "{} — {}{}, since {}",
                l["path"].as_str().unwrap_or(""),
                l["owner"]["name"].as_str().unwrap_or(""),
                if l["owner"]["kind"].as_str() == Some("agent") {
                    " (agent)"
                } else {
                    ""
                },
                format_date(l["created_at"].as_str().unwrap_or(""))
            );
            if let Some(c) = l["comment"].as_str() {
                line.push_str(": ");
                line.push_str(c);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// --- Shelves -------------------------------------------------------------------------------

fn owner_text(s: &Value) -> String {
    let name = s["owner"]["name"].as_str().unwrap_or("");
    match (s["owner"]["kind"].as_str(), s["agent_owner"].as_str()) {
        (Some("agent"), Some(o)) => format!("{name} for {o}"),
        _ => name.to_owned(),
    }
}

/// The checks target of a shelf version.
pub fn shelf_target(s: &Value) -> String {
    format!(
        "{}@{}",
        s["id"].as_u64().unwrap_or(0),
        s["version"].as_u64().unwrap_or(0)
    )
}

/// The rows of `lode shelves --json`; `me` is the signed-in user, `checks` an answer of
/// `Desk::checks` (or null).
pub fn shelf_rows(
    shelves: &[Value],
    me: &str,
    selected: Option<u64>,
    checks: &Value,
) -> Vec<ShelfRow> {
    shelves
        .iter()
        .map(|s| {
            let you = !me.is_empty() && s["owner"]["name"].as_str() == Some(me);
            ShelfRow {
                id: int(&s["id"]),
                label: format!("#{}", s["id"].as_u64().unwrap_or(0)).into(),
                title: summary(s["description"].as_str().unwrap_or("")).into(),
                owner: if you {
                    "you".into()
                } else {
                    owner_text(s).into()
                },
                you,
                agent: s["owner"]["kind"].as_str() == Some("agent"),
                version: format!("v{}", s["version"].as_u64().unwrap_or(0)).into(),
                updated: format_date(s["updated_at"].as_str().unwrap_or("")).into(),
                checks: badge(checks_of(checks, &shelf_target(s))),
                selected: selected.is_some_and(|id| s["id"].as_u64() == Some(id)),
            }
        })
        .collect()
}

fn state_text(state: &str) -> &'static str {
    match state {
        "submitted" => "submitted",
        "deleted" => "deleted",
        _ => "open",
    }
}

/// One shelf (`lode shelf diff --json`): its details and changed files.
pub fn shelf_detail(s: &Value, me: &str) -> (ShelfDetail, Vec<ShelfFileRow>) {
    let description = s["description"].as_str().unwrap_or("");
    let body = description
        .split_once('\n')
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    let state = s["state"].as_str().unwrap_or("open");
    let merges = s["merged_from"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .map(|m| {
                    format!(
                        "{} r{}..r{}",
                        m["branch"].as_str().unwrap_or(""),
                        m["from_rev"].as_u64().unwrap_or(0),
                        m["to_rev"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let files = s["changes"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    let (letter, status) = match c["action"].as_str() {
                        Some("add") => ("A", "added"),
                        Some("delete") => ("D", "deleted"),
                        _ => ("M", "modified"),
                    };
                    ShelfFileRow {
                        path: text(&c["path"]),
                        letter: letter.into(),
                        status: status.into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let owner = owner_text(s);
    let d = ShelfDetail {
        shown: true,
        id: int(&s["id"]),
        version: int(&s["version"]),
        title: summary(description).into(),
        body: body.into(),
        subtitle: format!(
            "Shelf {} · version {} · {}",
            s["id"].as_u64().unwrap_or(0),
            s["version"].as_u64().unwrap_or(0),
            state_text(state)
        )
        .into(),
        owner: if s["owner"]["kind"].as_str() == Some("agent") {
            format!("{owner} (agent)").into()
        } else {
            owner.into()
        },
        branch: format!(
            "{} at r{}",
            s["branch"].as_str().unwrap_or(""),
            s["base_rev"].as_u64().unwrap_or(0)
        )
        .into(),
        updated: format_date(s["updated_at"].as_str().unwrap_or("")).into(),
        submitted: s["submitted_rev"]
            .as_u64()
            .map(|r| format!("as r{r}"))
            .unwrap_or_default()
            .into(),
        merges: merges.into(),
        open: state == "open",
        mine: !me.is_empty() && s["owner"]["name"].as_str() == Some(me),
        deleted: state == "deleted",
        busy: SharedString::new(),
    };
    (d, files)
}

/// The rows of a shelf's diff: each file's name, then its hunks in `layout`, or why it has no
/// diff.
pub fn shelf_diff_rows(s: &Value, layout: Layout) -> Vec<DiffRow> {
    let mut out = Vec::new();
    for d in s["diff"].as_array().into_iter().flatten() {
        let path = d["path"].as_str().unwrap_or("");
        out.push(DiffRow {
            kind: "file".into(),
            header: path.into(),
            ..DiffRow::default()
        });
        match d["patch"].as_str() {
            Some(patch) if patch.len() > diffs::PLAIN_LIMIT => out.extend(diffs::plain_rows(patch)),
            Some(patch) => out.extend(diffs::rows(&diffs::parse_patch(patch), layout)),
            None => {
                let why = match d["skipped"].as_str() {
                    Some("too_large") => {
                        "Too large to compare as text: Lode compares files over 16 MB by size only."
                    }
                    Some("symlink") => "Symbolic link: links are not shown as a diff.",
                    _ => "Binary file: binary content is not shown as a diff.",
                };
                out.push(DiffRow {
                    kind: "plain".into(),
                    text: why.into(),
                    ..DiffRow::default()
                });
            }
        }
    }
    out
}

/// What an unshelve did: the counts in words, and one line per file.
pub fn unshelved(r: &Value) -> (String, String, bool) {
    let counts = r["counts"]
        .as_object()
        .map(|c| {
            c.iter()
                .map(|(k, n)| format!("{} {k}", n.as_u64().unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Nothing to apply.".into());
    let files = r["files"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let lines = files
        .iter()
        .map(|f| {
            format!(
                "{}  {}",
                f["outcome"].as_str().unwrap_or(""),
                f["path"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let conflicts = files
        .iter()
        .any(|f| f["outcome"].as_str() == Some("conflicted"));
    (counts, lines, conflicts)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use serde_json::json;

    use super::*;

    #[test]
    fn badges_show_the_worst_state() {
        assert!(!badge(&[]).shown);
        let ok = badge(&[json!({"state": "success"}), json!({"state": "success"})]);
        assert_eq!((ok.state.as_str(), ok.text.as_str()), ("success", "✓ 2"));
        assert_eq!(ok.label, "2 checks passed");
        let bad = badge(&[
            json!({"state": "success"}),
            json!({"state": "failure"}),
            json!({"state": "pending"}),
        ]);
        assert_eq!(bad.state, "failure");
        assert_eq!(bad.text, "✕ 1/3");
        assert_eq!(bad.label, "1 of 3 checks failed");
    }

    #[test]
    fn check_rows_by_name() {
        let rows = check_rows(&[
            json!({"name": "ci/test", "state": "failure", "summary": "2 failed",
                   "reporter": {"name": "bot", "kind": "agent", "owner": "ana"}, "updated_at": "2026-11-14T10:00:00Z"}),
            json!({"name": "ci/build", "state": "success", "reporter": {"name": "ci", "kind": "user"}, "updated_at": "x"}),
        ]);
        assert_eq!(rows[0].name, "ci/build");
        assert_eq!(rows[1].word, "failed");
        assert_eq!(rows[1].by, "bot for ana, 2026-11-14 10:00 UTC");
        assert_eq!(rows[1].summary, "2 failed");
    }

    #[test]
    fn missing_checks_in_words() {
        let d = json!({"missing": [
            {"name": "ci/build", "reporter": "ci", "reason": "absent"},
            {"name": "ci/test", "reporter": "ci", "reason": "not_success", "state": "failure"},
            {"name": "ci/lint", "reporter": "ci", "reason": "wrong_reporter", "state": "success", "reported_by": "bob"},
        ]});
        assert_eq!(
            missing_checks(&d),
            "ci/build: not reported yet (expected from ci)\nci/test: failure\nci/lint: success, but by bob, not ci"
        );
    }

    #[test]
    fn locks_mine_first_filtered() {
        let locks = [
            json!({"path": "b.txt", "owner": {"name": "bob", "kind": "user"}, "mine": false, "workspace": "w2", "created_at": "x"}),
            json!({"path": "z.txt", "owner": {"name": "me", "kind": "user"}, "mine": true, "workspace": "w1", "created_at": "x", "comment": "art"}),
            json!({"path": "a.txt", "owner": {"name": "bot", "kind": "agent"}, "mine": false, "workspace": "w3", "created_at": "x"}),
        ];
        let paths =
            |rows: Vec<LockRow>| rows.iter().map(|r| r.path.to_string()).collect::<Vec<_>>();
        assert_eq!(
            paths(lock_rows(&locks, "", false)),
            ["z.txt", "a.txt", "b.txt"]
        );
        assert_eq!(paths(lock_rows(&locks, "B.T", false)), ["b.txt"]);
        assert_eq!(paths(lock_rows(&locks, "", true)), ["z.txt"]);
        assert!(lock_rows(&locks, "", false)[1].agent);
        assert_eq!(holders(&locks, &["b.txt".into()]), "b.txt — bob, since x");
    }

    #[test]
    fn shelves_and_their_details() {
        let s = json!({
            "id": 3, "version": 2, "branch": "main", "base_rev": 9,
            "owner": {"name": "alice", "kind": "user"},
            "description": "Apricot\n\nFor the orchard level", "state": "open",
            "created_at": "x", "updated_at": "2026-11-14T10:00:00Z",
            "changes": [{"path": "a.txt", "action": "modify"}, {"path": "n.png", "action": "add"}],
            "diff": [
                {"path": "a.txt", "patch": "@@ -1 +1 @@\n-apple\n+apricot\n"},
                {"path": "n.png", "skipped": "binary"},
            ],
        });
        let checks = json!({"available": true, "checks": {"3@2": [{"state": "failure"}]}});
        let rows = shelf_rows(std::slice::from_ref(&s), "alice", Some(3), &checks);
        assert_eq!(rows[0].label, "#3");
        assert_eq!(rows[0].owner, "you");
        assert!(rows[0].selected);
        assert_eq!(rows[0].checks.state, "failure");
        let (d, files) = shelf_detail(&s, "alice");
        assert_eq!(d.title, "Apricot");
        assert_eq!(d.body, "For the orchard level");
        assert_eq!(d.subtitle, "Shelf 3 · version 2 · open");
        assert_eq!(d.branch, "main at r9");
        assert!(d.open && d.mine);
        assert_eq!(files[1].letter, "A");
        let diff = shelf_diff_rows(&s, Layout::Unified);
        assert_eq!(diff[0].kind, "file");
        assert!(diff.iter().any(|r| r.kind == "add" && r.text == "apricot"));
        assert!(diff.last().unwrap().text.starts_with("Binary file"));
    }

    #[test]
    fn unshelve_outcomes() {
        let (counts, lines, conflicts) = unshelved(&json!({
            "shelf": 3, "version": 1,
            "files": [{"path": "a.txt", "outcome": "applied"}, {"path": "b.txt", "outcome": "conflicted"}],
            "counts": {"applied": 1, "conflicted": 1},
        }));
        assert_eq!(counts, "1 applied, 1 conflicted");
        assert_eq!(lines, "applied  a.txt\nconflicted  b.txt");
        assert!(conflicts);
    }

    #[test]
    fn holders_name_only_the_refused_files() {
        let locks = [
            json!({"path": "door.txt", "owner": {"name": "bob", "kind": "user"},
                   "created_at": "2026-11-14T08:15:00Z", "comment": "door art"}),
            json!({"path": "tide.txt", "owner": {"name": "bot", "kind": "agent"},
                   "created_at": "2026-11-14T08:20:00Z"}),
            json!({"path": "free.txt", "owner": {"name": "ana", "kind": "user"}}),
        ];
        let named = ["door.txt".to_owned(), "tide.txt".to_owned()];
        assert_eq!(
            holders(&locks, &named),
            "door.txt — bob, since 2026-11-14 08:15 UTC: door art\ntide.txt — bot (agent), since 2026-11-14 08:20 UTC"
        );
    }

    #[test]
    fn shelf_diff_has_a_header_per_file_and_notes_for_binaries() {
        let shelf = json!({"diff": [
            {"path": "a.txt", "patch": "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-apple\n+apricot\n"},
            {"path": "pic.png", "skipped": "binary"},
        ]});
        let rows = shelf_diff_rows(&shelf, Layout::Unified);
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds.first(), Some(&"file"));
        assert_eq!(rows[0].header, "a.txt");
        assert!(rows.iter().any(|r| r.kind == "add" && r.text == "apricot"));
        let pic = rows.iter().position(|r| r.header == "pic.png").unwrap();
        assert_eq!(rows[pic + 1].kind, "plain");
        assert!(rows[pic + 1].text.starts_with("Binary file"));
    }
}
