//! macOS open-documents and quit Apple events.
//!
//! Finder and Open With deliver paths through LaunchServices, rather than command-line arguments.
//! `fmv-macos-events` registers a safe main-thread handler without replacing winit's app delegate.

use serde_json::{Value, json};

/// Open an arriving project only when the session is clean, then import any media. Media alone must
/// never create a project: the current one may contain unsaved work (or already be Untitled).
pub fn arrival_commands(paths: &[String], dirty: bool) -> Vec<(&'static str, Value)> {
    let (project, media) = crate::args::project_and_media(paths);
    let mut commands = Vec::new();
    if !dirty && let Some(path) = project {
        commands.push(("file.open", json!({"path": path})));
    }
    if !media.is_empty() {
        commands.push(("file.import", json!({"paths": media})));
    }
    commands
}

#[cfg(target_os = "macos")]
use fmv_macos_events::{Event, Inbox, Registration};

/// Keeps the handlers registered until the event loop returns, including graphics retries.
#[cfg(target_os = "macos")]
pub struct AppleEvents {
    _registration: Registration,
    inbox: Inbox,
}

#[cfg(target_os = "macos")]
impl AppleEvents {
    /// Call on the main thread before `run_native`, so the launch event is caught too.
    pub fn install() -> Self {
        let (registration, inbox) = Registration::install();
        Self { _registration: registration, inbox }
    }

    /// Connect once the egui context exists; later events wake the window to drain the queue.
    pub fn connect(&self, ctx: &egui::Context) -> Inbox {
        let ctx = ctx.clone();
        self.inbox.set_wake(move || ctx.request_repaint());
        self.inbox.clone()
    }
}

/// Dispatch arrivals through the same engine commands as launch files, and honour Dock Quit.
#[cfg(target_os = "macos")]
pub fn poll(inbox: &Inbox, app: &mut filmcraft_ui_egui::FilmcraftApp, ctx: &egui::Context) {
    let mut documents = Vec::new();
    for event in inbox.drain() {
        match event {
            Event::Open(paths) => documents.extend(paths.into_iter().map(|p| p.to_string_lossy().into_owned())),
            Event::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
    let dirty = app.session.is_dirty();
    if dirty && let Some(path) = documents.iter().find(|path| crate::args::is_project(path)) {
        let name = std::path::Path::new(path).file_name().unwrap_or_else(|| std::ffi::OsStr::new(path)).to_string_lossy();
        app.ui.status = format!("Save the current project first, then open {name} again.");
    }
    for (id, params) in arrival_commands(&documents, dirty) {
        if let Err(e) = app.session.execute(id, params) {
            log::error!("{id}: {e}");
            app.ui.status = e.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::arrival_commands;
    use serde_json::json;

    #[test]
    fn media_arrivals_import_into_the_open_project() {
        assert_eq!(
            arrival_commands(&["/n/a b/clip.mp4".into(), "/n/x.png".into()], false),
            vec![("file.import", json!({"paths": ["/n/a b/clip.mp4", "/n/x.png"]}))]
        );
    }

    #[test]
    fn a_project_file_still_opens_as_a_project() {
        assert_eq!(arrival_commands(&["/p/reel.fcproj".into()], false), vec![("file.open", json!({"path": "/p/reel.fcproj"}))]);
    }

    #[test]
    fn a_project_arrival_does_not_replace_unsaved_work() {
        assert!(arrival_commands(&["/p/reel.fcproj".into()], true).is_empty());
    }

    #[test]
    fn a_dirty_session_still_imports_media_when_a_project_arrives() {
        assert_eq!(arrival_commands(&["/p/reel.fcproj".into(), "/n/clip.mp4".into()], true), vec![("file.import", json!({"paths": ["/n/clip.mp4"]}))]);
    }

    #[test]
    fn nothing_arrives_nothing_runs() {
        assert!(arrival_commands(&[], false).is_empty());
    }
}
