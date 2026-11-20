//! `lode://` links: a revision, a file at a revision or on a branch, or a shelf.
//!
//! ```text
//! lode://HOST[:PORT]/DEPOT/rREV
//! lode://HOST[:PORT]/DEPOT/PATH@REV        (or PATH?branch=NAME, or both)
//! lode://HOST[:PORT]/DEPOT/shelf/ID
//! ```
//!
//! The server is `https://HOST[:PORT]`, or `http://` with `?http=1`. Path segments and the
//! branch are percent-encoded. Parsing is strict: anything that is not exactly one of these
//! forms is refused, and a refused link is never acted on. `tests/link_cases.json` lists the
//! cases.

use serde::{Deserialize, Serialize};

/// Longest link accepted.
pub const MAX_LINK: usize = 2048;

/// What a link opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Link {
    /// One revision.
    Revision {
        /// `https://host[:port]` or `http://host[:port]`.
        server: String,
        /// Depot name.
        depot: String,
        /// Revision number.
        rev: u64,
    },
    /// A file at a revision, or at the head of a branch.
    File {
        /// `https://host[:port]` or `http://host[:port]`.
        server: String,
        /// Depot name.
        depot: String,
        /// Depot path.
        path: String,
        /// Revision; the head of `branch` when None.
        rev: Option<u64>,
        /// Branch; the workspace's when None.
        branch: Option<String>,
    },
    /// One shelf.
    Shelf {
        /// `https://host[:port]` or `http://host[:port]`.
        server: String,
        /// Depot name.
        depot: String,
        /// Shelf id.
        id: u64,
    },
}

/// Why a link was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkError(pub String);

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LinkError {}

fn bad(why: impl Into<String>) -> LinkError {
    LinkError(why.into())
}

/// Characters that may appear as they are in a path segment or a query value.
fn plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@%".contains(c)
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// Percent-encodes everything but the unreserved characters.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> Result<String, LinkError> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text
                .get(i + 1..i + 3)
                .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| bad(format!("bad percent escape in `{text}`")))?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| bad("bad percent escape"))?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| bad(format!("`{text}` is not UTF-8 text")))
}

/// `r123` (or `123` when `r` is optional) with no leading zero, as a number from 1 to 2^53 - 1.
fn number(text: &str, need_r: bool) -> Option<u64> {
    let digits = match text.strip_prefix('r') {
        Some(d) => d,
        None if need_r => return None,
        None => text,
    };
    if digits.is_empty()
        || digits.len() > 19
        || digits.starts_with('0')
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // The window holds numbers as doubles, so both stop at 2^53 - 1.
    digits.parse().ok().filter(|n| *n <= MAX_NUMBER)
}

/// Largest revision or shelf id a link carries.
const MAX_NUMBER: u64 = (1 << 53) - 1;

fn check_depot(name: &str) -> Result<(), LinkError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(bad(format!("`{name}` is not a depot name")))
    }
}

fn check_branch(name: &str) -> Result<(), LinkError> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
        && !name
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
    if ok {
        Ok(())
    } else {
        Err(bad(format!("`{name}` is not a branch name")))
    }
}

/// One decoded path segment: not empty, `.` or `..`, and free of characters depot paths
/// refuse.
fn segment(raw: &str) -> Result<String, LinkError> {
    let s = decode(raw)?;
    if s.is_empty() || s == "." || s == ".." {
        return Err(bad("a path has an empty, `.` or `..` part"));
    }
    if s.chars()
        .any(|c| c.is_control() || "/\\<>:\"|?*".contains(c))
    {
        return Err(bad(format!(
            "`{s}` has a character depot paths cannot hold"
        )));
    }
    Ok(s)
}

/// `host[:port]`, lowercased; a name, an IPv4 address or a bracketed IPv6 address.
fn authority(text: &str) -> Result<String, LinkError> {
    if text.contains('@') {
        return Err(bad("links do not carry user names"));
    }
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(|| bad("unclosed `[`"))?;
        let inner = &rest[..end];
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return Err(bad(format!("`{inner}` is not an IPv6 address")));
        }
        let after = &rest[end + 1..];
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or_else(|| bad("bad host"))?),
        };
        (format!("[{}]", inner.to_ascii_lowercase()), port)
    } else {
        let (h, p) = match text.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (text, None),
        };
        let ok = !h.is_empty()
            && h.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if !ok {
            return Err(bad(format!("`{h}` is not a host name")));
        }
        (h.to_ascii_lowercase(), p)
    };
    match port {
        None => Ok(host),
        Some(p) => {
            let n = (!p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u32>().ok())
                .flatten()
                .filter(|n| (1..=65535).contains(n))
                .ok_or_else(|| bad(format!("`{p}` is not a port")))?;
            Ok(format!("{host}:{n}"))
        }
    }
}

