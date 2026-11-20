//! The pending changes of a workspace as the changes view shows them (T131): `lode status`
//! and `lode changelist list` grouped into rows, what each file holds, and the details of the
//! selected file. Plain functions over the JSON documents, so they are tested without a window.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Value;

use crate::app::{ChangeRow, FileDetails};

/// One changed file of `lode status --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub status: String,
    pub changelist: Option<String>,
    pub copy_from: Option<(String, u64)>,
    pub moved: bool,
    pub locked: bool,
    pub needs_lock: bool,
    pub lock_required: bool,
}

/// A changelist of `lode changelist list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListInfo {
    pub name: String,
    pub description: String,
}

/// A merge the next commit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub branch: String,
    pub from_rev: u64,
    pub to_rev: u64,
}

/// A workspace's pending changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub branch: String,
    pub rev: Option<u64>,
    pub files: Vec<FileEntry>,
    pub lists: Vec<ListInfo>,
    pub merges: Vec<Merge>,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

impl Pending {
    /// From the documents of `lode status --json` and `lode changelist list --json`.
    pub fn from_json(status: &Value, lists: &Value) -> Self {
        let files = status["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|f| f["status"] != "unchanged")
                    .map(|f| FileEntry {
                        path: s(&f["path"]),
                        status: s(&f["status"]),
                        changelist: f["changelist"].as_str().map(str::to_owned),
                        copy_from: f["copy_from"]["path"]
                            .as_str()
                            .map(|p| (p.to_owned(), f["copy_from"]["rev"].as_u64().unwrap_or(0))),
                        moved: f["moved"].as_bool().unwrap_or(false),
                        locked: f["locked"].as_bool().unwrap_or(false),
                        needs_lock: f["needs_lock"].as_bool().unwrap_or(false),
                        lock_required: f["lock_required"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let lists = lists["changelists"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| ListInfo {
                        name: s(&l["name"]),
                        description: s(&l["description"]),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let merges = status["merges"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| Merge {
                        branch: s(&m["branch"]),
                        from_rev: m["from_rev"].as_u64().unwrap_or(0),
                        to_rev: m["to_rev"].as_u64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            branch: s(&status["branch"]),
            rev: status["rev"].as_u64(),
            files,
            lists,
            merges,
        }
    }

    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Every file the view lists, in list order.
    pub fn paths(&self) -> Vec<String> {
        self.groups()
            .into_iter()
            .flat_map(|g| g.files.into_iter().map(|f| f.path.clone()))
            .collect()
    }

    /// Named changelists (not the default one).
    pub fn named_lists(&self) -> impl Iterator<Item = &ListInfo> {
        self.lists.iter().filter(|l| l.name != "default")
    }

    /// Files grouped as `lode status` lists them: changes outside changelists first, then each
    /// changelist by name, then files that are not added yet. Empty changelists show too, and
    /// so does an empty "Changes" group when there are named changelists, as places to move
    /// files to.
    pub fn groups(&self) -> Vec<Group<'_>> {
        let mut main = Vec::new();
        let mut by_list: BTreeMap<&str, Vec<&FileEntry>> = BTreeMap::new();
        let mut fresh = Vec::new();
        for f in &self.files {
            match (f.status.as_str(), f.changelist.as_deref()) {
                ("untracked", _) => fresh.push(f),
                (_, Some(l)) if l != "default" => by_list.entry(l).or_default().push(f),
                _ => main.push(f),
            }
        }
        for l in self.named_lists() {
            by_list.entry(l.name.as_str()).or_default();
        }
        let mut groups = Vec::new();
        if !main.is_empty() || !by_list.is_empty() {
            groups.push(Group {
                key: "default".into(),
                title: "Changes".into(),
                description: String::new(),
                named: false,
                files: main,
            });
        }
        for (name, files) in by_list {
            let description = self
                .lists
                .iter()
                .find(|l| l.name == name)
                .map(|l| l.description.clone())
                .unwrap_or_default();
            groups.push(Group {
                key: name.to_owned(),
                title: format!("Changelist {name}"),
                description,
                named: true,
                files,
            });
        }
        if !fresh.is_empty() {
            groups.push(Group {
                key: NEW_FILES.into(),
                title: "Not added yet".into(),
                description: String::new(),
                named: false,
                files: fresh,
            });
        }
        groups
    }

    /// "Merging main r3..r5" while merges wait for the commit, else empty.
    pub fn merging(&self) -> String {
        if self.merges.is_empty() {
            return String::new();
        }
        format!(
            "Merging {}",
            merge_message(&self.merges).trim_start_matches("Merge ")
        )
    }

    /// Conflicted files.
    pub fn conflicted(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.status == "conflicted")
            .count()
    }
}

/// The key of the "Not added yet" group (not a valid changelist name).
pub const NEW_FILES: &str = "(new)";

/// A group of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group<'a> {
    /// `default`, the changelist's name, or [`NEW_FILES`].
    pub key: String,
    pub title: String,
    pub description: String,
    pub named: bool,
    pub files: Vec<&'a FileEntry>,
}

/// The commit message a merge suggests: `Merge BRANCH rA..rB` (or `rN` for one revision).
pub fn merge_message(merges: &[Merge]) -> String {
    merges
        .iter()
        .map(|m| {
            if m.from_rev == m.to_rev {
                format!("Merge {} r{}", m.branch, m.to_rev)
            } else {
                format!("Merge {} r{}..r{}", m.branch, m.from_rev, m.to_rev)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The rows of the list: each group's header, then its files.
pub fn rows(
    pending: &Pending,
    picked: &BTreeSet<String>,
    selected: Option<&str>,
) -> Vec<ChangeRow> {
    let mut out = Vec::with_capacity(pending.files.len() + 4);
    for g in pending.groups() {
        let all_picked = !g.files.is_empty() && g.files.iter().all(|f| picked.contains(&f.path));
        out.push(ChangeRow {
            is_group: true,
            key: g.key.as_str().into(),
            title: g.title.as_str().into(),
            count: i32::try_from(g.files.len()).unwrap_or(i32::MAX),
            description: g.description.as_str().into(),
            named: g.named,
            picked: all_picked,
            ..ChangeRow::default()
        });
        for f in g.files {
            out.push(file_row(
                f,
                picked.contains(&f.path),
                selected == Some(f.path.as_str()),
            ));
        }
    }
    out
}

/// Splits the rows for the two lists of the changes view: the picked files (for "To commit"),
/// and the rest under their groups (for "Pending"; a group whose files are all picked is left
/// out unless it is a named changelist, which keeps its actions). The third list is the named
/// changelists, for the sidebar.
pub fn split_rows(rows: &[ChangeRow]) -> (Vec<ChangeRow>, Vec<ChangeRow>, Vec<ChangeRow>) {
    let mut picked = Vec::new();
    let mut pending = Vec::new();
    let mut lists = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let g = &rows[i];
        if !g.is_group {
            if g.picked {
                picked.push(g.clone());
            } else {
                pending.push(g.clone());
            }
            i += 1;
            continue;
        }
        if g.named {
            lists.push(g.clone());
        }
        let end = rows[i + 1..]
            .iter()
            .position(|r| r.is_group)
            .map_or(rows.len(), |n| i + 1 + n);
        let files = &rows[i + 1..end];
        let left: Vec<ChangeRow> = files.iter().filter(|f| !f.picked).cloned().collect();
        picked.extend(files.iter().filter(|f| f.picked).cloned());
        if !left.is_empty() || g.named {
            pending.push(g.clone());
            pending.extend(left);
        }
        i = end;
    }
    (picked, pending, lists)
}

/// One file's row.
pub fn file_row(f: &FileEntry, picked: bool, selected: bool) -> ChangeRow {
    let (dir, name) = split_path(&f.path);
    let mut tags = Vec::new();
    if let Some((from, _)) = &f.copy_from {
        tags.push(format!(
            "{} from {from}",
            if f.moved { "moved" } else { "copied" }
        ));
    }
    if f.locked {
        tags.push("locked".to_owned());
    }
    if f.needs_lock {
        tags.push("needs lock".to_owned());
    }
    ChangeRow {
        is_group: false,
        key: f.path.as_str().into(),
        title: name.into(),
        dir: dir.into(),
        status: f.status.as_str().into(),
        file_kind: file_kind(&f.path).into(),
        tags: tags.join(" · ").into(),
        picked,
        selected,
        ..ChangeRow::default()
    }
}

/// `("art/ui/", "logo.png")` for `art/ui/logo.png`.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => (&path[..=at], &path[at + 1..]),
        None => ("", path),
    }
}

/// What a file holds, from its extension, for its icon.
pub fn file_kind(path: &str) -> &'static str {
    let ext = split_path(path)
        .1
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tga" | "tif" | "tiff" | "psd"
        | "exr" | "hdr" | "dds" | "svg" | "ico" | "kra" => "image",
        "wav" | "mp3" | "ogg" | "flac" | "aif" | "aiff" | "m4a" | "wem" | "bank" => "audio",
        "mp4" | "mov" | "avi" | "mkv" | "webm" => "video",
        "fbx" | "obj" | "blend" | "gltf" | "glb" | "max" | "ma" | "mb" | "usd" | "usda"
        | "usdc" | "abc" | "uasset" | "umap" => "model",
        "zip" | "7z" | "rar" | "tar" | "gz" | "xz" | "zst" | "pak" => "archive",
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "go" | "py" | "js" | "ts" | "tsx"
        | "jsx" | "java" | "kt" | "swift" | "lua" | "sh" | "ps1" | "bat" | "slint" | "hlsl"
        | "glsl" | "shader" | "usf" | "ush" | "json" | "toml" | "yaml" | "yml" | "xml" | "ini"
        | "cfg" | "html" | "css" => "code",
        "txt" | "md" | "rst" | "csv" | "log" | "" => "text",
        _ => "other",
    }
}

/// Words for a file's status.
pub fn status_text(status: &str) -> &'static str {
    match status {
        "modified" => "Modified",
        "added" => "Added",
        "deleted" => "Deleted",
        "missing" => "Missing (deleted outside lode)",
        "conflicted" => "Conflicted",
        "untracked" => "New, not added",
        _ => "Unchanged",
    }
}

/// The details panel for `f`.
pub fn details(f: &FileEntry) -> FileDetails {
    let lock = if f.locked {
        "You hold the lock"
    } else if f.needs_lock {
        "Needs a lock before it can be committed"
    } else if f.lock_required {
        "Lock required for changes"
    } else {
        "Not needed"
    };
    FileDetails {
        shown: true,
        path: f.path.as_str().into(),
        name: split_path(&f.path).1.into(),
        status: f.status.as_str().into(),
        status_text: status_text(&f.status).into(),
        copy: f
            .copy_from
            .as_ref()
            .map(|(p, rev)| {
                format!(
                    "{} {p}@r{rev}",
                    if f.moved { "moved from" } else { "copied from" }
                )
            })
            .unwrap_or_default()
            .into(),
        changelist: f
            .changelist
            .as_deref()
            .filter(|c| *c != "default")
            .unwrap_or("none")
            .into(),
        lock: lock.into(),
        locked: f.locked,
    }
}

/// Whether `name` may name a changelist: 1 to 64 letters, digits, dots, dashes and
/// underscores, and not `default`.
pub fn valid_list_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "default"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The recent commit messages, newest first, at most [`MESSAGES_MAX`], without repeats.
pub fn remember_message(list: &mut Vec<String>, message: &str) {
    let m = message.trim();
    if m.is_empty() {
        return;
    }
    list.retain(|x| x != m);
    list.insert(0, m.to_owned());
    list.truncate(MESSAGES_MAX);
}

/// How many commit messages the app remembers.
pub const MESSAGES_MAX: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pending() -> Pending {
        Pending::from_json(
            &json!({
                "branch": "main", "rev": 7,
                "files": [
                    {"path": "src/a.rs", "status": "modified", "changelist": null, "copy_from": null, "moved": false, "locked": false, "needs_lock": false, "lock_required": false},
                    {"path": "art/logo.png", "status": "modified", "changelist": "ui", "copy_from": null, "moved": false, "locked": true, "needs_lock": false, "lock_required": true},
                    {"path": "b.txt", "status": "added", "changelist": null, "copy_from": {"path": "a.txt", "rev": 3}, "moved": true, "locked": false, "needs_lock": false, "lock_required": false},
                    {"path": "notes.md", "status": "untracked", "changelist": null, "copy_from": null, "moved": false, "locked": false, "needs_lock": false, "lock_required": false},
                    {"path": "same.txt", "status": "unchanged", "changelist": null, "copy_from": null, "moved": false, "locked": false, "needs_lock": false, "lock_required": false}
                ]
            }),
            &json!({"changelists": [
                {"name": "default", "description": "", "files": []},
                {"name": "ui", "description": "New logo", "files": ["art/logo.png"]},
                {"name": "empty", "description": "", "files": []}
            ]}),
        )
    }

    #[test]
    fn files_are_grouped_as_lode_status_lists_them() {
        let p = pending();
        let g = p.groups();
        let keys: Vec<_> = g.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["default", "empty", "ui", NEW_FILES]);
        assert_eq!(g[0].files.len(), 2);
        assert_eq!(
            g[1].files.len(),
            0,
            "an empty changelist is a place to move files to"
        );
        assert_eq!(g[2].description, "New logo");
        assert_eq!(p.paths(), ["src/a.rs", "b.txt", "art/logo.png", "notes.md"]);
    }

