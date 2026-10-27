//! Auto-save and crash recovery in OPFS (the desktop recovery journal, mapped to the browser).
//!
//! - While the project has unsaved changes, a snapshot is written every few seconds to
//!   `recovery/snapshot.fcproj` with `recovery/meta.json` saying it is dirty; once the project is
//!   saved (downloaded) the meta is marked clean.
//! - Imported media are copied to `media/<name>` (the browser streams the copy; nothing goes
//!   through wasm memory), so the snapshot's `/files/<name>` references resolve after a reload.
//! - At start-up a dirty snapshot is reopened (`?norecover` skips it, `?fresh` also skips the media).
//!   A skipped snapshot stays dirty until this session writes its own ([`crate::recovery_policy`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use filmcraft_engine::Session;
use serde_json::{Value, json};

use crate::opfs;
use crate::recovery_guard::{self, Start};
use crate::recovery_policy::{Action, Policy};

const SNAPSHOT: &str = "recovery/snapshot.fcproj";
const META: &str = "recovery/meta.json";
/// Media larger than this are not copied to OPFS.
const KEEP_MAX: f64 = 4.0 * 1024.0 * 1024.0 * 1024.0;

/// A finished OPFS write, reported back to the next [`Autosave::tick`].
enum Outcome {
    Snapshot { revision: u64, ok: bool },
    Clean { ok: bool },
}

#[derive(Default)]
pub struct Autosave {
    policy: Policy,
    outcomes: Rc<RefCell<Vec<Outcome>>>,
}

impl Autosave {
    /// Called every frame.
    pub fn tick(&mut self, s: &Session, ctx: &egui::Context) {
        if !opfs::available() {
            return;
        }
        let now = ctx.input(|i| i.time);
        for o in self.outcomes.borrow_mut().drain(..) {
            match o {
                Outcome::Snapshot { revision, ok } => self.policy.written(revision, ok, now),
                Outcome::Clean { ok } => self.policy.cleaned(ok, now),
            }
        }
        match self.policy.tick(now, s.revision, s.saved_revision) {
            Action::Nothing => {}
            Action::WakeAfter(secs) => ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs)),
            Action::WriteSnapshot(revision) => {
                // Snapshot and meta both have to land; either failing retries the pair.
                let failed = Rc::new(Cell::new(false));
                let left = Rc::new(Cell::new(2u8));
                let done = |out: Rc<RefCell<Vec<Outcome>>>, failed: Rc<Cell<bool>>, left: Rc<Cell<u8>>, ctx: egui::Context| -> opfs::Done {
                    Box::new(move |ok| {
                        failed.set(failed.get() || !ok);
                        left.set(left.get().saturating_sub(1));
                        if left.get() == 0 {
                            out.borrow_mut().push(Outcome::Snapshot { revision, ok: !failed.get() });
                            ctx.request_repaint();
                        }
                    })
                };
                let meta = json!({"dirty": true, "name": s.project.name, "revision": s.revision, "time": js_sys::Date::now()});
                let d1 = done(self.outcomes.clone(), failed.clone(), left.clone(), ctx.clone());
                let d2 = done(self.outcomes.clone(), failed, left, ctx.clone());
                opfs::write_then(SNAPSHOT.into(), filmcraft_format::encode(&s.project, false), Some(d1));
                opfs::write_then(META.into(), meta.to_string().into_bytes(), Some(d2));
            }
            Action::WriteClean => {
                let (out, ctx) = (self.outcomes.clone(), ctx.clone());
                let done: opfs::Done = Box::new(move |ok| {
                    out.borrow_mut().push(Outcome::Clean { ok });
                    ctx.request_repaint();
                });
                opfs::write_then(META.into(), json!({"dirty": false}).to_string().into_bytes(), Some(done));
            }
        }
    }
}

/// The unsaved snapshot of a session that ended without saving: (file name, bytes).
pub async fn load_snapshot() -> Option<(String, Vec<u8>)> {
    if !opfs::available() {
        return None;
    }
    let meta: Value = serde_json::from_slice(&opfs::read(META).await?).ok()?;
    if meta.get("dirty").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let bytes = opfs::read(SNAPSHOT).await?;
    let name = meta.get("name").and_then(Value::as_str).unwrap_or("Recovered").to_string();
    Some((format!("{name}.fcproj"), bytes))
}

/// Keep a copy of an imported file in OPFS (background).
pub fn keep_media(path: &str, file: &web_sys::File) {
    if !opfs::available() || file.size() > KEEP_MAX {
        return;
    }
    let Some(name) = path.strip_prefix("/files/").map(str::to_string) else { return };
    let blob: web_sys::Blob = file.clone().into();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = opfs::store_blob(&format!("media/{name}"), &blob).await {
            log::warn!("keeping {name} in OPFS: {e:?}");
        }
    });
}

/// Register the media kept in OPFS under their `/files/` paths; returns how many.
pub async fn restore_media() -> usize {
    let files = opfs::files_in("media").await;
    let n = files.len();
    for (name, f) in files {
        crate::fs::register_blob(&name, f.into());
    }
    n
}

/// What start-up found in OPFS.
pub struct Startup {
    /// Media copies registered.
    pub restored: usize,
    /// The unsaved snapshot to reopen: (file name, bytes).
    pub recovered: Option<(String, Vec<u8>)>,
    /// Said to the user when an autosave was put aside instead of reopened.
    pub skipped: Option<String>,
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn marker_set() -> bool {
    storage().and_then(|s| s.get_item(recovery_guard::KEY).ok()).flatten().is_some()
}

fn set_marker() {
    if let Some(s) = storage() {
        let _ = s.set_item(recovery_guard::KEY, &js_sys::Date::now().to_string());
    }
}

fn clear_marker() {
    if let Some(s) = storage() {
        let _ = s.remove_item(recovery_guard::KEY);
    }
}

/// Decide what to reopen (see [`recovery_guard`]). Reopening an autosave sets a `localStorage` marker that is removed once the editor
/// has stayed up; a start that finds the marker still set puts the autosave aside (still downloadable through
/// `filmcraft.crashedSnapshot()`) and starts clean, so an autosave that crashes the page cannot do it again on every reload.
pub async fn startup(no_recover: bool, force: bool) -> Startup {
    let none = Startup { restored: 0, recovered: None, skipped: None };
    if !opfs::available() {
        return none;
    }
    match recovery_guard::start(marker_set(), no_recover, force) {
        Start::Skip => none,
        Start::SkipCrashed => {
            clear_marker();
            let Some((name, bytes)) = load_snapshot().await else { return none };
            opfs::write(recovery_guard::ASIDE.into(), bytes);
            opfs::write(META.into(), json!({"dirty": false, "crashedName": name}).to_string().into_bytes());
            let note = format!("The last autosave of {name} stopped the editor when it was reopened, so it was not reopened. It is kept in this browser.");
            Startup { skipped: Some(note), ..none }
        }
        Start::Recover => {
            let recovered = load_snapshot().await;
            if recovered.is_none() {
                return Startup { restored: restore_media().await, ..none };
            }
            set_marker();
            wasm_bindgen_futures::spawn_local(async {
                crate::fs::sleep((recovery_guard::SETTLE_S * 1000.0) as i32).await;
                clear_marker();
            });
            Startup { restored: restore_media().await, recovered, skipped: None }
        }
    }
}

/// The autosave that was put aside by [`startup`], if any.
pub async fn crashed_snapshot() -> Option<Vec<u8>> {
    opfs::read(recovery_guard::ASIDE).await
}
