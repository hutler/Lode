//! Renders the screens of the Slint app into PNG files for review, light and dark, with
//! the software renderer (no window system needed).
//!
//! `cargo run --example screenshots -- <folder>` (default `target/screenshots`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use std::collections::BTreeSet;

use lode_desk::app::{
    self, ChangesState, CheckoutState, Dialog, ErrorInfo, ListDialogState, Options, Recent,
    ServerRowData, SignInState, WorkspaceTab,
};
use lode_desk::app::{AssetCard, AssetSearch, Assets, DepotFolder};
use lode_desk::app::{BlameState, DiffNote, DiffState, HistoryState, ImageState, Pane, Reading};
use lode_desk::app::{
    BranchesState, Branching, ConflictState, FolderRow, MergeState, OutcomeState, RegionRow,
    SideLine, ViewState,
};
use lode_desk::app::{LinkChoice, LinkState, Links, UpdateOffer, Updates};
use lode_desk::app::{LocksState, ShelvesState, SubmitState, Team};
use lode_desk::changes::{self, FileEntry, ListInfo, Pending};
use lode_desk::{branches, diffs, history, merge, team, views};
use serde_json::json;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, VecModel};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const SCALE: f32 = 2.0;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/screenshots"), PathBuf::from);
    std::fs::create_dir_all(&out).unwrap();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
    }))
    .unwrap();

    let home = tempfile::tempdir().unwrap();
    let w = app::build(Options {
        home: home.path().to_path_buf(),
        data_dir: home.path().join("app"),
        config_dir: Some(home.path().join("config")),
        pick_folder: Rc::new(|_| None),
        open_browser: Arc::new(|_| false),
        updater: lode_desk::update::Updater {
            pubkey: String::new(),
            feed: String::new(),
            current: "0.1.0".into(),
            platforms: Vec::new(),
            settings_file: home.path().join("app/settings.json"),
        },
    })
    .unwrap();
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = PhysicalSize::new(
        (WIDTH as f32 * SCALE) as u32,
        (HEIGHT as f32 * SCALE) as u32,
    );
    window.set_size(size);
    w.show().unwrap();

    let recent = Rc::new(VecModel::from(vec![
        recent("game", "main", "D:\\work\\game", "lode.example.com"),
        recent(
            "engine",
            "release-2.4",
            "D:\\work\\engine",
            "lode.example.com",
        ),
        recent("art-source", "main", "E:\\art", "art.example.com:8443"),
    ]));
    w.set_recent(ModelRc::from(recent));
    w.set_servers(ModelRc::from(Rc::new(VecModel::from(vec![
        ServerRowData {
            url: "https://lode.example.com".into(),
            host: "lode.example.com".into(),
            signed_in: true,
            status: "Signed in as alice".into(),
        },
        ServerRowData {
            url: "https://art.example.com:8443".into(),
            host: "art.example.com:8443".into(),
            signed_in: false,
            status: "Not signed in".into(),
        },
    ]))));

    for dark in [false, true] {
        w.invoke_use_dark(dark);
        let theme = if dark { "dark" } else { "light" };
        let shot = |name: &str| render(&window, size, &out.join(format!("{name}-{theme}.png")));

        w.set_current_tab(0);
        w.set_dialog(Dialog::None);
        w.set_home_error(ErrorInfo::default());
        shot("welcome");

        w.set_sign_in(SignInState {
            server: "https://lode.example.com".into(),
            name: "alice".into(),
            sso_label: "Example Corp".into(),
            password_login: true,
            probe: "ok".into(),
            probe_text: "Lode server 0.2.0 at lode.example.com. It offers Example Corp sign-in."
                .into(),
            ..SignInState::default()
        });
        w.set_dialog(Dialog::SignIn);
        shot("sign-in");

        w.set_sign_in(SignInState {
            server: "https://lode.example.com/admin/".into(),
            name: "alice".into(),
            password_login: true,
            probe: "bad".into(),
            probe_text: "No Lode server answers at this address. The server is at \
                         https://lode.example.com; /admin/ is its web admin page."
                .into(),
            suggested: "https://lode.example.com".into(),
            ..SignInState::default()
        });
        shot("sign-in-path");

        w.set_sign_in(SignInState {
            server: "https://lode.example.com".into(),
            name: "alice".into(),
            password: "wrong".into(),
            password_login: true,
            probe: "ok".into(),
            probe_text: "Lode server 0.2.0 at lode.example.com.".into(),
            error: ErrorInfo {
                shown: true,
                message: "The name or password is wrong.".into(),
                next: "Check both and try again. A server administrator can reset your password."
                    .into(),
                code: "unauthenticated".into(),
            },
            ..SignInState::default()
        });
        shot("sign-in-refused");

        w.set_checkout(CheckoutState {
            target: "https://lode.example.com/game".into(),
            branch: "main".into(),
            folder: "D:\\work\\game".into(),
            busy: true,
            progress: 0.42,
            progress_text: "Fetching 1,260 of 3,000 · 1.4 GB".into(),
            ..CheckoutState::default()
        });
        w.set_dialog(Dialog::CheckOut);
        shot("check-out");

        w.set_dialog(Dialog::None);
        w.set_workspaces(ModelRc::from(Rc::new(VecModel::from(vec![
            WorkspaceTab {
                root: "D:\\work\\game".into(),
                depot: "game".into(),
                branch: "main".into(),
                rev: "1482".into(),
                server: "lode.example.com".into(),
                server_url: "https://lode.example.com".into(),
            },
            WorkspaceTab {
                root: "D:\\work\\engine".into(),
                depot: "engine".into(),
                branch: "release-2.4".into(),
                rev: "977".into(),
                server: "lode.example.com".into(),
                server_url: "https://lode.example.com".into(),
            },
        ]))));
        w.set_current_tab(1);
        w.global::<Reading>().set_pane(Pane::Changes);
        w.global::<Reading>().set_diff(DiffState::default());
        w.set_change_rows(ModelRc::default());
        w.set_picked_rows(ModelRc::default());
        w.set_pending_rows(ModelRc::default());
        w.set_list_rows(ModelRc::default());
        w.set_changes(ChangesState {
            loaded: true,
            ..ChangesState::default()
        });
        w.set_commit_message("".into());
        let b = w.global::<Branching>();
        b.set_side_rows(ModelRc::from(Rc::new(VecModel::from(
            branches::branch_rows(&sample_branches(), "main", "", "")
                .into_iter()
                .filter(|r| !r.archived)
                .collect::<Vec<_>>(),
        ))));
        b.set_behind(4);
        shot("workspace");

        // T131: the changes view, a refused commit and changelists.
        let pending = sample_pending();
        let picked: BTreeSet<String> = ["Content/Maps/Harbor.umap", "Source/Game/Boat.cpp"]
            .into_iter()
            .map(String::from)
            .collect();
        let selected = "Source/Game/Boat.cpp";
        let rows = changes::rows(&pending, &picked, Some(selected));
        let (picked_rows, pending_rows, list_rows) = changes::split_rows(&rows);
        w.set_change_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        w.set_picked_rows(ModelRc::from(Rc::new(VecModel::from(picked_rows))));
        w.set_pending_rows(ModelRc::from(Rc::new(VecModel::from(pending_rows))));
        w.set_list_rows(ModelRc::from(Rc::new(VecModel::from(list_rows))));
        let r = w.global::<Reading>();
        r.set_diff(DiffState {
            path: selected.into(),
            name: "Boat.cpp".into(),
            r#where: "Source/Game/Boat.cpp (working copy)".into(),
            insertions: 5,
            deletions: 3,
            layout: "unified".into(),
            ..DiffState::default()
        });
        r.set_diff_rows(ModelRc::from(Rc::new(VecModel::from(diffs::rows(
            &diffs::parse_patch(SAMPLE_PATCH),
            diffs::Layout::Unified,
        )))));
        let state = ChangesState {
            loaded: true,
            total: 7,
            picked: 2,
            details: changes::details(pending.file(selected).unwrap()),
            ..ChangesState::default()
        };
        w.set_changes(state.clone());
        w.set_commit_message("Harbor lights: boats bob with the tide".into());
        shot("changes");

        let validation = w.global::<app::Validation>();
        validation.set_stage("results".into());
        validation.set_heading("Validation failed".into());
        validation
            .set_detail("Review the problems below, then edit the files and check again.".into());
        validation.set_retryable(true);
        validation.set_allow_shelve(true);
        validation.set_allow_commit(false);
        let results = json!([
            {"name":"json", "outcome":"failed", "problems":[
                {"path":"Data/harbor.json","line":14,"level":"error","message":"Expected a comma between entries"}
            ]},
            {"name":"asset-names", "outcome":"passed", "problems":[]}
        ]);
        validation.set_rows(ModelRc::from(Rc::new(VecModel::from(
            lode_desk::validation::rows(&results)
                .into_iter()
                .map(|row| app::ValidationRow {
                    kind: row.kind.into(),
                    label: row.label.into(),
                    path: row.path.into(),
                    line: row.line,
                })
                .collect::<Vec<_>>(),
        ))));
        w.set_dialog(Dialog::Validation);
        shot("validation");
        w.set_dialog(Dialog::None);

        w.set_changes(ChangesState {
            error: app::error_info(&lode_desk::desk::ErrorView {
                code: "locked".into(),
                message: "Content/Maps/Harbor.umap is locked by bob".into(),
                details: serde_json::Value::Null,
            }),
            refused: "Content/Maps/Harbor.umap".into(),
            ..state.clone()
        });
        shot("changes-refused");

        w.set_changes(state);
        w.set_list_dialog(ListDialogState {
            value: "harbor-lights".into(),
            description: "Lights and boats for the harbor level".into(),
            picked: 2,
            move_picked: true,
            ..ListDialogState::default()
        });
        w.set_dialog(Dialog::NewChangelist);
        shot("changelist-new");
        w.set_dialog(Dialog::None);

        // T132: history, blame and diffs.
        let r = w.global::<Reading>();
        let revisions = sample_history();
        let checks = sample_checks();
        r.set_revs(ModelRc::from(Rc::new(VecModel::from(history::rows(
            &revisions,
            "main",
            true,
            Some(1478),
            &checks,
        )))));
        r.set_rev_checks(ModelRc::from(Rc::new(VecModel::from(team::check_rows(
            team::checks_of(&checks, "r1478"),
        )))));
        let (detail, files) = history::detail(&revisions[4], "main");
        r.set_detail(detail);
        r.set_files(ModelRc::from(Rc::new(VecModel::from(files))));
        r.set_history(HistoryState {
            title: "History of main".into(),
            subtitle: "game".into(),
            more: true,
            lanes: 2,
            scroll_to: -1,
            ..HistoryState::default()
        });
        r.set_diff(DiffState {
            path: "Source/Game/Boat.cpp".into(),
            name: "Boat.cpp".into(),
            r#where: "Source/Game/Boat.cpp in r1478".into(),
            insertions: 5,
            deletions: 3,
            layout: "unified".into(),
            ..DiffState::default()
        });
        r.set_pane(Pane::History);
        shot("history");

        r.set_blame(BlameState {
            path: "Source/Game/Boat.cpp".into(),
            name: "Boat.cpp".into(),
            at: " at r1482".into(),
            ..BlameState::default()
        });
        r.set_blame_lines(ModelRc::from(Rc::new(VecModel::from(
            history::blame_lines(&sample_blame()),
        ))));
        r.set_pane(Pane::Blame);
        shot("blame");

        let hunks = diffs::parse_patch(SAMPLE_PATCH);
        let diff = DiffState {
            path: "Source/Game/Boat.cpp".into(),
            name: "Boat.cpp".into(),
            r#where: "Source/Game/Boat.cpp in r1478".into(),
            back_label: "Back to History".into(),
            insertions: 5,
            deletions: 3,
            layout: "unified".into(),
            ..DiffState::default()
        };
        r.set_diff(diff.clone());
        r.set_diff_rows(ModelRc::from(Rc::new(VecModel::from(diffs::rows(
            &hunks,
            diffs::Layout::Unified,
        )))));
        r.set_pane(Pane::Diff);
        shot("diff-unified");
        r.set_diff(DiffState {
            layout: "split".into(),
            ..diff.clone()
        });
        r.set_diff_rows(ModelRc::from(Rc::new(VecModel::from(diffs::rows(
            &hunks,
            diffs::Layout::Split,
        )))));
        shot("diff-split");

        r.set_diff(DiffState {
            path: "Content/Maps/Harbor.umap".into(),
            name: "Harbor.umap".into(),
            r#where: "Content/Maps/Harbor.umap in r1478".into(),
            back_label: "Back to History".into(),
            binary: true,
            note: DiffNote {
                shown: true,
                title: "Binary file".into(),
                ext: "umap".into(),
                before: "2.4 MB".into(),
                after: "2.5 MB".into(),
                why: "Binary content is not shown as a diff.".into(),
                ..DiffNote::default()
            },
            ..DiffState::default()
        });
        r.set_diff_rows(ModelRc::default());
        shot("diff-binary");

        r.set_diff(DiffState {
            path: "Content/Textures/Lamp_D.png".into(),
            name: "Lamp_D.png".into(),
            r#where: "Content/Textures/Lamp_D.png in r1478".into(),
            back_label: "Back to History".into(),
            binary: true,
            image: true,
            ..DiffState::default()
        });
        for mode in ["side", "swipe", "onion"] {
            r.set_image(ImageState {
                both: true,
                mode: mode.into(),
                has_old: true,
                has_new: true,
                old: lamp(false),
                new: lamp(true),
                old_caption: "Before   256 × 256   12.0 KB".into(),
                new_caption: "After   256 × 256   13.4 KB".into(),
                ..ImageState::default()
            });
            r.set_image_pos(42.0);
            r.set_image_opacity(55.0);
            shot(&format!("image-{mode}"));
        }

        // T133: locks, shelves and a shelf's checks.
        let t = w.global::<Team>();
        let locks = sample_locks();
        let lock_rows = team::lock_rows(&locks, "", false);
        t.set_locks(LocksState {
            loaded: true,
            held: i32::try_from(lock_rows.len()).unwrap_or(0),
            mine: 2,
            ..LocksState::default()
        });
        t.set_lock_rows(ModelRc::from(Rc::new(VecModel::from(lock_rows))));
        r.set_pane(Pane::Locks);
        shot("locks");

        let shelves = sample_shelves();
        t.set_shelves(ShelvesState {
            loaded: true,
            whose: "all".into(),
            state: "open".into(),
            layout: "unified".into(),
            ..ShelvesState::default()
        });
        t.set_shelf_rows(ModelRc::from(Rc::new(VecModel::from(team::shelf_rows(
            &shelves,
            "alice",
            Some(12),
            &checks,
        )))));
        let (detail, files) = team::shelf_detail(&shelves[0], "alice");
        t.set_shelf(detail);
        t.set_shelf_files(ModelRc::from(Rc::new(VecModel::from(files))));
        t.set_shelf_diff(ModelRc::from(Rc::new(VecModel::from(
            team::shelf_diff_rows(&shelves[0], diffs::Layout::Unified),
        ))));
        t.set_shelf_checks(ModelRc::from(Rc::new(VecModel::from(team::check_rows(
            team::checks_of(&checks, "12@3"),
        )))));
        r.set_pane(Pane::Shelves);
        shot("shelves");

        t.set_submit_text("Boats bob with the tide".into());
        t.set_submit(SubmitState {
            title: "Submit shelf 12, version 3".into(),
            note: "Commits the shelf on main exactly as shown. You are the submitter; alice stays the author.".into(),
            error: ErrorInfo {
                shown: true,
                message: "Required checks have not passed for version 3.".into(),
                next: "Submit again once they pass.".into(),
                code: "checks_required".into(),
            },
            missing: "ci/test: failure".into(),
            ..SubmitState::default()
        });
        w.set_dialog(Dialog::SubmitShelf);
        shot("shelf-submit-blocked");
        w.set_dialog(Dialog::None);

        // T134: branches, a merge, a three-way conflict and the view editor.
        let b = w.global::<Branching>();
        let list = sample_branches();
        b.set_rows(ModelRc::from(Rc::new(VecModel::from(
            branches::branch_rows(&list, "main", "", "harbor-night"),
        ))));
        b.set_branches(BranchesState {
            loaded: true,
            current: "main".into(),
            selected: "harbor-night".into(),
            active: true,
            head: 1477,
            ..BranchesState::default()
        });
        r.set_pane(Pane::Changes);
        w.set_dialog(Dialog::Branches);
        shot("branches");

        b.set_sources(ModelRc::from(Rc::new(VecModel::from(vec![
            slint::SharedString::from("harbor-night (r1477)"),
            "release/1.2 (r1460)".into(),
        ]))));
        b.set_merge(MergeState {
            title: "Merge into main".into(),
            head: "r1477".into(),
            ..MergeState::default()
        });
        w.set_dialog(Dialog::Merge);
        shot("merge");

        let result = serde_json::json!({"source": "harbor-night", "from_rev": 1470, "to_rev": 1477,
            "nothing_to_do": false, "counts": {"applied": 3, "merged": 1, "conflicted": 1},
            "files": [{"path": "Source/Game/Boat.cpp", "outcome": "conflicted"},
                      {"path": "Source/Game/Tide.cpp", "outcome": "merged"},
                      {"path": "Content/Maps/Harbor_Night.umap", "outcome": "applied"},
                      {"path": "Content/Props/Lamp.uasset", "outcome": "applied"},
                      {"path": "Config/DefaultGame.ini", "outcome": "applied"}]});
        let o = branches::merge_outcome(&result);
        let (rows, _) = branches::outcome_rows(&result, 40);
        b.set_outcome_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        b.set_outcome(OutcomeState {
            title: o.title.into(),
            text: o.text.into(),
            conflicts: o.conflicts.into(),
            notes: o.notes.into(),
            done: o.done,
            ..OutcomeState::default()
        });
        w.set_dialog(Dialog::Outcome);
        shot("merge-result");
        w.set_dialog(Dialog::None);

        let (base, mine, theirs, current) = sample_conflict();
        let base_lines = merge::lines(base);
        let side = |text: &str, against: Option<&[String]>| -> Vec<SideLine> {
            let own = merge::lines(text);
            let changed = against.and_then(|a| merge::changed_lines(a, &own, merge::COMPARE_LIMIT));
            own.into_iter()
                .enumerate()
                .map(|(i, l)| SideLine {
                    n: i32::try_from(i + 1).unwrap_or(0),
                    text: l.into(),
                    changed: changed.as_ref().is_some_and(|c| c.contains(&i)),
                })
                .collect()
        };
        b.set_base_lines(ModelRc::from(Rc::new(VecModel::from(side(base, None)))));
        b.set_mine_lines(ModelRc::from(Rc::new(VecModel::from(side(
            mine,
            Some(&base_lines),
        )))));
        b.set_theirs_lines(ModelRc::from(Rc::new(VecModel::from(side(
            theirs,
            Some(&base_lines),
        )))));
        let regions: Vec<RegionRow> = merge::regions(current)
            .into_iter()
            .enumerate()
            .map(|(i, (m, t))| RegionRow {
                index: i32::try_from(i).unwrap_or(0),
                label: format!("Conflict {} of 1", i + 1).into(),
                mine: m.join("\n").into(),
                theirs: t.join("\n").into(),
            })
            .collect();
        b.set_regions(ModelRc::from(Rc::new(VecModel::from(regions))));
        b.set_result_text(current.into());
        b.set_conflict(ConflictState {
            loaded: true,
            path: "Source/Game/Boat.cpp".into(),
            name: "Boat.cpp".into(),
            from: "r1477".into(),
            editable: true,
            text: true,
            base_note: "412 B".into(),
            mine_note: "431 B · 1 line changed".into(),
            theirs_note: "447 B · 2 lines changed".into(),
            regions_left: 1,
            ..ConflictState::default()
        });
        r.set_pane(Pane::Conflict);
        shot("conflict");

        let spec = views::Spec {
            include: vec![],
            exclude: vec!["Content/Cinematics/**".into()],
        };
        let folder = |path: &str, depth: i32, open: bool| {
            let st = views::folder_state(&spec, path);
            FolderRow {
                path: path.into(),
                name: path.rsplit('/').next().unwrap_or(path).into(),
                depth,
                open,
                on: st.on,
                mixed: st.mixed,
                locked_by: st.locked_by.unwrap_or_default().into(),
                files: "".into(),
            }
        };
        b.set_folders(ModelRc::from(Rc::new(VecModel::from(vec![
            folder("Config", 0, false),
            folder("Content", 0, true),
            folder("Content/Boats", 1, false),
            folder("Content/Cinematics", 1, false),
            folder("Content/Maps", 1, false),
            folder("Content/Props", 1, false),
            FolderRow {
                path: "Content/".into(),
                depth: 1,
                files: "2 files".into(),
                ..FolderRow::default()
            },
            folder("Source", 0, false),
        ]))));
        b.set_exclude_text("Content/Cinematics/**".into());
        b.set_view(ViewState {
            loaded: true,
            depot: "game".into(),
            server_label: "In the view at r1482".into(),
            server: "18,240 files, 41.7 GB".into(),
            disk: "21,530 files, 52.3 GB".into(),
            dirty: true,
            preview: true,
            preview_text: "Fetch 0 files (0 B); remove 3,290 files from disk.".into(),
            preview_paths: "− Content/Cinematics/Intro.umap\n− Content/Cinematics/Intro_Audio.uasset\n− Content/Cinematics/Outro.umap".into(),
            ..ViewState::default()
        });
        r.set_pane(Pane::View);
        shot("view-editor");

        // The files pane in Workspace scope: the folder tree and the list with kind icons and
        // lock badges.
        let a = w.global::<Assets>();
        a.set_local_only(false);
        a.set_folder("Content/Textures".into());
        a.set_lock_known(true);
        a.set_folders(ModelRc::from(Rc::new(VecModel::from(vec![
            DepotFolder {
                expanded: true,
                ..Default::default()
            },
            DepotFolder {
                path: "Content".into(),
                caption: "Content".into(),
                depth: 1,
                parent_index: 0,
                expanded: true,
            },
            DepotFolder {
                path: "Content/Maps".into(),
                caption: "Maps".into(),
                depth: 2,
                parent_index: 1,
                expanded: false,
            },
            DepotFolder {
                path: "Content/Textures".into(),
                caption: "Textures".into(),
                depth: 2,
                parent_index: 1,
                expanded: true,
            },
        ]))));
        let card = |name: &str, local: bool, holder: &str| AssetCard {
            path: format!("Content/Textures/{name}").into(),
            name: name.into(),
            kind: changes::file_kind(name).into(),
            picture: lamp(true),
            preview: true,
            local,
            writable: local && holder != "bob",
            mine: holder == "alice",
            size: "1.2 MB".into(),
            holder: holder.into(),
            ..AssetCard::default()
        };
        a.set_rows(ModelRc::from(Rc::new(VecModel::from(vec![
            card("Lamp_D.png", true, ""),
            card("Lamp_N.png", true, "bob"),
            card("Waves.png", false, ""),
            card("Harbor.png", true, ""),
            card("Boat.png", true, "alice"),
        ]))));
        let mut sel = card("Lamp_N.png", true, "bob");
        sel.mine = false;
        sel.comment = "Retexturing for harbor-night".into();
        sel.workspace = "D:\\work\\game".into();
        a.set_selected(sel);
        a.set_page_label("1–5 of 5".into());
        r.set_pane(Pane::Files);
        shot("files");
        let search = w.global::<AssetSearch>();
        search.set_query("rock".into());
        search.set_paths(
            [
                "Content/Art/BigRock.uasset".into(),
                "Environment/Cliffs/BigRock.uasset".into(),
            ]
            .as_slice()
            .into(),
        );
        search.set_message("2 matches".into());
        w.set_dialog(Dialog::AssetSearch);
        shot("asset-search");
        w.set_dialog(Dialog::None);
        a.set_local_only(false);
        a.set_selected(AssetCard::default());
        a.set_rows(ModelRc::default());
        a.set_folders(ModelRc::default());
        a.set_folder("".into());

        r.set_pane(Pane::Changes);
        w.set_workspaces(ModelRc::default());

        // T135: links handed to the app, a new version, and an error on Home.
        w.set_current_tab(0);
        let l = w.global::<Links>();
        l.set_link(LinkState {
            text: "lode://someone@lode.example.com/game/r1".into(),
            why: "Links do not carry user names.".into(),
            ..LinkState::default()
        });
        w.set_dialog(Dialog::LinkRefused);
        shot("link-refused");
        l.set_link(LinkState {
            text: "lode://lode.example.com/game/r1482".into(),
            why: "".into(),
            target: "r1482".into(),
            depot: "game".into(),
            host: "lode.example.com".into(),
        });
        l.set_choices(ModelRc::from(Rc::new(VecModel::from(vec![
            LinkChoice {
                path: "D:\\work\\game".into(),
                branch: "main".into(),
                open: true,
            },
            LinkChoice {
                path: "E:\\review\\game".into(),
                branch: "release-2.4".into(),
                open: false,
            },
        ]))));
        w.set_dialog(Dialog::LinkChoose);
        shot("link-choose");
        l.set_link(LinkState {
            text: "lode://lode.example.com/tools/shelf/12".into(),
            why: "".into(),
            target: "shelf 12".into(),
            depot: "tools".into(),
            host: "lode.example.com".into(),
        });
        w.set_dialog(Dialog::LinkNone);
        shot("link-none");
        w.set_dialog(Dialog::None);

        let u = w.global::<Updates>();
        u.set_offer(UpdateOffer {
            shown: true,
            version: "0.2.0".into(),
            current: "0.1.0".into(),
            notes: "Lode Desk is a native app now: faster start, less memory, and one window per computer that takes every lode:// link.".into(),
            busy: false,
            error: ErrorInfo::default(),
        });
        u.set_notes_open(true);
        shot("update");
        u.set_offer(UpdateOffer::default());
        u.set_notes_open(false);

        w.set_home_error(app::error_info(&lode_desk::desk::ErrorView {
            code: "not_a_workspace".into(),
            message: "D:\\Downloads is not inside a Lode workspace".into(),
            details: serde_json::Value::Null,
        }));
        shot("open-error");
        w.set_home_error(ErrorInfo::default());

        w.set_dialog(Dialog::About);
        shot("about");
    }
    println!("screenshots in {}", out.display());
}