/// Reads a `lode://` link, refusing anything that is not exactly one of the three forms.
pub fn parse(text: &str) -> Result<Link, LinkError> {
    if text.len() > MAX_LINK {
        return Err(bad("the link is too long"));
    }
    if !text.is_ascii() || text.chars().any(|c| c.is_ascii_control() || c == ' ') {
        return Err(bad(
            "a link has no spaces or characters that are not percent-encoded",
        ));
    }
    let rest = text
        .get(..7)
        .filter(|s| s.eq_ignore_ascii_case("lode://"))
        .map(|_| &text[7..])
        .ok_or_else(|| bad("not a lode:// link"))?;
    if rest.contains('#') {
        return Err(bad("links have no `#` part"));
    }
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let (auth, path) = rest
        .split_once('/')
        .ok_or_else(|| bad("the link names no depot"))?;
    let host = authority(auth)?;

    // The query: `http=1` and, for a file, `branch=NAME`, each at most once.
    let mut http = false;
    let mut branch: Option<String> = None;
    if let Some(q) = query {
        if q.is_empty() {
            return Err(bad("empty query"));
        }
        for pair in q.split('&') {
            let (k, v) = pair.split_once('=').ok_or_else(|| bad("bad query"))?;
            if !v.chars().all(|c| plain(c) || c == '/' || c == '?') {
                return Err(bad("bad query"));
            }
            match k {
                "http" if !http && v == "1" => http = true,
                "branch" if branch.is_none() => {
                    let name = decode(v)?;
                    check_branch(&name)?;
                    branch = Some(name);
                }
                _ => return Err(bad(format!("`{pair}` is not part of a lode link"))),
            }
        }
    }
    let server = format!("{}://{host}", if http { "http" } else { "https" });

    let raw: Vec<&str> = path.split('/').collect();
    if raw.iter().any(|s| !s.chars().all(plain)) {
        return Err(bad("bad character in the path"));
    }
    let depot = decode(raw[0])?;
    check_depot(&depot)?;
    let parts = &raw[1..];
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(bad("the link names nothing to open"));
    }
    let last = parts[parts.len() - 1];
    let at = last.matches('@').count();
    if parts[..parts.len() - 1].iter().any(|p| p.contains('@')) || at > 1 {
        return Err(bad("`@` must come once, before the revision"));
    }
    if at == 0 && branch.is_none() {
        if let [one] = parts {
            if let Some(rev) = number(one, true) {
                return Ok(Link::Revision { server, depot, rev });
            }
        }
        if let ["shelf", id] = parts {
            let id = number(id, false)
                .filter(|_| !id.starts_with('r'))
                .ok_or_else(|| bad(format!("`{id}` is not a shelf id")))?;
            return Ok(Link::Shelf { server, depot, id });
        }
        return Err(bad("a file link needs @REV or ?branch=NAME"));
    }
    // Without `@REV` these would read as a revision or a shelf link too.
    if at == 0
        && (matches!(parts, [one] if number(one, true).is_some())
            || parts.first() == Some(&"shelf"))
    {
        return Err(bad("a file link to this path needs @REV"));
    }
    let (name, rev) = match last.split_once('@') {
        Some((name, r)) => (
            name,
            Some(number(r, false).ok_or_else(|| bad(format!("`{r}` is not a revision")))?),
        ),
        None => (last, None),
    };
    if name.is_empty() {
        return Err(bad("the link names no file"));
    }
    let mut segs = Vec::with_capacity(parts.len());
    for p in &parts[..parts.len() - 1] {
        segs.push(segment(p)?);
    }
    segs.push(segment(name)?);
    Ok(Link::File {
        server,
        depot,
        path: segs.join("/"),
        rev,
        branch,
    })
}

/// Splits `https://host[:port][/]` into the host part and whether it is plain http.
fn server_host(server: &str) -> Result<(String, bool), LinkError> {
    let (http, rest) = if let Some(r) = server.strip_prefix("https://") {
        (false, r)
    } else if let Some(r) = server.strip_prefix("http://") {
        (true, r)
    } else {
        return Err(bad(format!("`{server}` is not an http or https address")));
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') {
        return Err(bad(
            "links need a server at the root of its address, without a path",
        ));
    }
    Ok((authority(rest)?, http))
}

/// Writes the link to `link`.
pub fn build(link: &Link) -> Result<String, LinkError> {
    let (server, depot) = match link {
        Link::Revision { server, depot, .. }
        | Link::File { server, depot, .. }
        | Link::Shelf { server, depot, .. } => (server, depot),
    };
    check_depot(depot)?;
    let (host, http) = server_host(server)?;
    let mut query: Vec<String> = Vec::new();
    let tail = match link {
        Link::Revision { rev, .. } => format!("r{rev}"),
        Link::Shelf { id, .. } => format!("shelf/{id}"),
        Link::File {
            path, rev, branch, ..
        } => {
            if rev.is_none() && branch.is_none() {
                return Err(bad("a file link needs a revision or a branch"));
            }
            let mut segs = Vec::new();
            for s in path.split('/') {
                segs.push(encode(&segment(&encode(s))?));
            }
            if let Some(b) = branch {
                check_branch(b)?;
                query.push(format!("branch={}", encode(b)));
            }
            match rev {
                Some(r) => format!("{}@{r}", segs.join("/")),
                None => segs.join("/"),
            }
        }
    };
    if http {
        query.push("http=1".into());
    }
    let q = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    let text = format!("lode://{host}/{depot}/{tail}{q}");
    // Never hand out a link that would be refused, such as a branch link to a file named `r1`.
    parse(&text)?;
    Ok(text)
}

/// The first argument that starts with `lode://` (Windows and Linux start the app with the
/// link as an argument). It is not parsed here: the window parses it and says why when it is
/// refused.
pub fn from_args<I: IntoIterator<Item = S>, S: AsRef<str>>(args: I) -> Option<String> {
    args.into_iter().map(|a| a.as_ref().to_owned()).find(|a| {
        a.get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("lode://"))
    })
}
