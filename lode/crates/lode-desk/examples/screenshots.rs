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
    SignInState, WorkspaceTab,
};
use lode_desk::app::{BlameState, DiffNote, DiffState, HistoryState, ImageState, Pane, Reading};
use lode_desk::changes::{self, FileEntry, ListInfo, Pending};
use lode_desk::{diffs, history};
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
            ..SignInState::default()
        });
        w.set_dialog(Dialog::SignIn);
        shot("sign-in");

        w.set_sign_in(SignInState {
            server: "https://lode.example.com".into(),
            name: "alice".into(),
            password: "wrong".into(),
            password_login: true,
            error: app::error_info(&lode_desk::desk::ErrorView {
                code: "unauthenticated".into(),
                message: "the name or password is wrong".into(),
                details: serde_json::Value::Null,
            }),
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
                server: "https://lode.example.com".into(),
            },
            WorkspaceTab {
                root: "D:\\work\\engine".into(),
                depot: "engine".into(),
                branch: "release-2.4".into(),
                rev: "977".into(),
                server: "https://lode.example.com".into(),
            },
        ]))));
        w.set_current_tab(1);
        shot("workspace");

        // T131: the changes view, a refused commit and changelists.
        let pending = sample_pending();
        let picked: BTreeSet<String> = ["Content/Maps/Harbor.umap", "Source/Game/Boat.cpp"]
            .into_iter()
            .map(String::from)
            .collect();
        let selected = "Source/Game/Boat.cpp";
        w.set_change_rows(ModelRc::from(Rc::new(VecModel::from(changes::rows(
            &pending,
            &picked,
            Some(selected),
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
        r.set_revs(ModelRc::from(Rc::new(VecModel::from(history::rows(
            &revisions,
            "main",
            true,
            Some(1478),
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
        r.set_pane(Pane::Changes);
        w.set_workspaces(ModelRc::default());

        w.set_current_tab(0);
        w.set_dialog(Dialog::About);
        shot("about");
    }
    println!("screenshots in {}", out.display());
}

const SAMPLE_PATCH: &str = "--- a/Source/Game/Boat.cpp\n+++ b/Source/Game/Boat.cpp\n@@ -12,9 +12,11 @@ void ABoat::Tick(float DeltaTime)\n {\n     Super::Tick(DeltaTime);\n-    const float Speed = 10.0f;\n-    const float Roll = FMath::Sin(Time * 0.5f) * 3.0f;\n+    const float Speed = 12.0f;\n+    const float Roll = FMath::Sin(Time * 0.35f) * 3.0f;\n+    // Bob with the tide.\n+    const float Bob = Tide->HeightAt(GetActorLocation());\n     AddActorWorldOffset(GetActorForwardVector() * Speed * DeltaTime);\n-    SetActorRotation(FRotator(0.f, Yaw, Roll));\n+    SetActorRotation(FRotator(0.f, Yaw, Roll));\n+    SetActorLocation(GetActorLocation() + FVector(0.f, 0.f, Bob));\n     Time += DeltaTime;\n }\n";

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
        rev(1475, "main", "carol", "user", "Imported from Perforce"),
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
    v[7]["provenance"] = json!({"imported": {"system": "perforce", "id": "12345"}});
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
