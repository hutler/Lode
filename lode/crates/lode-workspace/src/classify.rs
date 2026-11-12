//! Text or binary (ARCHITECTURE §10.4): `.lodeconfig` `[files]` patterns first, otherwise
//! binary if the first 8 KiB contain a NUL byte.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use lode_core::{DepotPath, LodeConfig};

/// Bytes inspected for a NUL.
pub const SNIFF_LEN: usize = 8 * 1024;

/// How a file's content is handled by diff and merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// Line-based diff and three-way merge.
    Text,
    /// Compared whole; both-sides changes conflict.
    Binary,
}

/// Classifies by configuration alone, if the config decides. `binary` wins over `text` when
/// both match, the safe side for merging.
pub fn class_by_config(path: &DepotPath, config: Option<&LodeConfig>) -> Option<FileClass> {
    let config = config?;
    if config.binary.matches(path) {
        Some(FileClass::Binary)
    } else if config.text.matches(path) {
        Some(FileClass::Text)
    } else {
        None
    }
}

/// Classifies content from its first bytes.
pub fn class_by_content(head: &[u8]) -> FileClass {
    if head[..head.len().min(SNIFF_LEN)].contains(&0) {
        FileClass::Binary
    } else {
        FileClass::Text
    }
}

/// Classifies `path` whose content is at `local`, reading at most [`SNIFF_LEN`] bytes.
pub fn classify_file(
    path: &DepotPath,
    config: Option<&LodeConfig>,
    local: &Path,
) -> io::Result<FileClass> {
    if let Some(c) = class_by_config(path, config) {
        return Ok(c);
    }
    let mut head = Vec::with_capacity(SNIFF_LEN);
    File::open(local)?
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)?;
    Ok(class_by_content(&head))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn config_then_nul() {
        let cfg = LodeConfig::parse(
            b"[files]\nbinary = [\"**/*.dat\"]\ntext = [\"**/*.shader\", \"**/*.dat\"]\n",
        )
        .unwrap();
        let p = |s: &str| DepotPath::parse(s).unwrap();
        assert_eq!(
            class_by_config(&p("a/x.dat"), Some(&cfg)),
            Some(FileClass::Binary)
        );
        assert_eq!(
            class_by_config(&p("s.shader"), Some(&cfg)),
            Some(FileClass::Text)
        );
        assert_eq!(class_by_config(&p("s.txt"), Some(&cfg)), None);
        assert_eq!(class_by_content(b"hello\n"), FileClass::Text);
        assert_eq!(class_by_content(b"he\0llo"), FileClass::Binary);
        // A NUL after the first 8 KiB does not count.
        let mut late = vec![b'a'; SNIFF_LEN];
        late.push(0);
        assert_eq!(class_by_content(&late), FileClass::Text);

        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.shader");
        std::fs::write(&f, b"\0\0\0").unwrap();
        assert_eq!(
            classify_file(&p("s.shader"), Some(&cfg), &f).unwrap(),
            FileClass::Text
        );
        assert_eq!(
            classify_file(&p("s.shader"), None, &f).unwrap(),
            FileClass::Binary
        );
    }
}
