//! Reading a Git repository through the `git` command: the first-parent history of a branch,
//! the changes of each commit, and blob content through one `git cat-file --batch` process.
//!
//! Only plumbing output is parsed (`-z` forms), so paths with any characters and messages in
//! any language come through unchanged.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::ImportError;

/// A file mode as Git records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Regular file.
    File,
    /// Executable file.
    Executable,
    /// Symbolic link; the blob is the target.
    Symlink,
    /// A submodule commit; it has no content in this repository.
    Submodule,
    /// No entry (the side of an add or delete that is absent).
    None,
}

impl Mode {
    fn parse(text: &str) -> Result<Mode, ImportError> {
        Ok(match text {
            "000000" => Mode::None,
            "100644" | "100664" => Mode::File,
            "100755" => Mode::Executable,
            "120000" => Mode::Symlink,
            "160000" => Mode::Submodule,
            other => return Err(ImportError::Protocol(format!("unknown file mode {other}"))),
        })
    }

    /// True for content stored in this repository: files and symlinks.
    pub fn has_content(self) -> bool {
        matches!(self, Mode::File | Mode::Executable | Mode::Symlink)
    }
}

/// One commit of the imported history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    /// Full commit id.
    pub id: String,
    /// Author name.
    pub author_name: String,
    /// Author email.
    pub author_email: String,
    /// Author date, strict ISO 8601 (RFC 3339) with the author's offset.
    pub author_date: String,
    /// Full message, without trailing newlines.
    pub message: String,
}

/// One changed path between two commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitChange {
    /// The path is new.
    Add {
        /// Path.
        path: String,
        /// Its mode.
        mode: Mode,
        /// Blob id.
        blob: String,
    },
    /// The content, mode or kind changed.
    Modify {
        /// Path.
        path: String,
        /// Mode before.
        old_mode: Mode,
        /// Mode after.
        mode: Mode,
        /// Blob id after.
        blob: String,
    },
    /// The path is gone.
    Delete {
        /// Path.
        path: String,
        /// Mode before.
        old_mode: Mode,
    },
    /// The path moved, maybe with edits.
    Rename {
        /// Path before.
        from: String,
        /// Mode before.
        old_mode: Mode,
        /// Path after.
        path: String,
        /// Mode after.
        mode: Mode,
        /// Blob id after.
        blob: String,
    },
}

/// A Git repository on disk.
#[derive(Debug, Clone)]
pub struct GitRepo {
    /// The folder given; `git -C` runs there.
    root: PathBuf,
    /// The repository's `.git` folder (or the folder itself when bare).
    git_dir: PathBuf,
}

impl GitRepo {
    /// Opens the repository at or above `path`. Fails when `git` cannot be run or `path` is
    /// not in a repository.
    pub async fn open(path: &Path) -> Result<GitRepo, ImportError> {
        let probe = Command::new("git")
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .await;
        match probe {
            Ok(out) if out.status.success() => {}
            _ => return Err(ImportError::GitMissing),
        }
        let repo = GitRepo {
            root: path.to_owned(),
            git_dir: PathBuf::new(),
        };
        let out = repo
            .run(&["rev-parse", "--absolute-git-dir"])
            .await
            .map_err(|_| ImportError::NotARepository(path.to_owned()))?;
        let git_dir = PathBuf::from(String::from_utf8_lossy(&out).trim());
        Ok(GitRepo { git_dir, ..repo })
    }

    /// The repository's `.git` folder.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn command(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(&self.root)
            // Never ask for anything; this runs unattended.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        c
    }

    async fn run(&self, args: &[&str]) -> Result<Vec<u8>, ImportError> {
        let out = self
            .command()
            .args(args)
            .output()
            .await
            .map_err(|_| ImportError::GitMissing)?;
        if !out.status.success() {
            return Err(ImportError::Git {
                command: format!("git {}", args.join(" ")),
                message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(out.stdout)
    }

    /// The commit `branch` points at.
    pub async fn resolve(&self, branch: &str) -> Result<String, ImportError> {
        let spec = format!("{branch}^{{commit}}");
        let out = self
            .run(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &spec,
            ])
            .await
            .map_err(|_| ImportError::UnknownBranch(branch.to_owned()))?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// The first-parent history ending at commit `tip`, oldest first.
    pub async fn first_parent(&self, tip: &str) -> Result<Vec<GitCommit>, ImportError> {
        let out = self
            .run(&[
                "log",
                "--first-parent",
                "--reverse",
                "-z",
                "--encoding=UTF-8",
                "--format=%H%n%an%n%ae%n%aI%n%B",
                tip,
                "--",
            ])
            .await?;
        let text = String::from_utf8_lossy(&out);
        let mut commits = Vec::new();
        for record in text.split('\0') {
            let record = record.trim_start_matches('\n');
            if record.is_empty() {
                continue;
            }
            let mut lines = record.splitn(5, '\n');
            let mut next = || lines.next().unwrap_or_default().to_owned();
            let (id, author_name, author_email, author_date) = (next(), next(), next(), next());
            let message = next().trim_end().to_owned();
            if id.len() < 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ImportError::Protocol(format!(
                    "unexpected log record {id:?}"
                )));
            }
            commits.push(GitCommit {
                id,
                author_name,
                author_email,
                author_date,
                message,
            });
        }
        Ok(commits)
    }

    /// The changes from commit `from` (none: the empty tree) to commit `to`, with renames
    /// detected.
    pub async fn changes(
        &self,
        from: Option<&str>,
        to: &str,
    ) -> Result<Vec<GitChange>, ImportError> {
        let mut args = vec!["diff-tree", "-r", "-z", "-M", "--no-commit-id"];
        match from {
            Some(f) => {
                args.push(f);
                args.push(to);
            }
            None => {
                args.push("--root");
                args.push(to);
            }
        }
        let out = self.run(&args).await?;
        parse_raw(&out)
    }

    /// Starts a `git cat-file --batch` reader.
    pub fn blobs(&self) -> Result<BlobReader, ImportError> {
        let mut child = self
            .command()
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ImportError::GitMissing)?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(ImportError::Protocol("cat-file has no pipes".into()));
        };
        Ok(BlobReader {
            _child: child,
            input,
            output: BufReader::new(output),
        })
    }
}

