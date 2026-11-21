//! A branch's or a file's history and blame for the Slint window (T132): the rows of the
//! revision list with their graph, the selected revision's details, and the lines of blame.
//!
//! A Lode branch is a single line of revisions: merges are recorded on the merging revision as
//! ranges of another branch, and a branch's history continues into its parent's below the
//! branch point. So the graph is the branch's own lane, with an incoming edge from a second
//! lane at each merge, and the lane changes color where the history becomes the parent's.

use std::collections::HashMap;

use serde_json::Value;
use slint::SharedString;

use crate::app::{BlameLine, RevDetail, RevFile, RevRow};

/// Revisions loaded per page.
pub const PAGE: u32 = 100;

/// The first line of a message.
pub fn summary(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}

/// `2026-11-14T19:13:39Z` as `2026-11-14 19:13 UTC`; anything else as it is.
pub fn format_date(iso: &str) -> String {
    let b = iso.as_bytes();
    let shaped = b.len() >= 16
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && iso.is_char_boundary(16);
    if !shaped {
        return iso.to_owned();
    }
    let zone = if iso.ends_with('Z') || iso.ends_with("+00:00") {
        " UTC".to_owned()
    } else {
        match iso
            .get(19..)
            .map(|r| r.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit()))
        {
            Some(z) if !z.is_empty() => format!(" {z}"),
            _ => String::new(),
        }
    };
    format!("{} {}{zone}", &iso[..10], &iso[11..16])
}

fn text(v: &Value) -> SharedString {
    v.as_str().unwrap_or_default().into()
}

fn rev_of(v: &Value) -> i32 {
    v["rev"]
        .as_u64()
        .and_then(|r| i32::try_from(r).ok())
        .unwrap_or(0)
}

