//! The diff of one file for the Slint window (T132): the patch of `lode diff --json` turned
//! into rows for the unified and the side-by-side layouts, with the words that differ between
//! a removed line and the added line it pairs with marked, and the facts shown for a binary
//! file.

use serde_json::Value;
use slint::{ModelRc, SharedString, VecModel};

use crate::app::{DiffCell, DiffRow, Span};

/// Patches longer than this are shown as plain text, without layouts or highlights.
pub const PLAIN_LIMIT: usize = 512 * 1024;
/// Files above this size are compared by size only (`lode diff` gives no patch).
pub const TEXT_LIMIT: u64 = 16 * 1024 * 1024;
/// Lines longer than this (in characters) get no word highlights.
const WORD_LIMIT: usize = 400;
/// Lines this alike are shown as one changed line rather than a removal and an addition.
const ALIKE: f64 = 0.5;
/// Runs longer than this are paired in order, without comparing lines.
const ALIGN_LIMIT: usize = 40;
/// A tab is shown as this many spaces, so marked words line up in the monospace grid.
const TAB: &str = "    ";

/// What a line of a patch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Context,
    Add,
    Delete,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Context => "context",
            Kind::Add => "add",
            Kind::Delete => "delete",
        }
    }
}

/// A run of characters of a line, marked when it differs from the line's counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub changed: bool,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// The text with tabs expanded.
    pub text: String,
    pub old: Option<u64>,
    pub new: Option<u64>,
    /// The line's words, marked where they differ from its counterpart; none when unpaired.
    pub words: Option<Vec<Segment>>,
}

/// A hunk: its `@@` header and lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
}

/// The two layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Unified,
    Split,
}

impl Layout {
    pub fn parse(s: &str) -> Self {
        if s == "split" {
            Layout::Split
        } else {
            Layout::Unified
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Unified => "unified",
            Layout::Split => "split",
        }
    }
}

/// `@@ -A[,B] +C[,D] @@` → (A, C).
fn hunk_start(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let first = |s: &str| s.split(',').next()?.parse::<u64>().ok();
    Some((first(old)?, first(new)?))
}

/// The hunks of a unified patch, with line numbers and word highlights; the header lines
/// before the first hunk are skipped.
pub fn parse_patch(patch: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_no, mut new_no) = (0u64, 0u64);
    let mut lines: Vec<&str> = patch.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for l in lines {
        if let Some((o, n)) = hunk_start(l) {
            hunks.push(Hunk {
                header: l.to_owned(),
                lines: Vec::new(),
            });
            old_no = o;
            new_no = n;
            continue;
        }
        let Some(h) = hunks.last_mut() else { continue };
        // Some tools write an empty context line as an empty line.
        let mark = l.chars().next().unwrap_or(' ');
        let text = l.get(1..).unwrap_or("").replace('\t', TAB);
        let line = |kind, old, new| Line {
            kind,
            text: text.clone(),
            old,
            new,
            words: None,
        };
        match mark {
            ' ' => {
                h.lines
                    .push(line(Kind::Context, Some(old_no), Some(new_no)));
                old_no += 1;
                new_no += 1;
            }
            '-' => {
                h.lines.push(line(Kind::Delete, Some(old_no), None));
                old_no += 1;
            }
            '+' => {
                h.lines.push(line(Kind::Add, None, Some(new_no)));
                new_no += 1;
            }
            // "\ No newline at end of file" and anything else is not a line of the file.
            _ => {}
        }
    }
    for h in &mut hunks {
        highlight(&mut h.lines);
    }
    hunks
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Splits a line into words, runs of white space and single other characters.
pub fn tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None::<(usize, u8)>;
    for (i, c) in line.char_indices() {
        let class = if is_word(c) {
            1
        } else if c.is_whitespace() {
            2
        } else {
            3
        };
        match start {
            // Words and white space run on; every other character is a token of its own.
            Some((_, k)) if k == class && class != 3 => {}
            Some((s, _)) => {
                out.push(&line[s..i]);
                start = Some((i, class));
            }
            None => start = Some((i, class)),
        }
    }
    if let Some((s, _)) = start {
        out.push(&line[s..]);
    }
    out
}