/// Parses `git diff-tree -r -z` raw output.
fn parse_raw(out: &[u8]) -> Result<Vec<GitChange>, ImportError> {
    let text = std::str::from_utf8(out)
        .map_err(|_| ImportError::Protocol("a path in the repository is not UTF-8".into()))?;
    let mut fields = text.split('\0');
    let mut changes = Vec::new();
    while let Some(head) = fields.next() {
        if head.is_empty() {
            continue;
        }
        let bad = || ImportError::Protocol(format!("unexpected diff line {head:?}"));
        let head = head.strip_prefix(':').ok_or_else(bad)?;
        let parts: Vec<&str> = head.split(' ').collect();
        let [old_mode, new_mode, _old_blob, new_blob, status] = parts[..] else {
            return Err(bad());
        };
        let old_mode = Mode::parse(old_mode)?;
        let mode = Mode::parse(new_mode)?;
        let blob = new_blob.to_owned();
        let mut path = || fields.next().map(str::to_owned).ok_or_else(bad);
        let change = match status.chars().next() {
            Some('A') => GitChange::Add {
                path: path()?,
                mode,
                blob,
            },
            Some('M') | Some('T') => GitChange::Modify {
                path: path()?,
                old_mode,
                mode,
                blob,
            },
            Some('D') => GitChange::Delete {
                path: path()?,
                old_mode,
            },
            Some('R') => {
                let from = path()?;
                GitChange::Rename {
                    from,
                    old_mode,
                    path: path()?,
                    mode,
                    blob,
                }
            }
            _ => return Err(bad()),
        };
        changes.push(change);
    }
    Ok(changes)
}

/// Reads blobs through one `git cat-file --batch` process.
pub struct BlobReader {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl BlobReader {
    /// Copies blob `id` into `out`, feeding every byte to `hasher` too. Returns its size.
    pub async fn copy<W: AsyncWrite + Unpin>(
        &mut self,
        id: &str,
        out: &mut W,
        hasher: &mut blake3::Hasher,
    ) -> Result<u64, ImportError> {
        self.input.write_all(format!("{id}\n").as_bytes()).await?;
        self.input.flush().await?;
        let mut header = String::new();
        self.output.read_line(&mut header).await?;
        let bad = || ImportError::Protocol(format!("unexpected cat-file answer {header:?}"));
        let mut parts = header.split_whitespace();
        let (Some(_), Some(kind), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(bad());
        };
        if kind != "blob" {
            return Err(bad());
        }
        let size: u64 = size.parse().map_err(|_| bad())?;
        let mut left = size;
        let mut buf = vec![0u8; 64 * 1024];
        while left > 0 {
            let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            let n = self.output.read(&mut buf[..want]).await?;
            if n == 0 {
                return Err(ImportError::Protocol("cat-file ended early".into()));
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).await?;
            left -= n as u64;
        }
        let mut newline = [0u8; 1];
        self.output.read_exact(&mut newline).await?;
        out.flush().await?;
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn raw_output_parses() {
        let raw = b":000000 100644 0000000 1111111 A\0new.txt\0\
:100644 100755 2222222 2222222 M\0tool.sh\0\
:100644 000000 3333333 0000000 D\0gone.txt\0\
:100644 100644 4444444 5555555 R087\0old name.txt\0dir/new name.txt\0\
:120000 100644 6666666 7777777 T\0link\0";
        let c = parse_raw(raw).unwrap();
        assert_eq!(c.len(), 5);
        assert_eq!(
            c[3],
            GitChange::Rename {
                from: "old name.txt".into(),
                old_mode: Mode::File,
                path: "dir/new name.txt".into(),
                mode: Mode::File,
                blob: "5555555".into(),
            }
        );
        assert!(matches!(
            &c[4],
            GitChange::Modify {
                old_mode: Mode::Symlink,
                mode: Mode::File,
                ..
            }
        ));
        assert!(parse_raw(b":100644 100644 1 2 X\0p\0").is_err());
    }
}