/// The merges a revision records, as "merge BRANCH" tags.
fn merge_tags(r: &Value) -> String {
    r["merged_from"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter_map(|m| m["branch"].as_str())
                .map(|b| format!("merge {b}"))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .unwrap_or_default()
}

fn is_agent(r: &Value) -> bool {
    r["provenance"]["agent"].is_object() || r["author"]["kind"].as_str() == Some("agent")
}

fn imported(r: &Value) -> String {
    r["provenance"]["imported"]["system"]
        .as_str()
        .map(|s| format!("imported from {s}"))
        .unwrap_or_default()
}

/// The rows of `revisions` (newest first) of `branch`, with the graph laid out; `more` when
/// older revisions exist that are not loaded yet. `checks` is an answer of `Desk::checks`
/// for `rN` targets (or `Value::Null`), for each row's badge.
pub fn rows(
    revisions: &[Value],
    branch: &str,
    more: bool,
    selected: Option<u64>,
    checks: &Value,
) -> Vec<RevRow> {
    let mut out = Vec::with_capacity(revisions.len());
    for (i, r) in revisions.iter().enumerate() {
        let on = r["branch"].as_str().unwrap_or_default();
        let inherited = on != branch;
        let prev = i.checked_sub(1).and_then(|p| revisions.get(p));
        let prev_branch = prev.and_then(|p| p["branch"].as_str());
        let branch_point = inherited && prev_branch != Some(on);
        let merges = merge_tags(r);
        let message = r["message"].as_str().unwrap_or_default();
        out.push(RevRow {
            rev: rev_of(r),
            label: format!("r{}", rev_of(r)).into(),
            summary: summary(message).into(),
            author: text(&r["author"]["name"]),
            date: format_date(r["created_at"].as_str().unwrap_or_default()).into(),
            branch_tag: if (i == 0 && !inherited) || branch_point {
                on.into()
            } else {
                SharedString::new()
            },
            inherited,
            merge: !merges.is_empty(),
            merges: merges.into(),
            agent: is_agent(r),
            imported: imported(r).into(),
            line_above: i > 0,
            above_inherited: prev_branch.is_some_and(|b| b != branch),
            line_below: !r["parent"].is_null() && (i + 1 < revisions.len() || more),
            selected: selected.is_some_and(|s| r["rev"].as_u64() == Some(s)),
            checks: crate::team::badge(crate::team::checks_of(checks, &format!("r{}", rev_of(r)))),
        });
    }
    out
}

/// The rows whose summary, author or revision label hold `filter` (ignoring case); a blank
/// filter keeps them all. The kept rows are re-linked so the graph's line runs unbroken.
pub fn filter_rows(rows: &[RevRow], filter: &str) -> Vec<RevRow> {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    let mut out: Vec<RevRow> = rows
        .iter()
        .filter(|r| {
            r.summary.to_lowercase().contains(&needle)
                || r.author.to_lowercase().contains(&needle)
                || r.label.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    let n = out.len();
    for (i, row) in out.iter_mut().enumerate() {
        row.line_above = i > 0;
        if i + 1 < n {
            row.line_below = true;
        }
    }
    out
}

/// The details of one revision (`lode log -v` of it), and its changed files.
pub fn detail(r: &Value, branch: &str) -> (RevDetail, Vec<RevFile>) {
    let message = r["message"].as_str().unwrap_or_default();
    let body = message
        .split_once('\n')
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();
    let on = r["branch"].as_str().unwrap_or_default();
    let author = r["author"]["name"].as_str().unwrap_or_default();
    let submitter = r["submitter"]["name"].as_str().unwrap_or_default();
    let p = &r["provenance"];
    let merged = r["merged_from"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .map(|m| {
                    format!(
                        "{} r{}..r{}",
                        m["branch"].as_str().unwrap_or_default(),
                        m["from_rev"].as_u64().unwrap_or(0),
                        m["to_rev"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let agent = p["agent"]
        .as_object()
        .map(|a| {
            a.iter()
                .map(|(k, v)| format!("{k} {}", v.as_str().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let imported = match (
        p["imported"]["system"].as_str(),
        p["imported"]["id"].as_str(),
    ) {
        (Some(s), Some(id)) => format!("{s} {}", id.chars().take(12).collect::<String>()),
        _ => String::new(),
    };
    let files = r["changes"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    let path = c["path"].as_str().unwrap_or_default();
                    let action = c["action"].as_str().unwrap_or_default();
                    let (letter, status) = match action {
                        "add" => ("A", "added"),
                        "delete" => ("D", "deleted"),
                        "modify" => ("M", "modified"),
                        _ => ("?", "modified"),
                    };
                    RevFile {
                        path: path.into(),
                        name: path.rsplit('/').next().unwrap_or(path).into(),
                        letter: letter.into(),
                        status: status.into(),
                        can_blame: action != "delete",
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let d = RevDetail {
        shown: true,
        rev: rev_of(r),
        title: format!("r{}", rev_of(r)).into(),
        on_branch: if on == branch {
            SharedString::new()
        } else {
            format!("on {on}").into()
        },
        summary: summary(message).into(),
        body: body.into(),
        author: if r["author"]["kind"].as_str() == Some("agent") {
            format!("{author} (agent)").into()
        } else {
            author.into()
        },
        submitter: if submitter != author {
            submitter.into()
        } else {
            SharedString::new()
        },
        date: format_date(r["created_at"].as_str().unwrap_or_default()).into(),
        branch: on.into(),
        merged: merged.into(),
        agent: agent.into(),
        imported: imported.into(),
        client: text(&p["client"]),
        files: i32::try_from(files.len()).unwrap_or(i32::MAX),
        truncated: r["changes_truncated"].as_bool().unwrap_or(false),
    };
    (d, files)
}

/// The lines of `lode blame --json`. The first line of each run of lines from the same
/// revision names it; runs alternate in shade so neighbours stand apart.
pub fn blame_lines(b: &Value) -> Vec<BlameLine> {
    let by_rev: HashMap<u64, &Value> = b["revisions"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| Some((r["rev"].as_u64()?, r)))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut shade = false;
    let mut last = None;
    for (i, l) in b["lines"].as_array().into_iter().flatten().enumerate() {
        let rev = l["rev"].as_u64().unwrap_or(0);
        let start = last != Some(rev);
        if start {
            shade = !shade;
        }
        last = Some(rev);
        let info = by_rev.get(&rev);
        let field = |k: &str| -> SharedString {
            if !start {
                return SharedString::new();
            }
            info.map(|r| match k {
                "author" => text(&r["author"]["name"]),
                "date" => format_date(r["created_at"].as_str().unwrap_or_default()).into(),
                _ => text(&r[k]),
            })
            .unwrap_or_default()
        };
        out.push(BlameLine {
            start,
            shade,
            rev: i32::try_from(rev).unwrap_or(0),
            label: if start {
                format!("r{rev}").into()
            } else {
                SharedString::new()
            },
            author: field("author"),
            summary: field("summary"),
            date: field("date"),
            num: i32::try_from(i + 1).unwrap_or(i32::MAX),
            text: l["text"]
                .as_str()
                .unwrap_or_default()
                .replace('\t', "    ")
                .into(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use serde_json::json;

    use super::*;

    fn rev(n: u64, branch: &str, parent: Option<u64>) -> Value {
        json!({
            "rev": n, "branch": branch, "parent": parent,
            "author": {"name": "alice", "kind": "user"},
            "submitter": {"name": "alice", "kind": "user"},
            "created_at": "2026-11-14T19:13:39Z",
            "message": format!("change {n}\n\nmore words"),
            "merged_from": [],
        })
    }

    #[test]
    fn filter_keeps_matching_revisions_and_relates_them() {
        let revs = vec![
            rev(5, "main", Some(4)),
            rev(4, "main", Some(3)),
            rev(3, "main", Some(2)),
        ];
        let all = rows(&revs, "main", true, Some(4), &Value::Null);
        let same = filter_rows(&all, "  ");
        assert_eq!(same.len(), 3, "a blank filter keeps every row");
        let r = filter_rows(&all, "change 5");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].rev, 5);
        assert!(!r[0].line_above, "the first kept row starts the graph");
        assert!(r[0].line_below, "the last kept row keeps its line down");
        let r = filter_rows(&all, "CHANGE 3");
        assert_eq!(r.len(), 1, "the match ignores case");
        assert_eq!(r[0].rev, 3);
        let r = filter_rows(&all, "alice");
        assert_eq!(r.len(), 3, "the author matches too");
        assert!(r[0].line_below && r[1].line_above && r[1].line_below);
        assert!(!r[2].line_below || all[2].line_below);
        let r = filter_rows(&all, "r4");
        assert_eq!(r.len(), 1, "the revision label matches");
        assert_eq!(r[0].rev, 4);
        assert!(filter_rows(&all, "nobody").is_empty());
    }

    #[test]
    fn graph_of_a_branch_with_a_merge() {
        let mut revs = vec![
            rev(5, "main", Some(4)),
            rev(4, "main", Some(3)),
            rev(3, "main", Some(2)),
        ];
        revs[1]["merged_from"] = json!([{"branch": "feature/x", "from_rev": 2, "to_rev": 3}]);
        let r = rows(&revs, "main", true, Some(4), &Value::Null);
        assert_eq!(r[0].branch_tag, "main");
        assert!(!r[0].line_above && r[1].line_above);
        assert!(r[2].line_below, "older revisions are not loaded yet");
        assert!(r[1].merge);
        assert_eq!(r[1].merges, "merge feature/x");
        assert!(r[1].selected && !r[0].selected);
        assert_eq!(r[0].summary, "change 5");
        assert_eq!(r[0].date, "2026-11-14 19:13 UTC");
        let r = rows(&revs, "main", false, None, &Value::Null);
        assert!(
            !r[2].line_below,
            "the last loaded revision has nothing below"
        );
    }

    #[test]
    fn the_parent_branch_below_the_branch_point() {
        let revs = vec![
            rev(9, "release", Some(8)),
            rev(8, "release", Some(5)),
            rev(5, "main", Some(4)),
            rev(4, "main", None),
        ];
        let r = rows(&revs, "release", false, None, &Value::Null);
        let got: Vec<_> = r
            .iter()
            .map(|r| (r.rev, r.inherited, r.branch_tag.to_string()))
            .collect();
        assert_eq!(
            got,
            [
                (9, false, "release".into()),
                (8, false, String::new()),
                (5, true, "main".into()),
                (4, true, String::new()),
            ]
        );
        assert!(!r[2].above_inherited && r[3].above_inherited);
        assert!(!r[3].line_below, "the first revision there is");
    }

    #[test]
    fn details_of_a_revision() {
        let mut r = rev(7, "feature", Some(6));
        r["author"] = json!({"name": "bot", "kind": "agent"});
        r["provenance"] = json!({"agent": {"session": "s1"}, "client": "lode 0.2"});
        r["changes"] = json!([
            {"path": "a/b.txt", "action": "modify"},
            {"path": "c.png", "action": "add"},
            {"path": "d.txt", "action": "delete"},
        ]);
        let (d, files) = detail(&r, "main");
        assert_eq!(d.title, "r7");
        assert_eq!(d.on_branch, "on feature");
        assert_eq!(d.body, "more words");
        assert_eq!(d.author, "bot (agent)");
        assert_eq!(d.submitter, "alice");
        assert_eq!(d.agent, "session s1");
        assert_eq!(d.client, "lode 0.2");
        assert_eq!(d.files, 3);
        assert_eq!(files[0].name, "b.txt");
        assert_eq!(files[1].letter, "A");
        assert!(!files[2].can_blame);
    }

    #[test]
    fn blame_runs() {
        let b = json!({
            "path": "a.txt", "rev": 3,
            "lines": [{"rev": 1, "text": "x"}, {"rev": 1, "text": "y"}, {"rev": 3, "text": "\tz"}],
            "revisions": [
                {"rev": 1, "branch": "main", "author": {"name": "alice"}, "created_at": "2026-01-02T03:04:05Z", "summary": "first"},
                {"rev": 3, "branch": "main", "author": {"name": "bob"}, "created_at": "x", "summary": "third"},
            ],
        });
        let l = blame_lines(&b);
        assert_eq!(l.len(), 3);
        assert!(l[0].start && !l[1].start && l[2].start);
        assert_eq!(l[0].label, "r1");
        assert_eq!(l[0].author, "alice");
        assert_eq!(l[1].author, "");
        assert_eq!(l[2].summary, "third");
        assert!(l[0].shade && l[1].shade && !l[2].shade);
        assert_eq!(l[2].text, "    z");
        assert_eq!(l[2].num, 3);
    }

    #[test]
    fn dates() {
        assert_eq!(
            format_date("2026-11-14T19:13:39.123Z"),
            "2026-11-14 19:13 UTC"
        );
        assert_eq!(
            format_date("2026-11-14T19:13:39+03:00"),
            "2026-11-14 19:13 +03:00"
        );
        assert_eq!(format_date("yesterday"), "yesterday");
    }
}
