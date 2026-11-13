//! T054 fuzz targets as property tests: the decoders and parsers that see bytes from the
//! network or from disk. Arbitrary input never panics, and whatever is accepted is canonical.
//! CI runs the default case count; `PROPTEST_CASES=100000` runs the long form.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_core::{DepotPath, EntryKind, LodeConfig, ObjectHeader, ObjectId, Tree, TreeEntry};
use proptest::prelude::*;

fn sample_tree() -> Vec<u8> {
    let entry = |name: &str, kind: EntryKind, b: u8| TreeEntry {
        name: name.into(),
        kind,
        flags: 0,
        id: ObjectId::from_bytes([b; 32]),
        size: if kind == EntryKind::Dir {
            0
        } else {
            u64::from(b)
        },
    };
    Tree::from_entries(vec![
        entry("a.txt", EntryKind::File, 1),
        entry("dir", EntryKind::Dir, 2),
        entry("link", EntryKind::Symlink, 3),
        entry("Zebra.bin", EntryKind::File, 4),
    ])
    .unwrap()
    .encode()
}

/// Strings made mostly of the characters that make paths dangerous.
fn tricky() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-c./\\\\:<>|?*\" \\x00\\t]{0,24}",
        "(\\.\\.?/|[a-z]{1,3}/|/|\\\\|[A-Z]:){0,6}[a-z.]{0,4}",
        any::<String>(),
    ]
}

proptest! {
    #[test]
    fn tree_decode_takes_any_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(t) = Tree::decode(&bytes) {
            prop_assert_eq!(t.encode(), bytes);
        }
    }

    #[test]
    fn a_damaged_tree_is_refused_or_canonical(
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
        cut in any::<prop::sample::Index>(),
        truncate in any::<bool>(),
    ) {
        let mut bytes = sample_tree();
        for (at, x) in flips {
            let i = at.index(bytes.len());
            bytes[i] ^= x;
        }
        if truncate {
            bytes.truncate(cut.index(bytes.len()));
        }
        if let Ok(t) = Tree::decode(&bytes) {
            // Accepted: it is exactly the encoding of what was decoded, and every name is
            // a valid path segment.
            prop_assert_eq!(t.encode(), bytes);
            for e in t.entries() {
                prop_assert!(DepotPath::parse(&e.name).is_ok(), "{:?}", e.name);
                prop_assert!(!e.name.contains('/'));
            }
        }
    }

    #[test]
    fn header_decode_takes_any_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..40)) {
        if let Ok(h) = ObjectHeader::decode(&bytes) {
            prop_assert_eq!(&h.encode()[..], &bytes[..16]);
        }
    }

    #[test]
    fn accepted_paths_stay_inside_the_depot(s in tricky()) {
        if let Ok(p) = DepotPath::parse(&s) {
            let again = DepotPath::parse(p.as_str()).unwrap();
            prop_assert_eq!(&again, &p);
            prop_assert!(!p.as_str().starts_with('/'));
            for seg in p.segments() {
                prop_assert!(!seg.is_empty() && seg != "." && seg != "..", "{:?}", s);
                prop_assert!(
                    !seg.contains(['\\', ':', '<', '>', '"', '|', '?', '*', '\0']),
                    "{:?}",
                    s
                );
            }
        }
    }

    #[test]
    fn lodeconfig_parse_takes_any_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = LodeConfig::parse(&bytes);
    }

    #[test]
    fn lodeconfig_parse_takes_any_toml_shape(
        text in "(\\[{1,2}[a-z_.\"*]{0,12}\\]{1,2}\n|[a-z_]{1,10} ?= ?(\"[^\"\\n]{0,16}\"|[0-9]{1,20}|true|\\[(\"[a-z/*]{0,6}\",? ?){0,3}\\]|\\{[a-z]{0,3} ?= ?[0-9]?\\})\n){0,12}"
    ) {
        let _ = LodeConfig::parse(text.as_bytes());
    }
}
