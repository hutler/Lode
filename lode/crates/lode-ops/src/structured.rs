//! Structure-aware three-way merge (ARCHITECTURE §10.5, T163): JSON objects merged key by key
//! when a line merge conflicts.
//!
//! The result keeps the local side's key order, places keys added on the other side after
//! their neighbour there, and is written with the local side's indentation, line endings and
//! final newline. Numbers keep their value but may be written differently (`1e3` as `1000.0`).

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value that keeps object keys in file order.
#[derive(Debug, Clone, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl<'de> Deserialize<'de> for J {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = J;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<J, E> {
                Ok(J::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<J, E> {
                Ok(J::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<J, E> {
                Ok(J::Num(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<J, E> {
                Ok(J::Num(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<J, E> {
                serde_json::Number::from_f64(v)
                    .map(J::Num)
                    .ok_or_else(|| E::custom("not a JSON number"))
            }
            fn visit_str<E>(self, v: &str) -> Result<J, E> {
                Ok(J::Str(v.to_owned()))
            }
            fn visit_string<E>(self, v: String) -> Result<J, E> {
                Ok(J::Str(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<J, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = seq.next_element()? {
                    out.push(v);
                }
                Ok(J::Arr(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<J, A::Error> {
                let mut out: Vec<(String, J)> = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, J>()? {
                    // A repeated key: the last one wins, as in most readers.
                    if let Some(slot) = out.iter_mut().find(|(x, _)| *x == k) {
                        slot.1 = v;
                    } else {
                        out.push((k, v));
                    }
                }
                Ok(J::Obj(out))
            }
        }
        d.deserialize_any(V)
    }
}

fn get<'a>(o: &'a [(String, J)], k: &str) -> Option<&'a J> {
    o.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

/// The three-way merge of `mine` and `theirs` from `base`; `None` on a real conflict.
fn merge(base: Option<&J>, mine: &J, theirs: &J) -> Option<J> {
    if mine == theirs {
        return Some(mine.clone());
    }
    if base == Some(mine) {
        return Some(theirs.clone());
    }
    if base == Some(theirs) {
        return Some(mine.clone());
    }
    let (J::Obj(m), J::Obj(t)) = (mine, theirs) else {
        return None;
    };
    let empty = Vec::new();
    let b = match base {
        Some(J::Obj(b)) => b,
        None => &empty,
        Some(_) => return None,
    };
    let mut out: Vec<(String, J)> = Vec::new();
    for (k, mv) in m {
        match (get(b, k), get(t, k)) {
            (bv, Some(tv)) => out.push((k.clone(), merge(bv, mv, tv)?)),
            // Added here only.
            (None, None) => out.push((k.clone(), mv.clone())),
            // Removed there: fine if unchanged here.
            (Some(bv), None) if bv == mv => {}
            (Some(_), None) => return None,
        }
    }
    // Keys only there: added there, or removed here.
    for (i, (k, tv)) in t.iter().enumerate() {
        if get(m, k).is_some() {
            continue;
        }
        match get(b, k) {
            None => {
                // After the nearest earlier key of theirs that the result has, else before
                // the nearest later one, else at the end.
                let at = t[..i]
                    .iter()
                    .rev()
                    .find_map(|(p, _)| out.iter().position(|(x, _)| x == p))
                    .map(|p| p + 1)
                    .or_else(|| {
                        t[i + 1..]
                            .iter()
                            .find_map(|(n, _)| out.iter().position(|(x, _)| x == n))
                    })
                    .unwrap_or(out.len());
                out.insert(at, (k.clone(), tv.clone()));
            }
            Some(bv) if bv == tv => {}
            Some(_) => return None,
        }
    }
    Some(J::Obj(out))
}

/// How the local file is laid out.
struct Style {
    indent: Option<String>,
    newline: &'static str,
    final_newline: bool,
}

impl Style {
    fn of(text: &str) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let indent = text
            .lines()
            .skip(1)
            .find_map(|l| {
                let ws: String = l.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                (!ws.is_empty() && ws.len() < l.len()).then_some(ws)
            })
            .or_else(|| text.trim_end().contains('\n').then(|| "  ".to_owned()));
        Self {
            indent,
            newline,
            final_newline: text.ends_with('\n'),
        }
    }

    fn write(&self, v: &J, depth: usize, out: &mut String) {
        let scalar = |v: &J| match v {
            J::Null => "null".to_owned(),
            J::Bool(b) => b.to_string(),
            J::Num(n) => n.to_string(),
            J::Str(s) => serde_json::to_string(s).unwrap_or_default(),
            J::Arr(_) | J::Obj(_) => String::new(),
        };
        let (open, close, items): (char, char, Vec<(Option<&str>, &J)>) = match v {
            J::Arr(a) => ('[', ']', a.iter().map(|x| (None, x)).collect()),
            J::Obj(o) => (
                '{',
                '}',
                o.iter().map(|(k, x)| (Some(k.as_str()), x)).collect(),
            ),
            other => {
                out.push_str(&scalar(other));
                return;
            }
        };
        out.push(open);
        if items.is_empty() {
            out.push(close);
            return;
        }
        for (i, (k, x)) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if let Some(ind) = &self.indent {
                out.push_str(self.newline);
                out.push_str(&ind.repeat(depth + 1));
            }
            if let Some(k) = k {
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                if self.indent.is_some() {
                    out.push(' ');
                }
            }
            self.write(x, depth + 1, out);
        }
        if let Some(ind) = &self.indent {
            out.push_str(self.newline);
            out.push_str(&ind.repeat(depth));
        }
        out.push(close);
    }
}

/// The three-way text merge Lode uses everywhere: by lines, and, when that conflicts on a file
/// `[merge].json` covers, key by key. `Err` holds the line merge with conflict markers.
pub fn merge3(
    path: &lode_core::DepotPath,
    config: Option<&lode_core::LodeConfig>,
    base: &[u8],
    mine: &[u8],
    theirs: &[u8],
) -> Result<Vec<u8>, Vec<u8>> {
    let mut opts = diffy::MergeOptions::new();
    opts.set_conflict_style(diffy::ConflictStyle::Merge)
        .set_incomplete_hunk_style(diffy::IncompleteHunkStyle::Git);
    match opts.merge_bytes(base, mine, theirs) {
        Ok(m) => Ok(m),
        Err(conflicted) => merge_for(path, config, base, mine, theirs).ok_or(conflicted),
    }
}

/// The structured merge for `path` under the branch configuration (`[merge].json`, or its
/// defaults without a `.lodeconfig`), when one applies and succeeds.
pub fn merge_for(
    path: &lode_core::DepotPath,
    config: Option<&lode_core::LodeConfig>,
    base: &[u8],
    mine: &[u8],
    theirs: &[u8],
) -> Option<Vec<u8>> {
    let json = match config {
        Some(c) => c.merge_json.matches(path),
        None => lode_core::LodeConfig::default().merge_json.matches(path),
    };
    if json {
        merge_json(base, mine, theirs)
    } else {
        None
    }
}

/// Merges three versions of a JSON file key by key. `None` when any side is not JSON or the
/// same value was changed differently on both sides.
pub fn merge_json(base: &[u8], mine: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    let parse = |b: &[u8]| serde_json::from_slice::<J>(b).ok();
    let base_v = if base.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        Some(parse(base)?)
    };
    let (m, t) = (parse(mine)?, parse(theirs)?);
    let merged = merge(base_v.as_ref(), &m, &t)?;
    let style = Style::of(std::str::from_utf8(mine).ok()?);
    let mut out = String::new();
    style.write(&merged, 0, &mut out);
    if style.final_newline {
        out.push_str(style.newline);
    }
    Some(out.into_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::merge_json;

    fn m(base: &str, mine: &str, theirs: &str) -> Option<String> {
        merge_json(base.as_bytes(), mine.as_bytes(), theirs.as_bytes())
            .map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn disjoint_additions_keep_order_and_layout() {
        let base = "{\n    \"a\": 1,\n    \"z\": 2\n}\n";
        let mine = "{\n    \"a\": 1,\n    \"m\": true,\n    \"z\": 2\n}\n";
        let theirs = "{\n    \"a\": 1,\n    \"z\": 2,\n    \"t\": \"x\"\n}\n";
        assert_eq!(
            m(base, mine, theirs).unwrap(),
            "{\n    \"a\": 1,\n    \"m\": true,\n    \"z\": 2,\n    \"t\": \"x\"\n}\n"
        );
    }

    #[test]
    fn nested_objects_and_the_same_change_twice() {
        let base = r#"{"p":{"a":1,"b":2},"q":0}"#;
        let mine = r#"{"p":{"a":10,"b":2},"q":5}"#;
        let theirs = r#"{"p":{"a":1,"b":20},"q":5}"#;
        assert_eq!(
            m(base, mine, theirs).unwrap(),
            r#"{"p":{"a":10,"b":20},"q":5}"#
        );
    }

    #[test]
    fn conflicts_and_bad_input() {
        // The same scalar changed differently.
        assert!(m(r#"{"a":1}"#, r#"{"a":2}"#, r#"{"a":3}"#).is_none());
        // Arrays merge as a whole.
        assert!(m(r#"{"a":[1]}"#, r#"{"a":[1,2]}"#, r#"{"a":[0,1]}"#).is_none());
        // Changed here, removed there.
        assert!(m(r#"{"a":1,"b":1}"#, r#"{"a":2,"b":1}"#, r#"{"b":1}"#).is_none());
        // Not JSON.
        assert!(m(r#"{"a":1}"#, "{oops", r#"{"a":1}"#).is_none());
        // Removed there, unchanged here: removed.
        assert_eq!(
            m(r#"{"a":1,"b":1}"#, r#"{"a":1,"b":2}"#, r#"{"b":1}"#).unwrap(),
            r#"{"b":2}"#
        );
    }

    #[test]
    fn crlf_tabs_and_no_base() {
        let mine = "{\r\n\t\"a\": [\r\n\t\t1\r\n\t]\r\n}";
        let theirs = "{\"b\": {}}";
        assert_eq!(
            m("", mine, theirs).unwrap(),
            "{\r\n\t\"a\": [\r\n\t\t1\r\n\t],\r\n\t\"b\": {}\r\n}"
        );
    }
}
