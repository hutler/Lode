//! `TreeEditor`: applies a validated change set to a root tree, loading and rewriting only the
//! directories on changed paths, and checking each change against what it finds.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use futures::future::BoxFuture;
use lode_core::path::case_fold;
use lode_core::{DepotId, DepotPath, EntryKind, ObjectId, Tree, TreeEntry};

use super::trees::{TreeLoadError, TreeSource};

/// What a change expects to find at its path in the base tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Nothing at the path (an add).
    Absent,
    /// A file or symlink with this id (a modify or delete).
    Id(ObjectId),
}

/// One path to set or remove.
#[derive(Debug, Clone)]
pub struct EditOp {
    /// The path.
    pub path: DepotPath,
    /// What must be there now.
    pub expect: Expect,
    /// The new entry (its `name` must be the last segment), or `None` to delete.
    pub entry: Option<TreeEntry>,
}

/// Why an edit could not be applied.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// Some paths do not match their expectation.
    #[error("{} path(s) changed on the branch", .0.len())]
    OutOfDate(Vec<String>),
    /// Two names in one directory differ only in case.
    #[error("{0} and {1} differ only in case")]
    CaseCollision(String, String),
    /// A tree could not be loaded.
    #[error(transparent)]
    Load(#[from] TreeLoadError),
    /// A resulting tree is invalid (an entry the change validation should have refused).
    #[error("invalid tree: {0}")]
    Invalid(String),
}

/// The result of an edit.
#[derive(Debug)]
pub struct Edited {
    /// The new root tree id.
    pub root: ObjectId,
    /// Every tree object created, as `(id, encoding)`, children before parents. Some may
    /// already exist in the store.
    pub new_trees: Vec<(ObjectId, Vec<u8>)>,
    /// For each changed path, the entry it had before (absent for adds).
    pub before: HashMap<DepotPath, TreeEntry>,
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    op: Option<usize>,
}

/// Applies edits to trees from a [`TreeSource`].
pub struct TreeEditor<'a> {
    src: &'a dyn TreeSource,
    depot: DepotId,
}

struct Walk<'o> {
    ops: &'o [EditOp],
    out_of_date: Vec<String>,
    new_trees: Vec<(ObjectId, Vec<u8>)>,
    before: HashMap<DepotPath, TreeEntry>,
}

impl<'a> TreeEditor<'a> {
    /// An editor reading trees of `depot` from `src`.
    pub fn new(src: &'a dyn TreeSource, depot: DepotId) -> Self {
        Self { src, depot }
    }

    /// Applies `ops` to the tree `root`. Paths must be unique and no path may be an ancestor
    /// of another (`validate_changes` guarantees both).
    pub async fn apply(&self, root: &ObjectId, ops: &[EditOp]) -> Result<Edited, EditError> {
        let mut trie = Node::default();
        for (i, op) in ops.iter().enumerate() {
            let mut node = &mut trie;
            for seg in op.path.segments() {
                node = node.children.entry(seg.to_owned()).or_default();
            }
            node.op = Some(i);
        }
        let mut walk = Walk {
            ops,
            out_of_date: Vec::new(),
            new_trees: Vec::new(),
            before: HashMap::new(),
        };
        let base = self.src.tree(self.depot, root).await?;
        let new_root = self.edit_dir(Some(base), &trie, "", &mut walk).await?;
        if !walk.out_of_date.is_empty() {
            walk.out_of_date.sort();
            return Err(EditError::OutOfDate(walk.out_of_date));
        }
        let root = match new_root {
            Some(id) => id,
            None => {
                // Only the root may be empty.
                let empty = Tree::default();
                let bytes = empty.encode();
                let id = empty.id();
                walk.new_trees.push((id, bytes));
                id
            }
        };
        Ok(Edited {
            root,
            new_trees: walk.new_trees,
            before: walk.before,
        })
    }

