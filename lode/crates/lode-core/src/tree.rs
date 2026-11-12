//! Directory trees and their canonical binary encoding (ARCHITECTURE §3.2).

use crate::id::{tree_id, ObjectId};
use crate::path::{validate_segment, PathError};
use crate::varint::{self, VarintError};

/// What a tree entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A regular file; `id` is its blob id.
    File,
    /// A directory; `id` is its tree id.
    Dir,
    /// A symbolic link; `id` is the blob id of the target string.
    Symlink,
}

impl EntryKind {
    fn code(self) -> u8 {
        match self {
            EntryKind::File => 1,
            EntryKind::Dir => 2,
            EntryKind::Symlink => 3,
        }
    }

    fn from_code(c: u8) -> Option<Self> {
        match c {
            1 => Some(EntryKind::File),
            2 => Some(EntryKind::Dir),
            3 => Some(EntryKind::Symlink),
            _ => None,
        }
    }
}

/// Flag bit 0: the file is executable. Only valid on files.
pub const FLAG_EXECUTABLE: u8 = 0b0000_0001;

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TreeEntry {
    /// One path segment, valid per SPEC §5.7 and NFC-normalized.
    pub name: String,
    /// Entry kind.
    pub kind: EntryKind,
    /// Flag bits; see [`FLAG_EXECUTABLE`].
    pub flags: u8,
    /// Blob id for files and symlinks, tree id for directories.
    pub id: ObjectId,
    /// Content length for files, target length for symlinks, zero for directories.
    pub size: u64,
}

impl TreeEntry {
    /// True if the executable flag is set.
    pub fn executable(&self) -> bool {
        self.flags & FLAG_EXECUTABLE != 0
    }

    fn validate(&self) -> Result<(), TreeError> {
        validate_segment(&self.name).map_err(|e| TreeError::InvalidName(self.name.clone(), e))?;
        if !unicode_normalization::is_nfc(&self.name) {
            return Err(TreeError::NotNfc(self.name.clone()));
        }
        let allowed = if self.kind == EntryKind::File {
            FLAG_EXECUTABLE
        } else {
            0
        };
        if self.flags & !allowed != 0 {
            return Err(TreeError::Flags(self.name.clone(), self.flags));
        }
        if self.kind == EntryKind::Dir && self.size != 0 {
            return Err(TreeError::DirSize(self.name.clone()));
        }
        Ok(())
    }
}

/// One directory: entries strictly sorted by name bytes, with no duplicates.
///
/// A tree with no entries is valid only as the root of an empty depot. This type cannot
/// know whether it is a root, so callers that build or accept subtrees must reject empty
/// ones themselves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

/// Why a tree cannot be built or decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// The encoding ended early.
    #[error("tree encoding is truncated")]
    Truncated,
    /// Bytes remain after the last entry.
    #[error("tree encoding has trailing bytes")]
    TrailingBytes,
    /// A varint uses more bytes than needed or exceeds 64 bits.
    #[error("tree encoding has a non-canonical varint")]
    NonCanonicalVarint,
    /// The version byte is not 1.
    #[error("unsupported tree version {0}")]
    Version(u8),
    /// Unknown kind byte.
    #[error("unknown tree entry kind {0}")]
    Kind(u8),
    /// Flag bits that are reserved, or the executable bit on a non-file.
    #[error("entry {0:?} has invalid flags {1:#04x}")]
    Flags(String, u8),
    /// A directory entry with a non-zero size.
    #[error("directory entry {0:?} has a non-zero size")]
    DirSize(String),
    /// Entries are not in strictly ascending byte order.
    #[error("tree entries are not sorted")]
    Unsorted,
    /// Two entries with the same name.
    #[error("tree has duplicate entry {0:?}")]
    Duplicate(String),
    /// A name that is not a valid path segment.
    #[error("invalid entry name {0:?}: {1}")]
    InvalidName(String, PathError),
    /// A name that is not NFC-normalized.
    #[error("entry name {0:?} is not NFC-normalized")]
    NotNfc(String),
    /// A name that is not UTF-8.
    #[error("entry name is not UTF-8")]
    NotUtf8,
    /// Two names that differ only in letter case.
    #[error("entries {0:?} and {1:?} differ only in case")]
    CaseCollision(String, String),
}

