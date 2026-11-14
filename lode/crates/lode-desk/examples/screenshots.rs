//! Renders the screens of the Slint app into PNG files for review, light and dark, with
//! the software renderer (no window system needed).
//!
//! `cargo run --example screenshots -- <folder>` (default `target/screenshots`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use lode_desk::app::{
    self, CheckoutState, Dialog, ErrorInfo, Options, Recent, SignInState, WorkspaceTab,
};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, VecModel};

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
        w.set_workspaces(ModelRc::default());

        w.set_current_tab(0);
        w.set_dialog(Dialog::About);
        shot("about");
    }
    println!("screenshots in {}", out.display());
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
