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

/// The menus a page may add commands to (the editor's own top-level menus).
const MENUS: [&str; 9] = ["File", "Edit", "Clip", "Sequence", "Markers", "Graphics and Titles", "View", "Window", "Help"];
const MAX_COMMANDS: usize = 8;

/// A command the page asks to add to the menus: id, label, menu path.
pub type PageCommand = (String, String, Vec<String>);

/// Read the list a page passes to `filmcraft.setHostCommands`: `[{id, label, menu: ["File"]}]`. Entries that are malformed (no id or label, an unknown
/// top-level menu, too long) are dropped, so a bad page cannot put junk in the menus; at most [`MAX_COMMANDS`] are kept.
pub fn parse_host_commands(list: &serde_json::Value) -> Vec<PageCommand> {
    let short = |v: Option<&serde_json::Value>| {
        v.and_then(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= 64).map(str::to_string)
    };
    let mut out = Vec::new();
    for item in list.as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(id), Some(label)) = (short(item.get("id")), short(item.get("label"))) else { continue };
        let menu: Vec<String> =
            item.get("menu").and_then(serde_json::Value::as_array).map(|a| a.iter().filter_map(|v| short(Some(v))).collect()).unwrap_or_default();
        if menu.first().is_none_or(|top| !MENUS.contains(&top.as_str())) || out.iter().any(|(existing, _, _): &PageCommand| *existing == id) {
            continue;
        }
        out.push((id, label, menu));
        if out.len() == MAX_COMMANDS {
            break;
        }
    }
    out
}