const SAMPLE_PATCH: &str = "--- a/Source/Game/Boat.cpp\n+++ b/Source/Game/Boat.cpp\n@@ -12,9 +12,11 @@ void ABoat::Tick(float DeltaTime)\n {\n     Super::Tick(DeltaTime);\n-    const float Speed = 10.0f;\n-    const float Roll = FMath::Sin(Time * 0.5f) * 3.0f;\n+    const float Speed = 12.0f;\n+    const float Roll = FMath::Sin(Time * 0.35f) * 3.0f;\n+    // Bob with the tide.\n+    const float Bob = Tide->HeightAt(GetActorLocation());\n     AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);\n-    SetActorRotation(FRotator(0.f, Yaw, Roll));\n+    SetActorRotation(FRotator(0.f, Yaw, Roll));\n+    SetActorLocation(GetActorLocation() + FVector(0.f, 0.f, Bob));\n     Time += DeltaTime;\n }\n";

/// Branches as `Desk::branches` lists them.
fn sample_branches() -> Vec<serde_json::Value> {
    let b = |name: &str, head: u64, parent: serde_json::Value, state: &str| json!({"name": name, "head_rev": head, "parent": parent, "state": state});
    vec![
        b("main", 1482, serde_json::Value::Null, "active"),
        b(
            "harbor-night",
            1477,
            json!({"branch": "main", "rev": 1440}),
            "active",
        ),
        b(
            "release/1.2",
            1460,
            json!({"branch": "main", "rev": 1400}),
            "active",
        ),
        b(
            "feature/boat-physics",
            1451,
            json!({"branch": "main", "rev": 1431}),
            "active",
        ),
        b(
            "old-water",
            1200,
            json!({"branch": "main", "rev": 1100}),
            "archived",
        ),
    ]
}