impl From<VarintError> for TreeError {
    fn from(e: VarintError) -> Self {
        match e {
            VarintError::Truncated => TreeError::Truncated,
            VarintError::NonCanonical => TreeError::NonCanonicalVarint,
        }
    }
}

/// Tree encoding version written by this build.
pub const TREE_VERSION: u8 = 1;

impl Tree {
    /// Builds a tree from entries in any order. Sorts them, and rejects invalid entries,
    /// duplicate names and names that differ only in case.
    pub fn from_entries(mut entries: Vec<TreeEntry>) -> Result<Self, TreeError> {
        for e in &entries {
            e.validate()?;
        }
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        for w in entries.windows(2) {
            if w[0].name == w[1].name {
                return Err(TreeError::Duplicate(w[0].name.clone()));
            }
        }
        check_case_collisions(&entries)?;
        Ok(Self { entries })
    }

    /// Entries in canonical order.
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// Looks up an entry by exact name.
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_bytes().cmp(name.as_bytes()))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// True if the tree has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Canonical encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.entries.len() * 48);
        out.push(TREE_VERSION);
        varint::encode(self.entries.len() as u64, &mut out);
        for e in &self.entries {
            out.push(e.kind.code());
            out.push(e.flags);
            varint::encode(e.name.len() as u64, &mut out);
            out.extend_from_slice(e.name.as_bytes());
            out.extend_from_slice(e.id.as_bytes());
            varint::encode(e.size, &mut out);
        }
        out
    }

    /// Tree id of the canonical encoding.
    pub fn id(&self) -> ObjectId {
        tree_id(&self.encode())
    }

    /// Decodes a tree, rejecting every non-canonical form.
    pub fn decode(bytes: &[u8]) -> Result<Self, TreeError> {
        let mut r = Reader { buf: bytes };
        let version = r.byte()?;
        if version != TREE_VERSION {
            return Err(TreeError::Version(version));
        }
        let count = r.varint()?;
        // Each entry needs at least 1+1+1+32+1 bytes; refuse counts the input cannot hold
        // before allocating.
        if count > (r.buf.len() / 36) as u64 {
            return Err(TreeError::Truncated);
        }
        let mut entries: Vec<TreeEntry> = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let kind_code = r.byte()?;
            let kind = EntryKind::from_code(kind_code).ok_or(TreeError::Kind(kind_code))?;
            let flags = r.byte()?;
            let name_len = usize::try_from(r.varint()?).map_err(|_| TreeError::Truncated)?;
            let name = std::str::from_utf8(r.take(name_len)?)
                .map_err(|_| TreeError::NotUtf8)?
                .to_owned();
            let mut id = [0u8; 32];
            id.copy_from_slice(r.take(32)?);
            let size = r.varint()?;
            let entry = TreeEntry {
                name,
                kind,
                flags,
                id: ObjectId::from_bytes(id),
                size,
            };
            entry.validate()?;
            if let Some(prev) = entries.last() {
                match prev.name.as_bytes().cmp(entry.name.as_bytes()) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => return Err(TreeError::Duplicate(entry.name)),
                    std::cmp::Ordering::Greater => return Err(TreeError::Unsorted),
                }
            }
            entries.push(entry);
        }
        if !r.buf.is_empty() {
            return Err(TreeError::TrailingBytes);
        }
        check_case_collisions(&entries)?;
        Ok(Self { entries })
    }
}

