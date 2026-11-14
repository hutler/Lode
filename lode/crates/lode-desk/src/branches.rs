//! Branches, switches, merges and the view for the Slint window (T134): the branch list's rows,
//! revision fields, and what an update, switch or merge did.

use serde_json::Value;
use slint::SharedString;

use crate::app::{BranchRow, OutcomeRow};

/// What a revision field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevField {
    /// Nothing: the head.
    Blank,
    Rev(u64),
    /// Neither a number nor `rN`.
    Bad,
}

/// `r1390` or `1390` as a revision.
pub fn parse_rev(text: &str) -> RevField {
    let t = text.trim();
    let t = t
        .strip_prefix('r')
        .or_else(|| t.strip_prefix('R'))
        .unwrap_or(t);
    if t.is_empty() {
        return RevField::Blank;
    }
    if !t.bytes().all(|b| b.is_ascii_digit()) {
        return RevField::Bad;
    }
    t.parse().map_or(RevField::Bad, RevField::Rev)
}

/// Whether `field` names a revision a branch with head `head` has (blank is its head).
pub fn rev_ok(field: RevField, head: u64) -> bool {
    match field {
        RevField::Blank => true,
        RevField::Rev(r) => r >= 1 && r <= head,
        RevField::Bad => false,
    }
}

/// Where a branch started.
pub fn origin(b: &Value) -> String {
    match (b["parent"]["branch"].as_str(), b["parent"]["rev"].as_u64()) {
        (Some(p), Some(r)) => format!("from {p}@r{r}"),
        _ => "first branch".into(),
    }
}

/// The branch list's rows: filtered by name, the current branch first, then active ones by
/// name, archived ones last.
pub fn branch_rows(
    branches: &[Value],
    current: &str,
    filter: &str,
    selected: &str,
) -> Vec<BranchRow> {
    let f = filter.trim().to_lowercase();
    let mut shown: Vec<&Value> = branches
        .iter()
        .filter(|b| {
            f.is_empty()
                || b["name"]
                    .as_str()
                    .is_some_and(|n| n.to_lowercase().contains(&f))
        })
        .collect();
    let name = |b: &Value| b["name"].as_str().unwrap_or("").to_owned();
    let archived = |b: &Value| b["state"].as_str() == Some("archived");
    shown.sort_by(|a, b| {
        (name(b) == current)
            .cmp(&(name(a) == current))
            .then_with(|| archived(a).cmp(&archived(b)))
            .then_with(|| name(a).cmp(&name(b)))
    });
    shown
        .into_iter()
        .map(|b| BranchRow {
            name: name(b).into(),
            current: name(b) == current,
            archived: archived(b),
            head: format!("r{}", b["head_rev"].as_u64().unwrap_or(0)).into(),
            origin: origin(b).into(),
            selected: name(b) == selected,
        })
        .collect()
}

