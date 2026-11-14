//! Reading a Subversion repository through the `svn` command (T121): the revisions that
//! change one path, file content and properties at a revision, and directory listings.
//!
//! Everything is read with `--xml` where svn offers it, and always `--non-interactive`, so
//! the import never waits for a prompt; credentials are whatever `svn` already has.

use std::path::Path;
use std::process::Stdio;

use lode_core::ObjectId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::xml::{self, Element};
use crate::ImportError;

/// What a log entry says happened to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvnAction {
    /// Added (maybe copied).
    Add,
    /// Content or properties changed.
    Modify,
    /// Deleted.
    Delete,
    /// Replaced: deleted and added again in the same revision.
    Replace,
}

/// The kind of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvnKind {
    /// A file.
    File,
    /// A directory.
    Dir,
    /// Not said (very old servers).
    Unknown,
}

/// One changed path of a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnPath {
    /// Repository path, starting with `/`.
    pub path: String,
    /// What happened.
    pub action: SvnAction,
    /// File or directory.
    pub kind: SvnKind,
    /// Copied from this repository path at this revision.
    pub copy_from: Option<(String, u64)>,
    /// Content changed.
    pub text_mods: bool,
    /// Properties changed.
    pub prop_mods: bool,
}

/// One revision of the imported history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnRevision {
    /// Revision number.
    pub rev: u64,
    /// Author, when recorded.
    pub author: Option<String>,
    /// Date (ISO 8601, UTC), when recorded.
    pub date: Option<String>,
    /// Log message.
    pub message: String,
    /// Changed paths, in every part of the repository.
    pub paths: Vec<SvnPath>,
}

/// The properties of a file that matter to a depot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SvnProps {
    /// `svn:executable` is set.
    pub executable: bool,
    /// `svn:special` is set: the content is `link TARGET`.
    pub special: bool,
}

/// A repository path and the URL it is read from.
#[derive(Debug, Clone)]
pub struct SvnRepo {
    /// The repository root URL, without a trailing slash.
    root: String,
    /// The imported path: empty for the root, otherwise `/trunk` and the like.
    base: String,
    /// The newest revision when opened.
    youngest: u64,
}

fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:".contains(&b);
        if keep {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        char::from(b)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            if let (Some(h), Some(l)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            ) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Longest `svn:special` content read; symlink targets are far shorter.
const MAX_SPECIAL: u64 = 64 * 1024;

fn num(e: &Element, attr: &str) -> Result<u64, ImportError> {
    e.attr(attr)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| ImportError::Protocol(format!("svn printed no {attr} on <{}>", e.name)))
}

impl SvnRepo {
    /// Opens the directory at `url`. Fails when `svn` cannot be run or the URL is not a
    /// directory of a repository.
    pub async fn open(url: &str) -> Result<SvnRepo, ImportError> {
        let probe = Command::new("svn")
            .args(["--version", "--quiet"])
            .stdin(Stdio::null())
            .output()
            .await;
        match probe {
            Ok(out) if out.status.success() => {}
            _ => return Err(ImportError::SvnMissing),
        }
        let url = url.trim_end_matches('/');
        let out = run(&["info", "--xml", url]).await.map_err(|e| match e {
            ImportError::Svn { message, .. } => ImportError::NotSvn {
                url: url.to_owned(),
                message,
            },
            other => other,
        })?;
        let doc = xml::parse(&out)?;
        let entry = doc
            .child("entry")
            .ok_or_else(|| ImportError::Protocol("svn info printed no entry".into()))?;
        if entry.attr("kind") != Some("dir") {
            return Err(ImportError::NotSvn {
                url: url.to_owned(),
                message: "it is not a directory".into(),
            });
        }
        let root = entry
            .child("repository")
            .and_then(|r| r.child_text("root"))
            .ok_or_else(|| ImportError::Protocol("svn info printed no repository root".into()))?
            .trim_end_matches('/')
            .to_owned();
        let relative = entry
            .child_text("relative-url")
            .ok_or_else(|| ImportError::Protocol("svn info printed no relative URL".into()))?;
        let base = percent_decode(relative.trim_start_matches('^'));
        let base = base.trim_end_matches('/').to_owned();
        Ok(SvnRepo {
            root,
            base,
            youngest: num(entry, "revision")?,
        })
    }

    /// The newest revision when the repository was opened.
    pub fn youngest(&self) -> u64 {
        self.youngest
    }

