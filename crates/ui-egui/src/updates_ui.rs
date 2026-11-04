//! Software updates, background check at startup, and startup prompt modal.

use serde_json::Value;

use crate::FilmcraftApp;

#[cfg(not(target_arch = "wasm32"))]
use craft_update::updater::{Action, Status, Updater};
#[cfg(not(target_arch = "wasm32"))]
use craft_update::{CheckResultDialog, CheckResultResponse};

pub(crate) struct Runtime {
    #[cfg(not(target_arch = "wasm32"))]
    updater: Updater,
    /// The desktop app turns the check at start on; tests and the web build leave it off.
    #[cfg(not(target_arch = "wasm32"))]
    startup_check: bool,
    #[cfg(not(target_arch = "wasm32"))]
    check_requested: bool,
    /// The context of the last frame, to wake the UI when a check is queued from a menu callback.
    #[cfg(not(target_arch = "wasm32"))]
    repaint: Option<egui::Context>,
    pub(crate) manual_window_open: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            updater: Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")),
            #[cfg(not(target_arch = "wasm32"))]
            startup_check: false,
            #[cfg(not(target_arch = "wasm32"))]
            check_requested: false,
            #[cfg(not(target_arch = "wasm32"))]
            repaint: None,
            manual_window_open: false,
        }
    }
}

impl FilmcraftApp {
    /// Whether the app asks for a newer release when it starts (off until the desktop app turns it on).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_startup_update_check(&mut self, on: bool) {
        self.updates_rt.startup_check = on;
    }
}

/// What Help ▸ Check for Updates… answers in DSPV Cut.
pub(crate) const FROM_DSPV_HUB: &str = "Updates for DSPV Cut come from DSPV Hub (Apps).";

/// Help ▸ Check for Updates… (`app.checkForUpdates`, also from the native menu, the control server and MCP).
///
/// DSPV patch: updates come from DSPV Hub. Upstream starts the updater here, which asks GitHub for FilmCraft's releases and
/// offers to download one over this build (same bundle id). Our builds are signed and shipped only through the DSPV Hub's
/// Apps panel, so the command starts no check and opens no window: it says where updates come from, on the status line and
/// in its result.
pub(crate) fn check(app: &mut FilmcraftApp) -> Result<Value, String> {
    let message = crate::i18n::t(FROM_DSPV_HUB).to_string();
    app.ui.status = message.clone();
    Ok(serde_json::json!({ "message": message }))
}

pub(crate) fn tick(app: &mut FilmcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let rt = &mut app.updates_rt;
        rt.repaint = Some(ctx.clone());
        if std::mem::take(&mut rt.check_requested) {
            rt.updater.check_now(ctx);
        } else if rt.startup_check {
            rt.updater.check_at_start(ctx);
        }
        rt.updater.tick(ctx);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}

pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // The restart helper is scheduled before the app closes, so no update starts over unsaved work.
        let clean = !app.session.is_dirty();
        match app.updates_rt.updater.show_if(ctx, clean) {
            Action::OpenUrl(url) => crate::links::open(app, ctx, &url),
            Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::None => {}
        }
        // The dialog answers Help ▸ Check for Updates. A check that finds a release leaves the status
        // idle and the startup prompt takes over, so the flag ends with the check.
        let rt = &mut app.updates_rt;
        if rt.manual_window_open {
            if matches!(rt.updater.status(), Status::Idle) && !rt.check_requested {
                rt.manual_window_open = false;
            } else {
                let dialog = CheckResultDialog::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION"));
                match dialog.show(ctx, rt.updater.status()) {
                    CheckResultResponse::Close => {
                        rt.manual_window_open = false;
                        rt.updater.dismiss_status();
                    }
                    CheckResultResponse::CheckAgain => rt.check_requested = true,
                    CheckResultResponse::None => {}
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    if app.updates_rt.manual_window_open {
        let mut open = true;
        egui::Window::new(crate::i18n::t("FilmCraft Updates"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.label(crate::i18n::t("Update check is not supported in the web browser."));
                ui.add_space(8.0);
                if ui.button(crate::i18n::t("OK")).clicked() {
                    app.updates_rt.manual_window_open = false;
                }
            });
        if !open {
            app.updates_rt.manual_window_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_is_idle() {
        let rt = Runtime::default();
        assert!(!rt.manual_window_open);
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(!rt.startup_check);
            assert!(!rt.updater.prompt_open());
            assert_eq!(rt.updater.status(), &Status::Idle);
        }
    }

    /// DSPV patch: updates come from DSPV Hub. The command (menu, native menu, control server, MCP all run it through
    /// `menus::invoke`) answers with the message and starts no check: the updater stays idle and no window opens.
    #[test]
    fn check_for_updates_says_dspv_hub_and_starts_no_check() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        let r = crate::menus::invoke(&mut app, &ctx, "app.checkForUpdates", serde_json::json!({})).expect("the command answers");
        assert_eq!(r["message"], FROM_DSPV_HUB);
        assert_eq!(app.ui.status, FROM_DSPV_HUB);
        assert!(!app.updates_rt.manual_window_open);
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(!app.updates_rt.check_requested);
            tick(&mut app, &ctx);
            assert_eq!(app.updates_rt.updater.status(), &Status::Idle, "no check started");
        }
    }

    /// DSPV patch: updates come from DSPV Hub. A new app never checks at start, and the desktop app leaves it off.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_startup_check_stays_off() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(!app.updates_rt.startup_check);
        app.set_startup_update_check(false);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            tick(&mut app, &ctx);
        }
        assert_eq!(app.updates_rt.updater.status(), &Status::Idle);
        assert!(!app.updates_rt.updater.prompt_open());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn startup_prompt_shows_and_closes() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        let release = craft_update::Release {
            tag: "v99.0.0".into(),
            version: craft_update::parse_version("99.0.0"),
            name: "FilmCraft 99.0.0".into(),
            html_url: "https://github.com/storytold/filmcraft/releases/tag/v99.0.0".into(),
            ..Default::default()
        };
        app.updates_rt.updater.offer(release);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(&mut app, ui.ctx());
        });
        output.textures_delta.clear();
        assert!(app.updates_rt.updater.prompt_open());
    }
}