    #[test]
    fn rows_carry_picks_selection_kinds_and_tags() {
        let p = pending();
        let picked: BTreeSet<String> = ["art/logo.png".to_owned()].into();
        let r = rows(&p, &picked, Some("b.txt"));
        assert_eq!(r.len(), 4 + 4);
        let ui = r.iter().find(|r| r.is_group && r.key == "ui").unwrap();
        assert!(ui.picked, "every file of the group is picked");
        let logo = r.iter().find(|r| r.key == "art/logo.png").unwrap();
        assert_eq!(
            (logo.dir.as_str(), logo.title.as_str()),
            ("art/", "logo.png")
        );
        assert_eq!(logo.file_kind, "image");
        assert_eq!(logo.tags, "locked");
        let b = r.iter().find(|r| r.key == "b.txt").unwrap();
        assert!(b.selected);
        assert_eq!(b.tags, "moved from a.txt");
        assert_eq!(
            details(p.file("b.txt").unwrap()).copy,
            "moved from a.txt@r3"
        );
    }

    #[test]
    fn kinds_names_messages_and_merges() {
        assert_eq!(file_kind("Content/Hero.uasset"), "model");
        assert_eq!(file_kind("src/main.RS"), "code");
        assert_eq!(file_kind("README"), "text");
        assert_eq!(file_kind("data.bin"), "other");
        assert!(valid_list_name("harbor-lights_2.0"));
        assert!(!valid_list_name("default"));
        assert!(!valid_list_name("two words"));
        let mut m = vec!["a".to_owned(), "b".to_owned()];
        remember_message(&mut m, " b ");
        assert_eq!(m, ["b", "a"]);
        for i in 0..30 {
            remember_message(&mut m, &i.to_string());
        }
        assert_eq!(m.len(), MESSAGES_MAX);
        let merges = [Merge {
            branch: "dev".into(),
            from_rev: 3,
            to_rev: 5,
        }];
        assert_eq!(merge_message(&merges), "Merge dev r3..r5");
    }
}
