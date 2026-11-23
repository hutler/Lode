//! Search the readable depot manifest, then reveal one exact path in the file inspector.
use super::read::View;
use super::{spawn, App, AppWindow, AssetSearch, Assets, Dialog};
use futures::future::AbortHandle;
use slint::{ComponentHandle, Model, ModelRc, SharedString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Default)]
pub(super) struct SearchState {
    generation: Cell<u64>,
    request: RefCell<Option<AbortHandle>>,
    pub(super) reveal: RefCell<Option<String>>,
}

impl SearchState {
    pub(super) fn cancel(&self) {
        self.generation.set(self.generation.get() + 1);
        if let Some(handle) = self.request.borrow_mut().take() {
            handle.abort();
        }
    }
}

impl App {
    pub(super) fn dismiss_asset_search(&self, w: &AppWindow) {
        if w.get_dialog() == Dialog::AssetSearch {
            self.search.cancel();
            w.set_dialog(Dialog::None);
        }
    }
}

fn clear(w: &AppWindow) {
    let s = w.global::<AssetSearch>();
    s.set_paths(ModelRc::default());
    s.set_message(SharedString::new());
    s.set_busy(false);
}

pub(super) fn connect(w: &AppWindow, app: &Rc<App>) {
    let ui = w.global::<AssetSearch>();
    let a = app.clone();
    ui.on_open(move || {
        let Some(w) = a.window.upgrade() else { return };
        if w.get_dialog() != Dialog::None || a.shown_root.borrow().is_none() {
            return;
        }
        a.search.cancel();
        clear(&w);
        w.global::<AssetSearch>().set_query(SharedString::new());
        w.set_dialog(Dialog::AssetSearch);
    });
    let a = app.clone();
    ui.on_edited(move || {
        a.search.cancel();
        if let Some(w) = a.window.upgrade() {
            clear(&w);
        }
    });
    let a = app.clone();
    ui.on_run(move || {
        let Some(w) = a.window.upgrade() else { return };
        let Some(root) = a.shown_root.borrow().clone() else {
            return;
        };
        let Some(tab) = a.workspaces_row(&root) else {
            return;
        };
        let s = w.global::<AssetSearch>();
        let query = s.get_query().trim().to_lowercase();
        if query.is_empty() || s.get_busy() {
            return;
        }
        clear(&w);
        let Ok(rev) = tab.rev.parse() else {
            s.set_message("Workspace revision is unavailable.".into());
            return;
        };
        a.search.cancel();
        let generation = a.search.generation.get();
        let (abort, registration) = AbortHandle::new_pair();
        *a.search.request.borrow_mut() = Some(abort);
        s.set_busy(true);
        s.set_message("Searching depot…".into());
        let me = a.clone();
        spawn(async move {
            let res = me
                .desk
                .find_assets(
                    tab.server_url.into(),
                    tab.depot.into(),
                    rev,
                    query,
                    registration,
                )
                .await;
            if me.search.generation.get() != generation {
                return;
            }
            me.search.request.borrow_mut().take();
            let Some(w) = me.window.upgrade() else { return };
            if w.get_dialog() != Dialog::AssetSearch {
                return;
            }
            let s = w.global::<AssetSearch>();
            s.set_busy(false);
            match res {
                Ok((paths, more)) => {
                    let text = match paths.len() {
                        0 => "No matching files.".to_owned(),
                        1 => "1 match".to_owned(),
                        n if more => format!("First {n} matches. Refine the name to see others."),
                        n => format!("{n} matches"),
                    };
                    let paths: Vec<SharedString> = paths.into_iter().map(Into::into).collect();
                    s.set_paths(paths.as_slice().into());
                    s.set_message(text.into());
                }
                Err(e) => s.set_message(e.message.into()),
            }
        });
    });
    let a = app.clone();
    ui.on_reveal(move |path| {
        let Some(w) = a.window.upgrade() else { return };
        if w.get_dialog() != Dialog::AssetSearch
            || !w
                .global::<AssetSearch>()
                .get_paths()
                .iter()
                .any(|p| p == path)
        {
            return;
        }
        let folder = path.rsplit_once('/').map_or("", |(p, _)| p).to_owned();
        a.close_dialog();
        if w.global::<Assets>().get_local_only() {
            w.global::<Assets>().set_local_only(false);
            a.assets.dirs.borrow_mut().clear();
        }
        *a.search.reveal.borrow_mut() = Some(path.to_string());
        a.open_view(View::Files(folder));
    });
}
