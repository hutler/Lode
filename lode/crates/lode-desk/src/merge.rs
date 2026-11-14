//! Reading and editing a file with conflict markers, and marking what each side changed (T134).
//! The rules of the Tauri window's `merge.ts`.

use std::collections::BTreeSet;

/// A run of lines outside any conflict, or one conflict region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Text(Vec<String>),
    Conflict {
        mine: Vec<String>,
        base: Option<Vec<String>>,
        theirs: Vec<String>,
        mine_label: String,
        theirs_label: String,
    },
}

/// Which side a region takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    Mine,
    Theirs,
    /// Mine, then theirs.
    Both,
}

impl Take {
    /// `mine`, `theirs` or `both`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mine" => Some(Self::Mine),
            "theirs" => Some(Self::Theirs),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

/// The label after a marker of seven `c`, or `None` when `line` is not that marker.
fn marker(line: &str, c: char) -> Option<Option<&str>> {
    let rest = line.strip_prefix(&c.to_string().repeat(7))?;
    if rest.is_empty() {
        Some(None)
    } else {
        rest.strip_prefix(' ').map(Some)
    }
}

fn is_start(l: &str) -> bool {
    marker(l, '<').is_some()
}

fn is_base(l: &str) -> bool {
    marker(l, '|').is_some()
}

fn is_mid(l: &str) -> bool {
    l == "======="
}

fn is_end(l: &str) -> bool {
    marker(l, '>').is_some()
}

/// Lines without their line ends (a final newline does not add an empty line).
pub fn lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut all: Vec<String> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect();
    if all.last().is_some_and(String::is_empty) {
        all.pop();
    }
    all
}

/// Splits a file into plain runs and conflict regions. Markers that do not close are plain text.
pub fn parse_conflicts(text: &str) -> Vec<Part> {
    let src = lines(text);
    let mut parts = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    let mut i = 0;
    while i < src.len() {
        if let Some(start) = marker(&src[i], '<') {
            let (mut base_at, mut mid_at, mut end_at) = (None, None, None);
            for (j, l) in src.iter().enumerate().skip(i + 1) {
                if is_start(l) {
                    break;
                }
                if mid_at.is_none() && base_at.is_none() && is_base(l) {
                    base_at = Some(j);
                } else if mid_at.is_none() && is_mid(l) {
                    mid_at = Some(j);
                } else if mid_at.is_some() && is_end(l) {
                    end_at = Some(j);
                    break;
                }
            }
            if let (Some(mid), Some(end)) = (mid_at, end_at) {
                if !plain.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut plain)));
                }
                parts.push(Part::Conflict {
                    mine: src[i + 1..base_at.unwrap_or(mid)].to_vec(),
                    base: base_at.map(|b| src[b + 1..mid].to_vec()),
                    theirs: src[mid + 1..end].to_vec(),
                    mine_label: start.unwrap_or("mine").to_owned(),
                    theirs_label: marker(&src[end], '>')
                        .flatten()
                        .unwrap_or("theirs")
                        .to_owned(),
                });
                i = end + 1;
                continue;
            }
        }
        plain.push(src[i].clone());
        i += 1;
    }
    if !plain.is_empty() {
        parts.push(Part::Text(plain));
    }
    parts
}

/// The conflict regions of `text`, as (mine, theirs).
pub fn regions(text: &str) -> Vec<(Vec<String>, Vec<String>)> {
    parse_conflicts(text)
        .into_iter()
        .filter_map(|p| match p {
            Part::Conflict { mine, theirs, .. } => Some((mine, theirs)),
            Part::Text(_) => None,
        })
        .collect()
}

fn render(parts: &[Part], eol: &str, final_newline: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        match p {
            Part::Text(l) => out.extend(l.iter().cloned()),
            Part::Conflict {
                mine,
                base,
                theirs,
                mine_label,
                theirs_label,
            } => {
                out.push(format!("<<<<<<< {mine_label}"));
                out.extend(mine.iter().cloned());
                if let Some(b) = base {
                    out.push("|||||||".into());
                    out.extend(b.iter().cloned());
                }
                out.push("=======".into());
                out.extend(theirs.iter().cloned());
                out.push(format!(">>>>>>> {theirs_label}"));
            }
        }
    }
    let mut s = out.join(eol);
    if final_newline && !out.is_empty() {
        s.push_str(eol);
    }
    s
}