/// `n` `one`s, with an `s` unless it is one.
pub fn count(n: u64, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// `counts` as "1 conflicted, 2 updated", by kind.
pub fn counts_text(counts: &Value) -> String {
    counts
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, n)| format!("{} {k}", n.as_u64().unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// The files of an update, switch or merge, at most `limit` of them, and how many more.
pub fn outcome_rows(r: &Value, limit: usize) -> (Vec<OutcomeRow>, usize) {
    let files = r["files"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let rows = files
        .iter()
        .take(limit)
        .map(|f| OutcomeRow {
            outcome: SharedString::from(f["outcome"].as_str().unwrap_or("")),
            path: SharedString::from(f["path"].as_str().unwrap_or("")),
        })
        .collect();
    (rows, files.len().saturating_sub(limit))
}

fn conflicted(r: &Value) -> usize {
    r["files"]
        .as_array()
        .map(|f| f.iter().filter(|f| f["outcome"] == "conflicted").count())
        .unwrap_or(0)
}

/// What an update or switch did, in words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub title: String,
    pub text: String,
    pub conflicts: String,
    pub notes: String,
    pub done: bool,
}

/// An update's or a switch's result.
pub fn sync_outcome(r: &Value) -> Outcome {
    let branch = r["branch"].as_str().unwrap_or("");
    let rev = r["rev"].as_u64().unwrap_or(0);
    let counts = counts_text(&r["counts"]);
    let counts = if counts.is_empty() {
        "No files changed.".to_owned()
    } else {
        format!("{counts}.")
    };
    let (title, text) = match r["switched_from"].as_str() {
        Some(from) => (
            format!("Switched to {branch}"),
            format!("The workspace is on {branch} at r{rev}, from {from}. {counts}"),
        ),
        None => (
            format!("Updated to r{rev}"),
            format!(
                "The workspace is at r{rev}, from r{}. {counts}",
                r["from"].as_u64().unwrap_or(0)
            ),
        ),
    };
    let n = conflicted(r);
    let conflicts = match n {
        0 => String::new(),
        _ => format!(
            "{} did not merge with your local changes. They are marked in Changes; resolve them before you commit.",
            if n == 1 { "1 file".to_owned() } else { format!("{n} files") }
        ),
    };
    let left: Vec<&str> = r["locks_left"]
        .as_array()
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let notes = if left.is_empty() {
        String::new()
    } else {
        format!(
            "Your locks stay on {}. Locks belong to a branch, so these are still held there; unlock them from that branch when you are done:\n{}",
            r["switched_from"].as_str().unwrap_or(""),
            left.join("\n")
        )
    };
    Outcome {
        title,
        text,
        conflicts,
        notes,
        done: true,
    }
}

/// A merge's result.
pub fn merge_outcome(r: &Value) -> Outcome {
    let source = r["source"].as_str().unwrap_or("");
    if r["nothing_to_do"].as_bool() == Some(true) {
        return Outcome {
            title: "Nothing to merge".into(),
            text: format!("Everything on {source} is already merged here."),
            done: true,
            ..Outcome::default()
        };
    }
    let (from, to) = (
        r["from_rev"].as_u64().unwrap_or(0),
        r["to_rev"].as_u64().unwrap_or(0),
    );
    let range = if from == to {
        format!("r{to}")
    } else {
        format!("r{from}..r{to}")
    };
    let n = conflicted(r);
    let skipped = r["files"]
        .as_array()
        .is_some_and(|f| f.iter().any(|f| f["outcome"] == "skipped"));
    Outcome {
        title: format!("Merged {source} {range}"),
        text: format!(
            "{}. Your next commit records the merge.",
            counts_text(&r["counts"])
        ),
        conflicts: match n {
            0 => String::new(),
            _ => format!(
                "{} changed on both sides and did not merge. Resolve them in Changes before you commit.",
                if n == 1 { "1 file".to_owned() } else { format!("{n} files") }
            ),
        },
        notes: if skipped {
            "Skipped files are outside this workspace's view.".into()
        } else {
            String::new()
        },
        done: n == 0,
    }
}

/// Bytes as "12.0 KB".
pub fn bytes(n: u64) -> String {
    crate::app::human_bytes(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn revision_fields() {
        assert_eq!(parse_rev(" r12 "), RevField::Rev(12));
        assert_eq!(parse_rev("7"), RevField::Rev(7));
        assert_eq!(parse_rev(""), RevField::Blank);
        assert_eq!(parse_rev("r1x"), RevField::Bad);
        assert!(rev_ok(RevField::Rev(5), 5) && !rev_ok(RevField::Rev(6), 5));
        assert!(!rev_ok(RevField::Rev(0), 5) && rev_ok(RevField::Blank, 5));
    }

    #[test]
    fn branch_rows_current_first_archived_last() {
        let b = |name: &str, state: &str, parent: Value| json!({"name": name, "state": state, "head_rev": 9, "parent": parent});
        let branches = [
            b("zeta", "active", json!({"branch": "main", "rev": 3})),
            b("old", "archived", json!({"branch": "main", "rev": 2})),
            b("main", "active", Value::Null),
            b("alpha", "active", json!({"branch": "main", "rev": 4})),
        ];
        let rows = branch_rows(&branches, "zeta", "", "alpha");
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["zeta", "alpha", "main", "old"]);
        assert!(rows[0].current && rows[1].selected && rows[3].archived);
        assert_eq!(rows[2].origin, "first branch");
        assert_eq!(rows[0].origin, "from main@r3");
        assert_eq!(branch_rows(&branches, "zeta", "AL", "").len(), 1);
    }

    #[test]
    fn outcomes_in_words() {
        let s = sync_outcome(&json!({"branch": "dev", "rev": 8, "switched_from": "main",
            "counts": {"updated": 2, "conflicted": 1},
            "files": [{"path": "a", "outcome": "updated"}, {"path": "b", "outcome": "conflicted"}],
            "locks_left": ["door.txt"]}));
        assert_eq!(s.title, "Switched to dev");
        assert_eq!(
            s.text,
            "The workspace is on dev at r8, from main. 1 conflicted, 2 updated."
        );
        assert!(s.conflicts.starts_with("1 file did not merge"));
        assert!(s.notes.contains("Your locks stay on main") && s.notes.ends_with("door.txt"));
        let u = sync_outcome(
            &json!({"branch": "main", "rev": 5, "from": 3, "switched_from": null,
            "counts": {}, "files": [], "locks_left": []}),
        );
        assert_eq!(
            (u.title.as_str(), u.text.as_str()),
            (
                "Updated to r5",
                "The workspace is at r5, from r3. No files changed."
            )
        );
        let m = merge_outcome(
            &json!({"source": "dev", "from_rev": 4, "to_rev": 6, "nothing_to_do": false,
            "counts": {"applied": 1}, "files": [{"path": "a", "outcome": "applied"}, {"path": "x", "outcome": "skipped"}]}),
        );
        assert_eq!(m.title, "Merged dev r4..r6");
        assert_eq!(m.text, "1 applied. Your next commit records the merge.");
        assert!(m.done && !m.notes.is_empty());
        let n = merge_outcome(&json!({"source": "dev", "nothing_to_do": true}));
        assert_eq!(n.title, "Nothing to merge");
        let (rows, more) = outcome_rows(
            &json!({"files": [{"path": "a", "outcome": "x"}, {"path": "b", "outcome": "y"}]}),
            1,
        );
        assert_eq!((rows.len(), more), (1, 1));
    }
}
