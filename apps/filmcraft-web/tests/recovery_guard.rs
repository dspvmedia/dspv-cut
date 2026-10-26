//! The crash-loop guard's decision. It is pure, so it runs natively even though the rest of the crate is wasm32-only.

#[path = "../src/recovery_guard.rs"]
mod recovery_guard;

use recovery_guard::{Start, start};

/// A normal start reopens the autosave.
#[test]
fn a_normal_start_recovers() {
    assert_eq!(start(false, false, false), Start::Recover);
}

/// A marker left by an attempt that never finished means the autosave is put aside, not reopened again.
#[test]
fn a_leftover_marker_skips_the_autosave_that_crashed() {
    assert_eq!(start(true, false, false), Start::SkipCrashed);
}

/// `?recoverforce` tries again anyway.
#[test]
fn force_tries_again() {
    assert_eq!(start(true, false, true), Start::Recover);
}

/// `?fresh` / `?norecover` win over everything, and leave the autosave where it is.
#[test]
fn a_clean_start_is_asked_for() {
    assert_eq!(start(true, true, false), Start::Skip);
    assert_eq!(start(false, true, true), Start::Skip);
}