/// A conflict's base, mine, theirs and the file with its markers.
fn sample_conflict() -> (&'static str, &'static str, &'static str, &'static str) {
    (
        "void ABoat::Tick(float DeltaTime)\n{\n    Super::Tick(DeltaTime);\n    const float Speed = 10.0f;\n    AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);\n}\n",
        "void ABoat::Tick(float DeltaTime)\n{\n    Super::Tick(DeltaTime);\n    const float Speed = 12.0f;\n    AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);\n}\n",
        "void ABoat::Tick(float DeltaTime)\n{\n    Super::Tick(DeltaTime);\n    const float Speed = Tide->SpeedAt(GetActorLocation());\n    AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);\n    Bob(DeltaTime);\n}\n",
        "void ABoat::Tick(float DeltaTime)\n{\n    Super::Tick(DeltaTime);\n<<<<<<< mine\n    const float Speed = 12.0f;\n=======\n    const float Speed = Tide->SpeedAt(GetActorLocation());\n>>>>>>> r1477\n    AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);\n    Bob(DeltaTime);\n}\n",
    )
}

/// Locks as `Desk::locks` lists them.
fn sample_locks() -> Vec<serde_json::Value> {
    let lock = |path: &str, who: &str, mine: bool, comment: &str| {
        json!({"path": path, "owner": {"name": who, "kind": "user"}, "mine": mine,
               "workspace": if mine { "alice-pc" } else { "" },
               "created_at": "2026-11-14T08:15:00Z", "comment": comment})
    };
    vec![
        lock("Content/Maps/Harbor.umap", "alice", true, "Lighting pass"),
        lock("Content/Boats/Skiff.uasset", "alice", true, ""),
        lock("Content/Props/Door.uasset", "bob", false, "Door art"),
        lock("Content/Water/Tide.uasset", "refactor-bot", false, ""),
    ]
}