fn push(side: &mut Vec<Segment>, text: &str, changed: bool) {
    match side.last_mut() {
        Some(last) if last.changed == changed => last.text.push_str(text),
        _ => side.push(Segment {
            text: text.to_owned(),
            changed,
        }),
    }
}

/// Word-level difference of two lines: each side's segments, with the changed ones marked.
pub fn word_diff(a: &str, b: &str) -> (Vec<Segment>, Vec<Segment>) {
    let x = tokens(a);
    let y = tokens(b);
    let (n, m) = (x.len(), y.len());
    // Longest common subsequence of the tokens.
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if x[i] == y[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && x[i] == y[j] {
            push(&mut left, x[i], false);
            push(&mut right, y[j], false);
            i += 1;
            j += 1;
        } else if j < m && (i >= n || table[i][j + 1] >= table[i + 1][j]) {
            push(&mut right, y[j], true);
            j += 1;
        } else {
            push(&mut left, x[i], true);
            i += 1;
        }
    }
    (left, right)
}

fn letters(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// How alike two lines are, from 0 to 1: the share of their tokens they have in common.
fn similarity(a: &str, b: &str) -> f64 {
    if a.chars().count() > WORD_LIMIT || b.chars().count() > WORD_LIMIT {
        return if a == b { 1.0 } else { 0.0 };
    }
    let (l, r) = word_diff(a, b);
    let same = |segs: &[Segment]| -> usize {
        segs.iter()
            .filter(|s| !s.changed)
            .map(|s| letters(&s.text))
            .sum()
    };
    let total = letters(a) + letters(b);
    if total == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let share = (same(&l) + same(&r)) as f64 / total as f64;
    share
}

/// Pairs for a run of removed lines followed by a run of added ones: each removed line next to
/// the most alike added line after the last pair, the others alone, in file order. Indexes
/// into `dels` and `adds`.
pub fn align(dels: &[&str], adds: &[&str]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut rows = Vec::new();
    if dels.len() > ALIGN_LIMIT || adds.len() > ALIGN_LIMIT {
        for k in 0..dels.len().max(adds.len()) {
            rows.push(((k < dels.len()).then_some(k), (k < adds.len()).then_some(k)));
        }
        return rows;
    }
    let mut next = 0;
    for (di, d) in dels.iter().enumerate() {
        // The most alike added line still free; the earliest of equals.
        let mut found = None;
        let mut best = ALIKE;
        for (j, a) in adds.iter().enumerate().skip(next) {
            let score = similarity(d, a);
            if score > best || (score >= best && found.is_none()) {
                found = Some(j);
                best = score;
            }
        }
        let Some(j) = found else {
            rows.push((Some(di), None));
            continue;
        };
        while next < j {
            rows.push((None, Some(next)));
            next += 1;
        }
        rows.push((Some(di), Some(j)));
        next = j + 1;
    }
    while next < adds.len() {
        rows.push((None, Some(next)));
        next += 1;
    }
    rows
}

/// A hunk's lines as context lines (alone) and runs of changes: (first removed, removed
/// count, added count), by index.
enum Run {
    Context(usize),
    Change {
        start: usize,
        dels: usize,
        adds: usize,
    },
}

fn runs(lines: &[Line]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == Kind::Context {
            out.push(Run::Context(i));
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind == Kind::Delete {
            i += 1;
        }
        let dels = i - start;
        while i < lines.len() && lines[i].kind == Kind::Add {
            i += 1;
        }
        out.push(Run::Change {
            start,
            dels,
            adds: i - start - dels,
        });
    }
    out
}

/// The pairs of one run of changes, as indexes into the hunk's lines.
fn run_pairs(
    lines: &[Line],
    start: usize,
    dels: usize,
    adds: usize,
) -> Vec<(Option<usize>, Option<usize>)> {
    let d: Vec<&str> = lines[start..start + dels]
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    let a: Vec<&str> = lines[start + dels..start + dels + adds]
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    align(&d, &a)
        .into_iter()
        .map(|(x, y)| (x.map(|x| start + x), y.map(|y| start + dels + y)))
        .collect()
}

/// Marks the words that differ between each removed line and the added line it pairs with.
fn highlight(lines: &mut [Line]) {
    for run in runs(lines) {
        let Run::Change { start, dels, adds } = run else {
            continue;
        };
        for (d, a) in run_pairs(lines, start, dels, adds) {
            let (Some(d), Some(a)) = (d, a) else { continue };
            if lines[d].text.chars().count() > WORD_LIMIT
                || lines[a].text.chars().count() > WORD_LIMIT
            {
                continue;
            }
            let (l, r) = word_diff(&lines[d].text, &lines[a].text);
            lines[d].words = Some(l);
            lines[a].words = Some(r);
        }
    }
}

/// The side-by-side rows of a hunk: context on both sides, each changed line next to the line
/// it pairs with.
pub fn split_rows(h: &Hunk) -> Vec<(Option<&Line>, Option<&Line>)> {
    let mut rows = Vec::new();
    for run in runs(&h.lines) {
        match run {
            Run::Context(i) => rows.push((Some(&h.lines[i]), Some(&h.lines[i]))),
            Run::Change { start, dels, adds } => {
                for (d, a) in run_pairs(&h.lines, start, dels, adds) {
                    rows.push((d.map(|d| &h.lines[d]), a.map(|a| &h.lines[a])));
                }
            }
        }
    }
    rows
}

/// The marked parts of a line, as character offsets for the window to draw behind the text.
fn spans(line: &Line) -> ModelRc<Span> {
    let Some(words) = &line.words else {
        return ModelRc::default();
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    for s in words {
        let len = s.text.chars().count();
        if s.changed {
            out.push(Span {
                start: i32::try_from(at).unwrap_or(i32::MAX),
                len: i32::try_from(len).unwrap_or(i32::MAX),
            });
        }
        at += len;
    }
    ModelRc::new(VecModel::from(out))
}

fn num(n: Option<u64>) -> SharedString {
    n.map(|n| n.to_string()).unwrap_or_default().into()
}

fn cell(line: Option<&Line>, old: bool) -> DiffCell {
    match line {
        None => DiffCell {
            kind: "empty".into(),
            ..DiffCell::default()
        },
        Some(l) => DiffCell {
            kind: l.kind.name().into(),
            num: num(if old { l.old } else { l.new }),
            text: l.text.as_str().into(),
            spans: spans(l),
        },
    }
}

fn header_row(h: &Hunk) -> DiffRow {
    DiffRow {
        hunk: true,
        header: h.header.as_str().into(),
        kind: "hunk".into(),
        ..DiffRow::default()
    }
}

/// The rows of `hunks` in `layout`.
pub fn rows(hunks: &[Hunk], layout: Layout) -> Vec<DiffRow> {
    let mut out = Vec::new();
    for h in hunks {
        out.push(header_row(h));
        match layout {
            Layout::Unified => {
                for l in &h.lines {
                    out.push(DiffRow {
                        kind: l.kind.name().into(),
                        old_num: num(l.old),
                        new_num: num(l.new),
                        text: l.text.as_str().into(),
                        spans: spans(l),
                        ..DiffRow::default()
                    });
                }
            }
            Layout::Split => {
                for (left, right) in split_rows(h) {
                    out.push(DiffRow {
                        kind: "split".into(),
                        left: cell(left, true),
                        right: cell(right, false),
                        ..DiffRow::default()
                    });
                }
            }
        }
    }
    out
}

/// A large patch as plain lines, without numbers or highlights.
pub fn plain_rows(patch: &str) -> Vec<DiffRow> {
    patch
        .lines()
        .map(|l| DiffRow {
            kind: "plain".into(),
            text: l.replace('\t', TAB).into(),
            ..DiffRow::default()
        })
        .collect()
}

/// The image type the window can show, by the file's extension.
pub fn is_image(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        ),
        _ => false,
    }
}