/// `text` with its `index`-th conflict region replaced by one side, or mine then theirs.
pub fn take_region(text: &str, index: usize, take: Take) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let final_newline = text.ends_with('\n');
    let mut n = 0;
    let parts: Vec<Part> = parse_conflicts(text)
        .into_iter()
        .map(|p| match p {
            Part::Conflict { mine, theirs, .. } if n == index => {
                n += 1;
                Part::Text(match take {
                    Take::Mine => mine,
                    Take::Theirs => theirs,
                    Take::Both => mine.into_iter().chain(theirs).collect(),
                })
            }
            c @ Part::Conflict { .. } => {
                n += 1;
                c
            }
            t => t,
        })
        .collect();
    render(&parts, eol, final_newline)
}

/// Which lines of `side` are not in `base`, by a longest common subsequence of lines. `None`
/// when the files are too long to compare here.
pub fn changed_lines(base: &[String], side: &[String], limit: usize) -> Option<BTreeSet<usize>> {
    let (n, m) = (base.len(), side.len());
    if n.saturating_mul(m) > limit {
        return None;
    }
    let mut lo = 0;
    while lo < n && lo < m && base[lo] == side[lo] {
        lo += 1;
    }
    let (mut hi_b, mut hi_s) = (n, m);
    while hi_b > lo && hi_s > lo && base[hi_b - 1] == side[hi_s - 1] {
        hi_b -= 1;
        hi_s -= 1;
    }
    let a = &base[lo..hi_b];
    let b = &side[lo..hi_s];
    let w = b.len() + 1;
    let mut dp = vec![0u32; (a.len() + 1) * w];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[i * w + j] = if a[i] == b[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let mut changed = BTreeSet::new();
    let (mut i, mut j) = (0, 0);
    while j < b.len() {
        if i < a.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if i < a.len() && dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            i += 1;
        } else {
            changed.insert(lo + j);
            j += 1;
        }
    }
    Some(changed)
}

/// How many lines two sides may have (multiplied) before marking is skipped.
pub const COMPARE_LIMIT: usize = 4_000_000;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const TWO: &str = "a\n<<<<<<< mine\nb1\n=======\nb2\n>>>>>>> r7\nc\n<<<<<<< mine\nd1\n|||||||\nd0\n=======\nd2\n>>>>>>> r7\n";

    #[test]
    fn regions_are_found_and_unclosed_markers_are_text() {
        let parts = parse_conflicts(TWO);
        assert_eq!(parts.len(), 4);
        assert_eq!(
            regions(TWO),
            [
                (vec!["b1".to_owned()], vec!["b2".to_owned()]),
                (vec!["d1".to_owned()], vec!["d2".to_owned()])
            ]
        );
        let Part::Conflict {
            base, theirs_label, ..
        } = &parts[3]
        else {
            panic!()
        };
        assert_eq!(base.as_deref(), Some(&["d0".to_owned()][..]));
        assert_eq!(theirs_label, "r7");
        assert!(regions("<<<<<<< mine\nx\n=======\ny\n").is_empty());
    }

    #[test]
    fn taking_a_side_keeps_the_rest() {
        let one = take_region(TWO, 0, Take::Theirs);
        assert_eq!(regions(&one).len(), 1);
        assert!(one.starts_with("a\nb2\nc\n<<<<<<< mine\n"));
        let both = take_region(&one, 0, Take::Both);
        assert_eq!(both, "a\nb2\nc\nd1\nd2\n");
        let crlf = take_region(
            "<<<<<<< mine\r\nx\r\n=======\r\ny\r\n>>>>>>> r2\r\n",
            0,
            Take::Mine,
        );
        assert_eq!(crlf, "x\r\n");
    }

    #[test]
    fn changed_lines_against_the_base() {
        let l = |s: &str| lines(s);
        let c = changed_lines(&l("a\nb\nc\n"), &l("a\nB\nc\nd\n"), COMPARE_LIMIT).unwrap();
        assert_eq!(c.into_iter().collect::<Vec<_>>(), [1, 3]);
        assert!(changed_lines(&l("a\nb\n"), &l("a\nb\n"), 1).is_none());
    }
}