/// Shelves as `Desk::shelves` lists them, the first with its changes and diff.
fn sample_shelves() -> Vec<serde_json::Value> {
    vec![
        json!({"id": 12, "version": 3, "state": "open", "branch": "main", "base_rev": 1478,
               "description": "Boats bob with the tide\n\nThe boats follow the tide height instead of a sine.",
               "owner": {"name": "alice", "kind": "user"}, "updated_at": "2026-11-14T09:30:00Z",
               "changes": [{"path": "Source/Game/Boat.cpp", "action": "modify"}],
               "diff": [{"path": "Source/Game/Boat.cpp", "patch": SAMPLE_PATCH}]}),
        json!({"id": 14, "version": 1, "state": "open", "branch": "main", "base_rev": 1481,
               "description": "Night fog for the harbor",
               "owner": {"name": "bob", "kind": "user"}, "updated_at": "2026-11-14T10:05:00Z"}),
        json!({"id": 9, "version": 2, "state": "open", "branch": "main", "base_rev": 1470,
               "description": "Rename tide parameters",
               "owner": {"name": "refactor-bot", "kind": "agent"}, "agent_owner": "dan",
               "updated_at": "2026-11-12T16:20:00Z"}),
    ]
}

/// Check results as `Desk::checks` answers them, for revisions and one shelf.
fn sample_checks() -> serde_json::Value {
    let ci = |name: &str, state: &str, summary: &str| {
        json!({"name": name, "state": state, "summary": summary,
               "url": format!("https://ci.example.com/{name}"),
               "reporter": {"name": "ci", "kind": "user"}, "updated_at": "2026-11-14T09:40:00Z"})
    };
    json!({"available": true, "checks": {
        "r1482": [ci("ci/build", "pending", "")],
        "r1481": [ci("ci/build", "success", ""), ci("ci/test", "failure", "2 of 418 tests failed")],
        "r1480": [ci("ci/build", "success", ""), ci("ci/test", "success", "418 passed")],
        "r1478": [ci("ci/build", "success", "Built in 6m 12s"), ci("ci/test", "success", "418 passed")],
        "12@3": [ci("ci/build", "success", "Built in 5m 40s"), ci("ci/test", "failure", "Boat roll test failed")],
        "14@1": [ci("ci/build", "pending", "")],
    }})
}

