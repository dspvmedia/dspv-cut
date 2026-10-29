//! The shared decoder and picture budgets. They are pure, so they run natively even though the rest of the
//! crate is wasm32-only.

// The crate uses the module's limits; this test reaches the rest of it.
#[path = "../src/decode_budget.rs"]
#[allow(dead_code)]
mod decode_budget;

use decode_budget::{FRAMES_PER_SOURCE, FrameCache, Open, pick_victim};

/// 78 clips each wanting a full cache of pictures stay inside the byte budget.
#[test]
fn many_clips_stay_inside_the_budget() {
    let mut c: FrameCache<u32> = FrameCache::default();
    let budget = 100;
    for source in 0..78u32 {
        for pts in 0..40i64 {
            c.insert(source, pts, 0, 3, 20, budget);
            assert!(c.bytes() <= budget, "over budget after clip {source} frame {pts}: {}", c.bytes());
        }
    }
    assert!(c.len() <= budget / 3);
}

/// The picture just stored is kept even when it alone is over the budget (the frame being shown must survive).
#[test]
fn the_picture_just_stored_survives_a_tiny_budget() {
    let mut c: FrameCache<u32> = FrameCache::default();
    c.insert(1, 5, 7, 50, 5, 10);
    assert_eq!(c.get(1, 5), Some(7));
    c.insert(1, 6, 8, 50, 6, 10);
    assert_eq!(c.get(1, 6), Some(8));
    assert_eq!(c.get(1, 5), None);
}

/// Least recently used goes first, across clips; a picture that was read recently outlives older ones.
#[test]
fn least_recently_used_goes_first_across_clips() {
    let mut c: FrameCache<u32> = FrameCache::default();
    c.insert(1, 0, 10, 10, 0, 30);
    c.insert(2, 0, 20, 10, 0, 30);
    c.insert(3, 0, 30, 10, 0, 30);
    assert_eq!(c.get(1, 0), Some(10)); // clip 1 is now the most recent
    c.insert(4, 0, 40, 10, 0, 30);
    assert_eq!(c.get(2, 0), None);
    assert_eq!(c.get(1, 0), Some(10));
    assert_eq!(c.get(4, 0), Some(40));
}

/// One clip keeps the pictures nearest the wanted frame, up to its own cap, and the byte count follows.
#[test]
fn a_clip_keeps_the_pictures_nearest_the_wanted_frame() {
    let mut c: FrameCache<i64> = FrameCache::default();
    for pts in 0..(FRAMES_PER_SOURCE as i64 + 10) {
        c.insert(9, pts, pts, 1, 100, usize::MAX);
    }
    assert_eq!(c.count(9), FRAMES_PER_SOURCE);
    assert_eq!(c.bytes(), FRAMES_PER_SOURCE);
    // the wanted frame is 100, so the highest times are the ones kept
    let top = FRAMES_PER_SOURCE as i64 + 9;
    assert_eq!(c.get(9, top), Some(top));
    assert_eq!(c.get(9, 0), None);
}

/// Replacing a picture does not count its bytes twice; dropping a clip returns all of its bytes.
#[test]
fn bytes_follow_replacement_and_removal() {
    let mut c: FrameCache<u32> = FrameCache::default();
    c.insert(1, 0, 1, 10, 0, usize::MAX);
    c.insert(1, 0, 2, 15, 0, usize::MAX);
    c.insert(2, 0, 3, 5, 0, usize::MAX);
    assert_eq!(c.bytes(), 20);
    c.remove_source(1);
    assert_eq!(c.bytes(), 5);
    assert_eq!(c.count(1), 0);
    c.remove_source(2);
    assert!(c.is_empty());
    assert_eq!(c.bytes(), 0);
}

fn open(id: u32, busy: bool, last_output: f64) -> Open {
    Open { id, busy, last_output }
}

/// Below the cap nothing is closed; at it, the idle decoder that delivered longest ago is.
#[test]
fn the_oldest_idle_decoder_is_closed_at_the_cap() {
    let all = [open(1, false, 500.0), open(2, false, 100.0), open(3, false, 300.0)];
    assert_eq!(pick_victim(&all, 4), None);
    assert_eq!(pick_victim(&all, 3), Some(2));
}

/// A decoder working towards a frame is never closed, even if it is the oldest; with all busy, none is (the cap is soft).
#[test]
fn a_busy_decoder_is_never_closed() {
    let all = [open(1, true, 1.0), open(2, false, 900.0), open(3, true, 2.0)];
    assert_eq!(pick_victim(&all, 3), Some(2));
    let busy = [open(1, true, 1.0), open(2, true, 2.0)];
    assert_eq!(pick_victim(&busy, 2), None);
}