fn check_case_collisions(entries: &[TreeEntry]) -> Result<(), TreeError> {
    let mut seen = std::collections::HashMap::with_capacity(entries.len());
    for e in entries {
        if let Some(other) = seen.insert(crate::path::case_fold(&e.name), &e.name) {
            return Err(TreeError::CaseCollision(other.clone(), e.name.clone()));
        }
    }
    Ok(())
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, TreeError> {
        let (&b, rest) = self.buf.split_first().ok_or(TreeError::Truncated)?;
        self.buf = rest;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], TreeError> {
        if self.buf.len() < n {
            return Err(TreeError::Truncated);
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    fn varint(&mut self) -> Result<u64, TreeError> {
        let (v, used) = varint::decode(self.buf)?;
        self.buf = &self.buf[used..];
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn file(name: &str, byte: u8, size: u64) -> TreeEntry {
        TreeEntry {
            name: name.into(),
            kind: EntryKind::File,
            flags: 0,
            id: ObjectId::from_bytes([byte; 32]),
            size,
        }
    }

    fn sample() -> Tree {
        Tree::from_entries(vec![
            file("b.txt", 2, 10),
            TreeEntry {
                name: "a".into(),
                kind: EntryKind::Dir,
                flags: 0,
                id: ObjectId::from_bytes([1; 32]),
                size: 0,
            },
            TreeEntry {
                name: "run.sh".into(),
                flags: FLAG_EXECUTABLE,
                ..file("run.sh", 3, 300)
            },
        ])
        .unwrap()
    }

    #[test]
    fn known_encoding() {
        let t = Tree::from_entries(vec![file("x", 0xaa, 300)]).unwrap();
        let mut want = vec![1, 1, 1, 0, 1, b'x'];
        want.extend_from_slice(&[0xaa; 32]);
        want.extend_from_slice(&[0xac, 0x02]);
        assert_eq!(t.encode(), want);
    }

    #[test]
    fn empty_root_round_trips() {
        let t = Tree::default();
        assert_eq!(t.encode(), vec![1, 0]);
        assert_eq!(Tree::decode(&[1, 0]).unwrap(), t);
    }

    #[test]
    fn sorts_by_bytes_and_looks_up() {
        let t = sample();
        let names: Vec<_> = t.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b.txt", "run.sh"]);
        assert!(t.get("run.sh").unwrap().executable());
        assert!(t.get("missing").is_none());
    }

    fn reencode(t: &Tree, f: impl FnOnce(&mut Vec<TreeEntry>)) -> Vec<u8> {
        let mut entries = t.entries().to_vec();
        f(&mut entries);
        Tree { entries }.encode()
    }

    #[test]
    fn decode_rejects_non_canonical_forms() {
        let t = sample();
        let good = t.encode();
        assert_eq!(Tree::decode(&good).unwrap(), t);

        let unsorted = reencode(&t, |e| e.swap(0, 1));
        assert_eq!(Tree::decode(&unsorted), Err(TreeError::Unsorted));

        let dup = reencode(&t, |e| e[1] = e[0].clone());
        assert!(matches!(Tree::decode(&dup), Err(TreeError::Duplicate(_))));

        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(Tree::decode(&trailing), Err(TreeError::TrailingBytes));

        assert_eq!(
            Tree::decode(&good[..good.len() - 1]),
            Err(TreeError::Truncated)
        );

        // Entry count 3 written as 0x83 0x00 instead of 0x03.
        let mut overlong = vec![1, 0x83, 0x00];
        overlong.extend_from_slice(&good[2..]);
        assert_eq!(Tree::decode(&overlong), Err(TreeError::NonCanonicalVarint));

        let mut kind = good.clone();
        kind[2] = 9;
        assert_eq!(Tree::decode(&kind), Err(TreeError::Kind(9)));

        let mut flags = good.clone();
        flags[3] = 0b10;
        assert!(matches!(Tree::decode(&flags), Err(TreeError::Flags(..))));

        // The executable bit on the directory entry "a".
        let mut dir_exec = good.clone();
        dir_exec[3] = FLAG_EXECUTABLE;
        assert!(matches!(Tree::decode(&dir_exec), Err(TreeError::Flags(..))));

        let mut version = good.clone();
        version[0] = 2;
        assert_eq!(Tree::decode(&version), Err(TreeError::Version(2)));

        let bad_name = reencode(&t, |e| e[0].name = "..".into());
        assert!(matches!(
            Tree::decode(&bad_name),
            Err(TreeError::InvalidName(..))
        ));

        let nfd = reencode(&t, |e| e[0].name = "e\u{301}".into());
        assert!(matches!(Tree::decode(&nfd), Err(TreeError::NotNfc(_))));

        let dir_size = reencode(&t, |e| e[0].size = 1);
        assert!(matches!(
            Tree::decode(&dir_size),
            Err(TreeError::DirSize(_))
        ));

        let case = reencode(&t, |e| {
            e[0] = file("README", 1, 1);
            e[1] = file("readme", 2, 1);
            e.truncate(2);
        });
        assert!(matches!(
            Tree::decode(&case),
            Err(TreeError::CaseCollision(..))
        ));
    }

    #[test]
    fn huge_count_does_not_allocate() {
        let mut b = vec![1];
        varint::encode(u64::MAX, &mut b);
        assert_eq!(Tree::decode(&b), Err(TreeError::Truncated));
    }

    #[test]
    fn from_entries_rejects_duplicates_and_case_variants() {
        assert!(matches!(
            Tree::from_entries(vec![file("a", 1, 1), file("a", 2, 2)]),
            Err(TreeError::Duplicate(_))
        ));
        assert!(matches!(
            Tree::from_entries(vec![file("A", 1, 1), file("a", 2, 2)]),
            Err(TreeError::CaseCollision(..))
        ));
    }

    fn name_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_][a-zA-Z0-9_ .\\-é日]{0,12}[a-zA-Z0-9_]".prop_filter("valid segment", |s| {
            validate_segment(s).is_ok() && unicode_normalization::is_nfc(s)
        })
    }

    fn entry_strategy() -> impl Strategy<Value = TreeEntry> {
        (
            name_strategy(),
            0u8..3,
            any::<bool>(),
            any::<[u8; 32]>(),
            any::<u64>(),
        )
            .prop_map(|(name, k, exec, id, size)| {
                let kind = [EntryKind::File, EntryKind::Dir, EntryKind::Symlink][k as usize];
                TreeEntry {
                    name,
                    kind,
                    flags: if exec && kind == EntryKind::File {
                        FLAG_EXECUTABLE
                    } else {
                        0
                    },
                    id: ObjectId::from_bytes(id),
                    size: if kind == EntryKind::Dir { 0 } else { size },
                }
            })
    }

    fn entries_strategy() -> impl Strategy<Value = Vec<TreeEntry>> {
        proptest::collection::vec(entry_strategy(), 0..40).prop_map(|v| {
            let mut seen = std::collections::HashSet::new();
            v.into_iter()
                .filter(|e| seen.insert(crate::path::case_fold(&e.name)))
                .collect()
        })
    }

    proptest! {
        #[test]
        fn decode_inverts_encode(entries in entries_strategy()) {
            let t = Tree::from_entries(entries).unwrap();
            prop_assert_eq!(Tree::decode(&t.encode()).unwrap(), t);
        }

        #[test]
        fn encoding_ignores_insertion_order(
            entries in entries_strategy(),
            seed in any::<u64>(),
        ) {
            let a = Tree::from_entries(entries.clone()).unwrap();
            let mut shuffled = entries;
            // Deterministic Fisher-Yates driven by the seed.
            let mut s = seed | 1;
            for i in (1..shuffled.len()).rev() {
                s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                shuffled.swap(i, (s % (i as u64 + 1)) as usize);
            }
            let b = Tree::from_entries(shuffled).unwrap();
            prop_assert_eq!(a.encode(), b.encode());
            prop_assert_eq!(a.id(), b.id());
        }
    }
}