fn sample_history() -> Vec<serde_json::Value> {
    let rev = |n: u64, branch: &str, who: &str, kind: &str, msg: &str| {
        json!({
            "rev": n, "branch": branch, "parent": n - 1,
            "author": {"name": who, "kind": kind},
            "submitter": {"name": who, "kind": kind},
            "created_at": format!("2026-11-{:02}T{:02}:{:02}:00Z", 14 - (1482 - n) / 3, 9 + n % 9, n % 60),
            "message": msg,
            "merged_from": [],
        })
    };
    let mut v = vec![
        rev(
            1482,
            "main",
            "alice",
            "user",
            "Harbor lights: lamps along the pier",
        ),
        rev(
            1481,
            "main",
            "bob",
            "user",
            "Tune the tide height for the night map",
        ),
        rev(
            1480,
            "main",
            "refactor-bot",
            "agent",
            "Rename BoatComponent to FloatComponent",
        ),
        rev(1479, "main", "carol", "user", "Lamp textures, first pass"),
        rev(
            1478,
            "main",
            "alice",
            "user",
            "Merge harbor-night into main\n\nBoats bob with the tide, lamps glow at night.",
        ),
        rev(1477, "main", "dan", "user", "Fix the dock collision"),
        rev(1476, "main", "bob", "user", "Audio: waves loop"),
        rev(1475, "main", "carol", "user", "Imported from Subversion"),
        rev(
            1474,
            "main",
            "alice",
            "user",
            "Streaming pool size for consoles",
        ),
        rev(1473, "main", "dan", "user", "Tidy the harbor level"),
    ];
    v[2]["provenance"] = json!({"agent": {"session": "s-41"}});
    v[4]["merged_from"] = json!([{"branch": "harbor-night", "from_rev": 1461, "to_rev": 1474}]);
    v[4]["changes"] = json!([
        {"path": "Source/Game/Boat.cpp", "action": "modify"},
        {"path": "Source/Game/Tide.h", "action": "add"},
        {"path": "Content/Textures/Lamp_D.png", "action": "modify"},
        {"path": "Content/Maps/Harbor.umap", "action": "modify"},
        {"path": "Docs/old-tide.md", "action": "delete"},
    ]);
    v[4]["provenance"] = json!({"client": "lode-desk 0.2.0"});
    v[7]["provenance"] = json!({"imported": {"system": "svn", "id": "12345"}});
    v
}

