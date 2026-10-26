//! Breaking a crash loop on reload (pure, so it is tested natively in `tests/recovery_guard.rs`; the storage lives in
//! [`crate::recovery`]).
//!
//! The editor keeps an autosave and reopens it when the page loads. If opening that autosave is what crashes the page
//! (a project with more footage than the browser can hold), reloading would restore it and crash again, for ever. So a
//! marker is set in `localStorage` (it survives a crashed tab) before the autosave is reopened and removed once the editor
//! has stayed up for [`SETTLE_S`] seconds. A page that starts with the marker still set knows the last attempt did not
//! survive: it does not reopen the autosave but puts it aside ([`ASIDE`]) and starts clean.
//!
//! The fatal-error dialog's "Start a new project" sets the same marker, so it takes the same path.

/// `localStorage` key of the marker.
pub const KEY: &str = "filmcraft.recovering";
/// Seconds the editor must stay up after reopening an autosave before the marker is removed.
pub const SETTLE_S: f64 = 45.0;
/// Where an autosave that was not reopened is kept (OPFS), so it can still be downloaded.
pub const ASIDE: &str = "recovery/crashed.fcproj";

/// What start-up does about the autosave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// Reopen it (and set the marker until the editor has settled).
    Recover,
    /// Not reopened: `?fresh` / `?norecover` ask for a clean start; an autosave stays as it is.
    Skip,
    /// The last attempt to reopen it did not survive: put it aside and start clean.
    SkipCrashed,
}

/// `marker`: the marker is set. `no_recover`: `?fresh` or `?norecover`. `force`: `?recoverforce` (try again anyway).
pub fn start(marker: bool, no_recover: bool, force: bool) -> Start {
    if no_recover {
        Start::Skip
    } else if marker && !force {
        Start::SkipCrashed
    } else {
        Start::Recover
    }
}