    /// The imported path, `/trunk` and the like; empty for the repository root.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `path` relative to the imported path: `Some("")` for the path itself, `None` when
    /// outside it.
    pub fn relative<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.base.is_empty() {
            return Some(path.trim_start_matches('/'));
        }
        let rest = path.strip_prefix(self.base.as_str())?;
        if rest.is_empty() {
            Some("")
        } else {
            rest.strip_prefix('/')
        }
    }

    /// The URL of a repository path at a revision.
    fn url_at(&self, path: &str, rev: u64) -> String {
        format!("{}{}@{rev}", self.root, percent_encode(path))
    }

    /// The URL of an imported path (relative to the imported directory) at a revision.
    pub fn url_of(&self, rel: &str, rev: u64) -> String {
        if rel.is_empty() {
            self.url_at(&self.base, rev)
        } else {
            self.url_at(&format!("{}/{rel}", self.base), rev)
        }
    }

    /// The revisions that change the imported path, oldest first, up to the newest when
    /// opened. A copy that created the path is the first; history from before it is not
    /// followed.
    pub async fn log(&self) -> Result<Vec<SvnRevision>, ImportError> {
        let url = self.url_of("", self.youngest);
        let range = format!("1:{}", self.youngest);
        let out = run(&["log", "--xml", "-v", "--stop-on-copy", "-r", &range, &url]).await?;
        let doc = xml::parse(&out)?;
        let mut revs = Vec::new();
        for e in doc.all("logentry") {
            let mut paths = Vec::new();
            if let Some(ps) = e.child("paths") {
                for p in ps.all("path") {
                    let action = match p.attr("action") {
                        Some("A") => SvnAction::Add,
                        Some("M") => SvnAction::Modify,
                        Some("D") => SvnAction::Delete,
                        Some("R") => SvnAction::Replace,
                        other => {
                            return Err(ImportError::Protocol(format!(
                                "unknown svn action {other:?}"
                            )))
                        }
                    };
                    let kind = match p.attr("kind") {
                        Some("file") => SvnKind::File,
                        Some("dir") => SvnKind::Dir,
                        _ => SvnKind::Unknown,
                    };
                    let copy_from = match (p.attr("copyfrom-path"), p.attr("copyfrom-rev")) {
                        (Some(path), Some(rev)) => Some((
                            path.to_owned(),
                            rev.parse().map_err(|_| {
                                ImportError::Protocol(format!("bad copyfrom-rev {rev}"))
                            })?,
                        )),
                        _ => None,
                    };
                    paths.push(SvnPath {
                        path: p.text.clone(),
                        action,
                        kind,
                        copy_from,
                        text_mods: p.attr("text-mods") != Some("false"),
                        prop_mods: p.attr("prop-mods") != Some("false"),
                    });
                }
            }
            revs.push(SvnRevision {
                rev: num(e, "revision")?,
                author: e.child_text("author").map(str::to_owned),
                date: e.child_text("date").map(str::to_owned),
                message: e
                    .child_text("msg")
                    .unwrap_or_default()
                    .trim_end()
                    .to_owned(),
                paths,
            });
        }
        revs.sort_by_key(|r| r.rev);
        Ok(revs)
    }

    /// The kind of an imported path at a revision.
    pub async fn kind(&self, rel: &str, rev: u64) -> Result<SvnKind, ImportError> {
        let out = run(&["info", "--xml", &self.url_of(rel, rev)]).await?;
        let doc = xml::parse(&out)?;
        Ok(match doc.child("entry").and_then(|e| e.attr("kind")) {
            Some("file") => SvnKind::File,
            Some("dir") => SvnKind::Dir,
            _ => SvnKind::Unknown,
        })
    }

    /// Every file under an imported directory at a revision, relative to that directory.
    pub async fn files_under(&self, rel: &str, rev: u64) -> Result<Vec<String>, ImportError> {
        let out = run(&["list", "-R", "--xml", &self.url_of(rel, rev)]).await?;
        let doc = xml::parse(&out)?;
        let mut files = Vec::new();
        for list in doc.all("list") {
            for e in list.all("entry") {
                if e.attr("kind") == Some("file") {
                    if let Some(name) = e.child_text("name") {
                        files.push(name.to_owned());
                    }
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// The properties of an imported file at a revision.
    pub async fn props(&self, rel: &str, rev: u64) -> Result<SvnProps, ImportError> {
        let out = run(&["proplist", "--xml", &self.url_of(rel, rev)]).await?;
        let doc = xml::parse(&out)?;
        let mut props = SvnProps::default();
        for t in doc.all("target") {
            for p in t.all("property") {
                match p.attr("name") {
                    Some("svn:executable") => props.executable = true,
                    Some("svn:special") => props.special = true,
                    _ => {}
                }
            }
        }
        Ok(props)
    }

    /// Directories under the imported path that have `svn:externals` now, relative to it.
    pub async fn externals(&self) -> Result<Vec<String>, ImportError> {
        let url = self.url_of("", self.youngest);
        let out = run(&["propget", "-R", "--xml", "svn:externals", &url]).await?;
        let doc = xml::parse(&out)?;
        let prefix = format!("{}{}", self.root, percent_encode(&self.base));
        let mut dirs = Vec::new();
        for t in doc.all("target") {
            let Some(path) = t.attr("path") else { continue };
            let path = path.split('@').next().unwrap_or(path);
            let rest = path.strip_prefix(&prefix).unwrap_or(path);
            let rel = percent_decode(rest.trim_start_matches('/'));
            dirs.push(if rel.is_empty() { ".".to_owned() } else { rel });
        }
        dirs.sort();
        Ok(dirs)
    }

    /// Writes an imported file's content at a revision to `dest`, and returns its id and
    /// size. With `special`, the `link ` prefix svn keeps for symlinks is dropped, so the
    /// content is the link target.
    pub async fn cat(
        &self,
        rel: &str,
        rev: u64,
        special: bool,
        dest: &Path,
    ) -> Result<(ObjectId, u64), ImportError> {
        let url = self.url_of(rel, rev);
        let mut child = command()
            .args(["cat", &url])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ImportError::SvnMissing)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| ImportError::Protocol("svn cat has no output".into()))?;
        let mut file = tokio::fs::File::create(dest).await?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut size = 0u64;
        if special {
            // A symlink is small: `link TARGET`. Read it whole and keep the target.
            let mut text = Vec::new();
            (&mut stdout)
                .take(MAX_SPECIAL)
                .read_to_end(&mut text)
                .await?;
            let target = text.strip_prefix(b"link ").unwrap_or(&text);
            hasher.update(target);
            file.write_all(target).await?;
            size = target.len() as u64;
        }
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let chunk = buf.get(..n).unwrap_or_default();
            hasher.update(chunk);
            file.write_all(chunk).await?;
            size += n as u64;
        }
        file.flush().await?;
        drop(file);
        let mut err = Vec::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_end(&mut err).await;
        }
        let status = child.wait().await?;
        if !status.success() {
            return Err(ImportError::Svn {
                command: format!("svn cat {url}"),
                message: String::from_utf8_lossy(&err).trim().to_owned(),
            });
        }
        Ok((ObjectId::from_bytes(*hasher.finalize().as_bytes()), size))
    }
}