fn sample_blame() -> serde_json::Value {
    let code = [
        (1201, "void ABoat::Tick(float DeltaTime)"),
        (1201, "{"),
        (1201, "    Super::Tick(DeltaTime);"),
        (1478, "    const float Speed = 12.0f;"),
        (
            1478,
            "    const float Roll = FMath::Sin(Time * 0.35f) * 3.0f;",
        ),
        (1478, "    // Bob with the tide."),
        (
            1478,
            "    const float Bob = Tide->HeightAt(GetActorLocation());",
        ),
        (
            1350,
            "    AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);",
        ),
        (1350, "    SetActorRotation(FRotator(0.f, Yaw, Roll));"),
        (
            1478,
            "    SetActorLocation(GetActorLocation() + FVector(0.f, 0.f, Bob));",
        ),
        (1201, "    Time += DeltaTime;"),
        (1201, "}"),
    ];
    json!({
        "path": "Source/Game/Boat.cpp",
        "rev": 1482,
        "lines": code.iter().map(|(r, t)| json!({"rev": r, "text": t})).collect::<Vec<_>>(),
        "revisions": [
            {"rev": 1201, "branch": "main", "author": {"name": "dan"}, "created_at": "2026-09-02T10:00:00Z", "summary": "Boats move forward"},
            {"rev": 1350, "branch": "main", "author": {"name": "bob"}, "created_at": "2026-10-11T10:00:00Z", "summary": "Boats roll in the waves"},
            {"rev": 1478, "branch": "main", "author": {"name": "alice"}, "created_at": "2026-11-13T10:00:00Z", "summary": "Merge harbor-night into main"},
        ],
    })
}

