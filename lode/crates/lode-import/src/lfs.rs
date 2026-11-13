//! Git LFS pointers: a small text file that stands for content stored elsewhere. When the
//! content is in the repository's local LFS store, the import uses it instead of the pointer.

use std::path::{Path, PathBuf};

/// Pointers are small; anything larger is ordinary content.
pub const MAX_POINTER_SIZE: u64 = 1024;

const VERSION_LINE: &str = "version https://git-lfs.github.com/spec/v1";

/// The object a pointer names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    /// SHA-256 of the content, lowercase hex.
    pub oid: String,
    /// Size of the content.
    pub size: u64,
}

/// Parses `text` as an LFS pointer.
pub fn parse(text: &[u8]) -> Option<Pointer> {
    let text = std::str::from_utf8(text).ok()?;
    let mut lines = text.lines();
    if lines.next()? != VERSION_LINE {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in lines {
        if let Some(v) = line.strip_prefix("oid sha256:") {
            if v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
                oid = Some(v.to_ascii_lowercase());
            }
        } else if let Some(v) = line.strip_prefix("size ") {
            size = v.parse().ok();
        }
    }
    Some(Pointer {
        oid: oid?,
        size: size?,
    })
}

/// Where the local LFS store keeps the pointer's content, if it is there with the right size.
pub fn local_object(git_dir: &Path, pointer: &Pointer) -> Option<PathBuf> {
    let path = git_dir
        .join("lfs")
        .join("objects")
        .join(&pointer.oid[..2])
        .join(&pointer.oid[2..4])
        .join(&pointer.oid);
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.len() == pointer.size).then_some(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn pointers_parse() {
        let oid = "a".repeat(64);
        let text = format!("{VERSION_LINE}\noid sha256:{oid}\nsize 12345\n");
        assert_eq!(
            parse(text.as_bytes()),
            Some(Pointer {
                oid: oid.clone(),
                size: 12345
            })
        );
        assert_eq!(parse(b"hello"), None);
        assert_eq!(parse(format!("{VERSION_LINE}\nsize 1\n").as_bytes()), None);
        assert_eq!(
            parse(format!("{VERSION_LINE}\noid sha256:abc\nsize 1\n").as_bytes()),
            None
        );
    }

    #[test]
    fn local_objects_need_the_right_size() {
        let dir = tempfile::tempdir().unwrap();
        let oid = "0123".to_owned() + &"f".repeat(60);
        let p = Pointer {
            oid: oid.clone(),
            size: 3,
        };
        assert_eq!(local_object(dir.path(), &p), None);
        let path = dir.path().join("lfs/objects/01/23").join(&oid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(local_object(dir.path(), &p), Some(path));
        let wrong = Pointer { oid, size: 4 };
        assert_eq!(local_object(dir.path(), &wrong), None);
    }
}