/// The file's extension, if it has one.
pub fn extension(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => "",
    }
}

/// What the diff of one file is, before any rows: the facts the header and notes show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facts {
    pub path: String,
    pub old_path: Option<String>,
    pub binary: bool,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub insertions: u64,
    pub deletions: u64,
    pub patch: String,
}

impl Facts {
    /// One file of `lode diff --json`.
    pub fn from_json(v: &Value) -> Self {
        let s = |k: &str| v[k].as_str().map(str::to_owned);
        Self {
            path: s("path").unwrap_or_default(),
            old_path: s("old_path"),
            binary: v["binary"].as_bool().unwrap_or(false),
            old_size: v["old_size"].as_u64(),
            new_size: v["new_size"].as_u64(),
            old_mode: s("old_mode"),
            new_mode: s("new_mode"),
            insertions: v["insertions"].as_u64().unwrap_or(0),
            deletions: v["deletions"].as_u64().unwrap_or(0),
            patch: s("patch").unwrap_or_default(),
        }
    }

    /// Either side is over the text limit.
    pub fn too_big(&self) -> bool {
        self.old_size.unwrap_or(0) > TEXT_LIMIT || self.new_size.unwrap_or(0) > TEXT_LIMIT
    }

    /// The mode changed, as "old → new".
    pub fn mode_change(&self) -> Option<String> {
        match (&self.old_mode, &self.new_mode) {
            (Some(a), Some(b)) if a != b => Some(format!("{a} → {b}")),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use slint::Model;

    use super::*;

    const PATCH: &str = "--- a/Source/Boat.cpp\n+++ b/Source/Boat.cpp\n@@ -10,4 +10,5 @@ void Boat::Tick()\n {\n-    int speed = 10;\n+    int speed = 12;\n+    int unused = 0;\n     Move(speed);\n }\n@@ -40,2 +41,2 @@\n-return a;\n+return b;\n";

    #[test]
    fn hunks_have_line_numbers() {
        let hunks = parse_patch(PATCH);
        assert_eq!(hunks.len(), 2);
        let got: Vec<_> = hunks[0]
            .lines
            .iter()
            .map(|l| (l.kind, l.old, l.new))
            .collect();
        assert_eq!(
            got,
            [
                (Kind::Context, Some(10), Some(10)),
                (Kind::Delete, Some(11), None),
                (Kind::Add, None, Some(11)),
                (Kind::Add, None, Some(12)),
                (Kind::Context, Some(12), Some(13)),
                (Kind::Context, Some(13), Some(14)),
            ]
        );
        let texts: Vec<_> = hunks[1].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["return a;", "return b;"]);
    }

    #[test]
    fn empty_context_lines_and_no_newline_notes() {
        let h = &parse_patch("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n\\ No newline at end of file\n")[0];
        let got: Vec<_> = h
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str(), l.old, l.new))
            .collect();
        assert_eq!(
            got,
            [
                (Kind::Context, "a", Some(1), Some(1)),
                (Kind::Context, "", Some(2), Some(2)),
                (Kind::Delete, "b", Some(3), None),
                (Kind::Add, "c", None, Some(3)),
            ]
        );
    }

    #[test]
    fn changed_words_are_marked() {
        let (l, r) = word_diff("int speed = 10;", "int speed = 12;");
        let changed = |s: &[Segment]| -> Vec<String> {
            s.iter()
                .filter(|s| s.changed)
                .map(|s| s.text.clone())
                .collect()
        };
        assert_eq!(changed(&l), ["10"]);
        assert_eq!(changed(&r), ["12"]);
        assert_eq!(
            l.iter().map(|s| s.text.as_str()).collect::<String>(),
            "int speed = 10;"
        );
        let h = &parse_patch(PATCH)[0];
        let speed = h
            .lines
            .iter()
            .find(|l| l.text.contains("speed = 12"))
            .unwrap();
        assert_eq!(changed(speed.words.as_ref().unwrap()), ["12"]);
        let unused = h.lines.iter().find(|l| l.text.contains("unused")).unwrap();
        assert!(unused.words.is_none(), "an unpaired line has no marks");
    }

    #[test]
    fn tokens_split_words_spaces_and_signs() {
        assert_eq!(
            tokens("a_b  = c+1;"),
            ["a_b", "  ", "=", " ", "c", "+", "1", ";"]
        );
        assert_eq!(tokens("((x"), ["(", "(", "x"]);
        assert!(tokens("").is_empty());
    }

    #[test]
    fn side_by_side_pairs_alike_lines() {
        let hunks = parse_patch(PATCH);
        let rows = split_rows(&hunks[0]);
        let got: Vec<_> = rows
            .iter()
            .map(|(l, r)| (l.map(|l| l.text.trim()), r.map(|r| r.text.trim())))
            .collect();
        assert_eq!(
            got,
            [
                (Some("{"), Some("{")),
                (Some("int speed = 10;"), Some("int speed = 12;")),
                (None, Some("int unused = 0;")),
                (Some("Move(speed);"), Some("Move(speed);")),
                (Some("}"), Some("}")),
            ]
        );
        // A line unlike every added one stands alone.
        let hunks =
            parse_patch("@@ -1,2 +1,2 @@\n-alpha beta\n-gamma\n+completely different\n+gamma!\n");
        let got: Vec<_> = split_rows(&hunks[0])
            .iter()
            .map(|(l, r)| (l.map(|l| l.text.as_str()), r.map(|r| r.text.as_str())))
            .collect();
        assert_eq!(
            got,
            [
                (Some("alpha beta"), None),
                (None, Some("completely different")),
                (Some("gamma"), Some("gamma!")),
            ]
        );
    }

    #[test]
    fn rows_for_each_layout() {
        let hunks = parse_patch(PATCH);
        let u = rows(&hunks, Layout::Unified);
        assert_eq!(u.len(), 2 + 6 + 2);
        assert!(u[0].hunk);
        let speed = u.iter().find(|r| r.text.contains("speed = 12")).unwrap();
        assert_eq!(speed.kind, "add");
        assert_eq!(speed.new_num, "11");
        let marks: Vec<_> = speed.spans.iter().collect();
        assert_eq!(marks, [Span { start: 16, len: 2 }]);
        let s = rows(&hunks, Layout::Split);
        assert_eq!(s.len(), 2 + 5 + 1, "return a and return b pair up");
        assert_eq!(s[2].left.text, "    int speed = 10;");
        assert_eq!(s[2].left.num, "11");
        assert_eq!(s[3].left.kind, "empty");
        assert_eq!(s[3].right.kind, "add");
    }

    #[test]
    fn tabs_are_expanded() {
        let h = &parse_patch("@@ -1 +1 @@\n-\tx = 1\n+\tx = 2\n")[0];
        assert_eq!(h.lines[0].text, "    x = 1");
    }

    #[test]
    fn long_runs_pair_in_order() {
        let dels: Vec<String> = (0..50).map(|i| format!("old {i}")).collect();
        let adds: Vec<String> = (0..45).map(|i| format!("new {i}")).collect();
        let d: Vec<&str> = dels.iter().map(String::as_str).collect();
        let a: Vec<&str> = adds.iter().map(String::as_str).collect();
        let pairs = align(&d, &a);
        assert_eq!(pairs.len(), 50);
        assert_eq!(pairs[44], (Some(44), Some(44)));
        assert_eq!(pairs[49], (Some(49), None));
    }

    #[test]
    fn images_and_extensions() {
        assert!(is_image("Content/Lamp.PNG"));
        assert!(is_image("a.jpeg"));
        assert!(!is_image("Map.umap"));
        assert!(!is_image(".png"));
        assert_eq!(extension("Maps/Harbor.umap"), "umap");
        assert_eq!(extension("Makefile"), "");
    }

    #[test]
    fn facts_from_json() {
        let f = Facts::from_json(&serde_json::json!({
            "path": "x.bin", "status": "modified", "old_path": null, "binary": true,
            "old_size": 20_000_000u64, "new_size": 10, "old_mode": "file", "new_mode": "exec",
            "insertions": 0, "deletions": 0
        }));
        assert!(f.binary && f.too_big());
        assert_eq!(f.mode_change().unwrap(), "file → exec");
        assert!(plain_rows("a\n\tb\n").iter().all(|r| r.kind == "plain"));
    }
}
