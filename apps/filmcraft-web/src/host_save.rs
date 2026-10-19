//! When the editor runs inside the DSPV hub, the page around it decides where a project is saved
//! (pure rules here, so they are tested natively in `tests/host_save.rs`; the JavaScript calls
//! live in [`crate::host`]).
//!
//! Without a host page the editor keeps its old behaviour: Save writes `/projects/<name>` in
//! memory and the browser downloads it.

/// Projects the editor writes are under this folder (`pick_save` hands out `/projects/<name>`).
pub const PROJECT_DIR: &str = "/projects/";

/// Whether a written file is a project the host page should be told about: a `.fcproj` the
/// editor saved, as opposed to an export, a caption file or an image, which are still downloads.
pub fn is_project_write(path: &str) -> bool {
    path.starts_with(PROJECT_DIR) && path.len() > PROJECT_DIR.len() && path.to_ascii_lowercase().ends_with(".fcproj")
}

/// What to do with a written file when a host page has asked to see project writes.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteRoute {
    /// Tell the host page; if it takes it, no download.
    Host,
    /// Offer it as a browser download, as always.
    Download,
}

pub fn route_write(path: &str, host_listening: bool) -> WriteRoute {
    if host_listening && is_project_write(path) { WriteRoute::Host } else { WriteRoute::Download }
}
