//! Desktop menus and the connection dialog. The CLI and Desk share the proxy settings.
use super::{spawn, App, AppWindow, Connection, Dialog, OpenBrowser};
use slint::ComponentHandle;
use std::rc::Rc;

const GUIDE: &str = "https://github.com/hutler/Lode/blob/main/desk/docs/guide.md";

pub(super) fn connect(w: &AppWindow, app: &Rc<App>, browser: OpenBrowser) {
    let a = app.clone();
    w.on_guide(move || {
        if !(browser)(GUIDE) {
            if let Some(w) = a.window.upgrade() {
                let err = super::ErrorInfo {
                    shown: true,
                    message: format!("Open the Desk guide at {GUIDE}").into(),
                    ..Default::default()
                };
                if w.get_current_tab() == 0 {
                    w.set_home_error(err);
                } else {
                    w.set_workspace_error(err);
                }
            }
        }
    });
    let weak = w.as_weak();
    w.on_exit_requested(move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.hide();
        }
    });
    let ui = w.global::<Connection>();
    let a = app.clone();
    ui.on_open(move || {
        let Some(w) = a.window.upgrade() else { return };
        if w.get_dialog() != Dialog::None {
            return;
        }
        let server = a
            .shown_root
            .borrow()
            .as_deref()
            .and_then(|r| a.workspaces_row(r))
            .map(|t| t.server_url.to_string())
            .unwrap_or_else(|| a.desk.default_connection().unwrap_or_default());
        let c = w.global::<Connection>();
        c.set_server(server.clone().into());
        c.set_proxy(a.desk.saved_proxy(&server).unwrap_or_default().into());
        c.set_report("".into());
        c.set_busy(false);
        w.set_dialog(Dialog::Connection);
    });
    let a = app.clone();
    ui.on_edited(move |url| {
        if let Some(w) = a.window.upgrade() {
            let c = w.global::<Connection>();
            c.set_proxy(a.desk.saved_proxy(&url).unwrap_or_default().into());
            c.set_report("".into());
        }
    });
    let a = app.clone();
    ui.on_test(move || a.connection_action(false));
    let a = app.clone();
    ui.on_save(move || a.connection_action(true));
}

impl App {
    fn connection_action(&self, save: bool) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let c = w.global::<Connection>();
        if c.get_busy() {
            return;
        }
        let server = c.get_server().trim().to_string();
        let proxy = c.get_proxy().trim().to_string();
        c.set_busy(true);
        c.set_report(
            if save {
                "Saving…"
            } else {
                "Checking connection…"
            }
            .into(),
        );
        let Some(me) = self.me() else { return };
        spawn(async move {
            let report = if save {
                me.desk.save_proxy(server, proxy).await.map(|v| {
                    v["proxy"].as_str().map_or_else(
                        || "Direct downloads selected.".into(),
                        |p| format!("Saved download proxy {p}."),
                    )
                })
            } else {
                me.desk
                    .connection_test(server)
                    .await
                    .map(|report| lode_ops::cmd::doctor::lines(&report).join("\n"))
            };
            if let Some(w) = me.window.upgrade() {
                let c = w.global::<Connection>();
                c.set_busy(false);
                c.set_report(report.unwrap_or_else(|e| e.message).into());
            }
        });
    }
}