fn command() -> Command {
    let mut c = Command::new("svn");
    c.args(["--non-interactive"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    c
}

async fn run(args: &[&str]) -> Result<Vec<u8>, ImportError> {
    let out = command()
        .args(args)
        .output()
        .await
        .map_err(|_| ImportError::SvnMissing)?;
    if !out.status.success() {
        return Err(ImportError::Svn {
            command: format!("svn {}", args.join(" ")),
            message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_encoded_and_decoded() {
        assert_eq!(
            percent_encode("/trunk/a b#c?.txt"),
            "/trunk/a%20b%23c%3F.txt"
        );
        assert_eq!(percent_encode("/é"), "/%C3%A9");
        assert_eq!(percent_decode("/a%20b/%C3%A9"), "/a b/é");
        assert_eq!(percent_decode("/50%"), "/50%");
    }

    #[test]
    fn paths_are_taken_relative_to_the_imported_one() {
        let r = SvnRepo {
            root: "file:///r".into(),
            base: "/trunk".into(),
            youngest: 1,
        };
        assert_eq!(r.relative("/trunk"), Some(""));
        assert_eq!(r.relative("/trunk/a/b"), Some("a/b"));
        assert_eq!(r.relative("/trunks/a"), None);
        assert_eq!(r.relative("/branches/x"), None);
        assert_eq!(r.url_of("a b", 3), "file:///r/trunk/a%20b@3");
        let root = SvnRepo {
            base: String::new(),
            ..r
        };
        assert_eq!(root.relative("/a"), Some("a"));
        assert_eq!(root.url_of("", 2), "file:///r@2");
    }
}