/// A lamp-like disc, dark or lit.
fn lamp(lit: bool) -> Image {
    let size = 256u32;
    let mut px = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    let w = px.width();
    for (i, p) in px.make_mut_slice().iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        let d = ((x - 128.0).powi(2) + (y - 110.0).powi(2)).sqrt();
        let glow = if lit { (1.0 - d / 120.0).max(0.0) } else { 0.0 };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let c = |base: f32, add: f32| (base + add * glow).min(255.0) as u8;
        *p = if d < 40.0 {
            Rgba8Pixel {
                r: c(90.0, 165.0),
                g: c(90.0, 150.0),
                b: c(70.0, 40.0),
                a: 255,
            }
        } else {
            Rgba8Pixel {
                r: c(20.0, 120.0),
                g: c(30.0, 100.0),
                b: c(60.0, 20.0),
                a: 255,
            }
        };
    }
    Image::from_rgba8(px)
}

fn sample_pending() -> Pending {
    let file = |path: &str, status: &str, list: Option<&str>| FileEntry {
        path: path.into(),
        status: status.into(),
        changelist: list.map(String::from),
        copy_from: None,
        moved: false,
        locked: false,
        needs_lock: path.ends_with(".umap"),
        lock_required: false,
    };
    Pending {
        branch: "main".into(),
        rev: Some(1482),
        files: vec![
            file("Content/Maps/Harbor.umap", "modified", None),
            file("Source/Game/Boat.cpp", "modified", None),
            file("Source/Game/Boat.h", "modified", None),
            file("Docs/tide.md", "deleted", None),
            file("Content/Textures/Lamp_D.png", "added", Some("lamps")),
            file("Content/Audio/Waves.wav", "modified", Some("lamps")),
            file("Content/Textures/Lamp_N.png", "untracked", None),
        ],
        lists: vec![ListInfo {
            name: "lamps".into(),
            description: "Street lamps along the pier".into(),
        }],
        merges: Vec::new(),
    }
}

fn recent(depot: &str, branch: &str, path: &str, host: &str) -> Recent {
    Recent {
        depot: depot.into(),
        branch: branch.into(),
        path: path.into(),
        host: host.into(),
        initial: depot[..1].to_uppercase().into(),
    }
}

fn render(window: &MinimalSoftwareWindow, size: PhysicalSize, file: &Path) {
    slint::platform::update_timers_and_animations();
    let (width, height) = (size.width as usize, size.height as usize);
    let mut pixels = vec![Rgb8Pixel::default(); width * height];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    });
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    let f = std::fs::File::create(file).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), size.width, size.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&bytes)
        .unwrap();
}
