//! The page around the editor (the DSPV hub) can take over saving. It sets `window.filmcraftHost`
//! (the editor's own window; the page and the editor are the same origin) with any of:
//!
//! ```js
//! filmcraftHost.pickSave = (name) => { /* show a Save dialog, then run
//!     filmcraft.execute("file.saveAs", { path: "/projects/<name>.fcproj" }) */ }
//! filmcraftHost.onProjectWrite = (path, bytes) => true   // true (or nothing): handled, no download
//!                                                        // false: download it as usual
//! filmcraftHost.runCommand = (id) => {}   // File ▸ Send to Review etc.: the commands the page added with
//!                                          // filmcraft.setHostCommands([{id, label, menu: ["File"]}])
//! ```
//!
//! Without them the editor saves by downloading, as the stock web app does. A host function that
//! throws is logged and the editor falls back to its old behaviour, so a bug on the page never
//! loses a save.

use wasm_bindgen::{JsCast, JsValue};

use crate::host_save::{WriteRoute, route_write};

fn host_fn(name: &str) -> Option<js_sys::Function> {
    let host = js_sys::Reflect::get(&js_sys::global(), &"filmcraftHost".into()).ok()?;
    if host.is_undefined() || host.is_null() {
        return None;
    }
    js_sys::Reflect::get(&host, &name.into()).ok()?.dyn_into::<js_sys::Function>().ok()
}

fn host() -> JsValue {
    js_sys::Reflect::get(&js_sys::global(), &"filmcraftHost".into()).unwrap_or(JsValue::UNDEFINED)
}

/// Ask the host page to take over File ▸ Save (and Save As, Save a Copy). `true`: the page has
/// the request and will save when its user has chosen, so the editor does nothing now.
pub fn pick_save(name: &str) -> bool {
    let Some(f) = host_fn("pickSave") else { return false };
    match f.call1(&host(), &JsValue::from_str(name)) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("host pickSave failed, saving by download: {e:?}");
            false
        }
    }
}

/// A file was written. `true`: the host page took it, so there is no download.
pub fn written(path: &str, data: &[u8]) -> bool {
    let f = host_fn("onProjectWrite");
    if route_write(path, f.is_some()) != WriteRoute::Host {
        return false;
    }
    let Some(f) = f else { return false };
    let bytes = js_sys::Uint8Array::from(data);
    match f.call2(&host(), &JsValue::from_str(path), &bytes) {
        Ok(answer) => answer.as_bool() != Some(false),
        Err(e) => {
            log::warn!("host onProjectWrite failed, downloading {path}: {e:?}");
            false
        }
    }
}

/// Run one of the commands the page added to the menus: the page's `runCommand(id)`.
pub fn run_command(id: &str) -> Result<(), String> {
    let Some(f) = host_fn("runCommand") else { return Err("this command is not available".into()) };
    f.call1(&host(), &JsValue::from_str(id)).map(|_| ()).map_err(|e| {
        js_sys::Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string()).or_else(|| e.as_string()).unwrap_or_else(|| "the command failed".into())
    })
}