    fn edit_dir<'s, 'w, 'o>(
        &'s self,
        base: Option<Arc<Tree>>,
        node: &'w Node,
        prefix: &'w str,
        walk: &'w mut Walk<'o>,
    ) -> BoxFuture<'w, Result<Option<ObjectId>, EditError>>
    where
        's: 'w,
        'o: 'w,
    {
        Box::pin(async move {
            let mut entries: BTreeMap<String, TreeEntry> = base
                .as_ref()
                .map(|t| {
                    t.entries()
                        .iter()
                        .map(|e| (e.name.clone(), e.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for (name, child) in &node.children {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let existing = entries.get(name).cloned();
                if let Some(i) = child.op {
                    let op = &walk.ops[i];
                    let ok = match (op.expect, &existing) {
                        (Expect::Absent, None) => true,
                        (Expect::Id(id), Some(e)) => e.kind != EntryKind::Dir && e.id == id,
                        _ => false,
                    };
                    if !ok {
                        walk.out_of_date.push(path);
                        continue;
                    }
                    if let Some(e) = existing {
                        walk.before.insert(op.path.clone(), e);
                    }
                    match &op.entry {
                        Some(e) => {
                            entries.insert(name.clone(), e.clone());
                        }
                        None => {
                            entries.remove(name);
                        }
                    }
                    continue;
                }
                // A directory with changes below it.
                let sub = match &existing {
                    None => None,
                    Some(e) if e.kind == EntryKind::Dir => {
                        Some(self.src.tree(self.depot, &e.id).await?)
                    }
                    Some(_) => {
                        // A file where the change needs a directory: every change below
                        // is out of date.
                        collect_paths(child, &path, walk);
                        continue;
                    }
                };
                match self.edit_dir(sub, child, &path, walk).await? {
                    Some(id) => {
                        entries.insert(
                            name.clone(),
                            TreeEntry {
                                name: name.clone(),
                                kind: EntryKind::Dir,
                                flags: 0,
                                id,
                                size: 0,
                            },
                        );
                    }
                    None => {
                        entries.remove(name);
                    }
                }
            }
            if !walk.out_of_date.is_empty() {
                // The result is discarded; skip building trees.
                return Ok(None);
            }
            if entries.is_empty() {
                return Ok(None);
            }
            let mut folded: HashMap<String, &str> = HashMap::with_capacity(entries.len());
            for name in entries.keys() {
                if let Some(prev) = folded.insert(case_fold(name), name) {
                    let (a, b) = (join(prefix, prev), join(prefix, name));
                    return Err(EditError::CaseCollision(a, b));
                }
            }
            let tree = Tree::from_entries(entries.into_values().collect())
                .map_err(|e| EditError::Invalid(e.to_string()))?;
            let id = tree.id();
            walk.new_trees.push((id, tree.encode()));
            Ok(Some(id))
        })
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn collect_paths(node: &Node, path: &str, walk: &mut Walk<'_>) {
    if node.op.is_some() {
        walk.out_of_date.push(path.to_owned());
    }
    for (name, child) in &node.children {
        collect_paths(child, &format!("{path}/{name}"), walk);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use proptest::prelude::*;

    /// Trees in memory.
    #[derive(Default)]
    struct Mem(Mutex<HashMap<ObjectId, Arc<Tree>>>);

    #[async_trait]
    impl TreeSource for Mem {
        async fn tree(&self, _: DepotId, id: &ObjectId) -> Result<Arc<Tree>, TreeLoadError> {
            self.0
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or(TreeLoadError::Missing(*id))
        }
    }

    impl Mem {
        fn with_empty() -> (Self, ObjectId) {
            let m = Mem::default();
            let t = Tree::default();
            let id = t.id();
            m.0.lock().unwrap().insert(id, Arc::new(t));
            (m, id)
        }

        fn store(&self, trees: &[(ObjectId, Vec<u8>)]) {
            let mut g = self.0.lock().unwrap();
            for (id, bytes) in trees {
                g.insert(*id, Arc::new(Tree::decode(bytes).unwrap()));
            }
        }

        /// Every file under `root`, as path → (id, size).
        fn files(&self, root: &ObjectId) -> BTreeMap<String, (ObjectId, u64)> {
            let mut out = BTreeMap::new();
            self.walk(root, "", &mut out);
            out
        }

        fn walk(&self, id: &ObjectId, prefix: &str, out: &mut BTreeMap<String, (ObjectId, u64)>) {
            let t = self.0.lock().unwrap().get(id).cloned().unwrap();
            for e in t.entries() {
                let p = join(prefix, &e.name);
                if e.kind == EntryKind::Dir {
                    assert!(!self.0.lock().unwrap().get(&e.id).unwrap().is_empty());
                    self.walk(&e.id, &p, out);
                } else {
                    out.insert(p, (e.id, e.size));
                }
            }
        }
    }

    fn blob(n: u64) -> ObjectId {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&n.to_le_bytes());
        b[31] = 0xAB;
        ObjectId::from_bytes(b)
    }

    fn file(path: &str, n: u64) -> TreeEntry {
        TreeEntry {
            name: path.rsplit('/').next().unwrap().to_owned(),
            kind: EntryKind::File,
            flags: 0,
            id: blob(n),
            size: n,
        }
    }

    fn p(s: &str) -> DepotPath {
        DepotPath::parse(s).unwrap()
    }

    fn add(path: &str, n: u64) -> EditOp {
        EditOp {
            path: p(path),
            expect: Expect::Absent,
            entry: Some(file(path, n)),
        }
    }

    fn modify(path: &str, from: u64, to: u64) -> EditOp {
        EditOp {
            path: p(path),
            expect: Expect::Id(blob(from)),
            entry: Some(file(path, to)),
        }
    }

    fn delete(path: &str, from: u64) -> EditOp {
        EditOp {
            path: p(path),
            expect: Expect::Id(blob(from)),
            entry: None,
        }
    }

    async fn apply(m: &Mem, root: ObjectId, ops: &[EditOp]) -> Result<Edited, EditError> {
        let r = TreeEditor::new(m, DepotId(1)).apply(&root, ops).await?;
        m.store(&r.new_trees);
        Ok(r)
    }

    #[tokio::test]
    async fn add_modify_delete_and_prune() {
        let (m, empty) = Mem::with_empty();
        let r = apply(
            &m,
            empty,
            &[add("a/b/c.txt", 1), add("a/d.txt", 2), add("e", 3)],
        )
        .await
        .unwrap();
        assert_eq!(m.files(&r.root).len(), 3);
        let r2 = apply(&m, r.root, &[modify("a/b/c.txt", 1, 4), delete("e", 3)])
            .await
            .unwrap();
        let files = m.files(&r2.root);
        assert_eq!(files["a/b/c.txt"], (blob(4), 4));
        assert!(!files.contains_key("e"));
        assert_eq!(r2.before[&p("a/b/c.txt")].id, blob(1));
        // Deleting the last file of a/b prunes a/b; deleting everything leaves the empty root.
        let r3 = apply(&m, r2.root, &[delete("a/b/c.txt", 4)]).await.unwrap();
        let root = m.0.lock().unwrap()[&r3.root].clone();
        assert!(root.get("a").is_some());
        let a = m.0.lock().unwrap()[&root.get("a").unwrap().id].clone();
        assert!(a.get("b").is_none());
        let r4 = apply(&m, r3.root, &[delete("a/d.txt", 2)]).await.unwrap();
        assert_eq!(r4.root, empty);
        // Only directories on changed paths were rewritten: two trees for one nested change.
        let r5 = apply(&m, r.root, &[modify("a/d.txt", 2, 9)]).await.unwrap();
        assert_eq!(r5.new_trees.len(), 2);
    }

    #[tokio::test]
    async fn out_of_date_lists_every_failing_path() {
        let (m, empty) = Mem::with_empty();
        let r = apply(&m, empty, &[add("a/x", 1), add("f", 2)])
            .await
            .unwrap();
        let err = apply(
            &m,
            r.root,
            &[
                add("a/x", 5),        // exists
                modify("a/x2", 1, 2), // absent
                add("f/under", 1),    // ancestor is a file
                modify("h", 1, 2),    // absent
            ],
        )
        .await
        .unwrap_err();
        match err {
            EditError::OutOfDate(paths) => assert_eq!(paths, ["a/x", "a/x2", "f/under", "h"]),
            other => panic!("{other}"),
        }
        let err = apply(&m, r.root, &[delete("f", 99)]).await.unwrap_err();
        assert!(
            matches!(err, EditError::OutOfDate(p) if p == ["f"]),
            "stale base"
        );
        let err = apply(&m, r.root, &[delete("a", 1)]).await.unwrap_err();
        assert!(
            matches!(err, EditError::OutOfDate(_)),
            "a directory is not a file"
        );
    }

    #[tokio::test]
    async fn case_collisions_with_existing_names() {
        let (m, empty) = Mem::with_empty();
        let r = apply(&m, empty, &[add("Src/a.rs", 1)]).await.unwrap();
        for op in [add("src/b.rs", 2), add("SRC", 3), add("Src/A.rs", 4)] {
            let err = apply(&m, r.root, &[op]).await.unwrap_err();
            assert!(matches!(err, EditError::CaseCollision(..)), "{err}");
        }
        // Renaming by case in one change set works: delete the old name, add the new one.
        let ok = apply(&m, r.root, &[delete("Src/a.rs", 1), add("Src/A.rs", 1)]).await;
        assert!(ok.is_ok());
    }

    #[derive(Debug, Clone)]
    enum Step {
        Put(usize, u64),
        Del(usize),
    }

    const NAMES: [&str; 12] = [
        "a", "b", "a/a", "a/b", "a/c/d", "a/c/e", "b/x", "c/d/e/f", "c/d/g", "h", "i/j", "i/k",
    ];

    /// Builds a tree from a file set directly, without the editor.
    fn build(m: &Mem, files: &BTreeMap<String, u64>) -> ObjectId {
        fn rec(m: &Mem, files: &[(&str, u64)]) -> ObjectId {
            let mut groups: BTreeMap<&str, Vec<(&str, u64)>> = BTreeMap::new();
            let mut entries = Vec::new();
            for (path, n) in files {
                match path.split_once('/') {
                    Some((dir, rest)) => groups.entry(dir).or_default().push((rest, *n)),
                    None => entries.push(file(path, *n)),
                }
            }
            for (dir, sub) in groups {
                entries.push(TreeEntry {
                    name: dir.to_owned(),
                    kind: EntryKind::Dir,
                    flags: 0,
                    id: rec(m, &sub),
                    size: 0,
                });
            }
            let t = Tree::from_entries(entries).unwrap();
            let id = t.id();
            m.0.lock().unwrap().insert(id, Arc::new(t));
            id
        }
        let list: Vec<(&str, u64)> = files.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        rec(m, &list)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn editor_matches_building_from_scratch(
            batches in proptest::collection::vec(
                proptest::collection::vec(
                    prop_oneof![
                        (0..NAMES.len(), 1u64..50).prop_map(|(i, n)| Step::Put(i, n)),
                        (0..NAMES.len()).prop_map(Step::Del),
                    ],
                    1..6,
                ),
                1..8,
            )
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let (m, empty) = Mem::with_empty();
                let mut root = empty;
                let mut files: BTreeMap<String, u64> = BTreeMap::new();
                for batch in batches {
                    // Turn the batch into a valid change set against the current file set.
                    let mut ops = Vec::new();
                    let mut touched: Vec<&str> = Vec::new();
                    let mut next = files.clone();
                    for step in batch {
                        let (i, put) = match step { Step::Put(i, n) => (i, Some(n)), Step::Del(i) => (i, None) };
                        let path = NAMES[i];
                        let clash = touched.iter().any(|t| {
                            *t == path || t.starts_with(&format!("{path}/")) || path.starts_with(&format!("{t}/"))
                        });
                        // Paths whose ancestor is a file, or that are a directory, are skipped.
                        let blocked = next.keys().any(|f| path.starts_with(&format!("{f}/")) || f.starts_with(&format!("{path}/")));
                        if clash || blocked {
                            continue;
                        }
                        let cur = next.get(path).copied();
                        let op = match (cur, put) {
                            (None, Some(n)) => { next.insert(path.to_owned(), n); add(path, n) }
                            (Some(c), Some(n)) => { next.insert(path.to_owned(), n); modify(path, c, n) }
                            (Some(c), None) => { next.remove(path); delete(path, c) }
                            (None, None) => continue,
                        };
                        touched.push(path);
                        ops.push(op);
                    }
                    if ops.is_empty() {
                        continue;
                    }
                    root = apply(&m, root, &ops).await.unwrap().root;
                    files = next;
                }
                let expected = if files.is_empty() { empty } else { build(&m, &files) };
                prop_assert_eq!(root, expected);
                let listed: BTreeMap<String, u64> =
                    m.files(&root).into_iter().map(|(k, (_, n))| (k, n)).collect();
                prop_assert_eq!(listed, files);
                Ok(())
            })?;
        }
    }
}
