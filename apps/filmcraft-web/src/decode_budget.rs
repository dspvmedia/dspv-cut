//! What the browser decoder path may hold at once, across every imported clip (pure, so it is tested natively in
//! `tests/decode_budget.rs`; the WebCodecs side lives in [`crate::webcodecs`]).
//!
//! Each imported clip owns a decoder and a cache of decoded pictures. Counted per clip, a project of 78 clips filled the
//! 4 GiB wasm heap by itself on open (about 118 MB of pictures per clip, then the page hung or aborted). So both are
//! shared budgets instead:
//!
//! - [`FrameCache`]: decoded pictures of all clips together, at most a byte budget; the least recently used go first, but
//!   never the picture just stored. Each clip also keeps at most [`FRAMES_PER_SOURCE`], the ones nearest the frame wanted.
//! - [`pick_victim`]: which idle decoder to close when a new one is wanted beyond [`MAX_DECODERS`]. A closed decoder
//!   restarts at a key frame when its clip is next shown, so nothing is lost but a little time.

use std::collections::BTreeMap;

/// Decoded pictures kept per clip (the ones nearest the wanted frame).
pub const FRAMES_PER_SOURCE: usize = 40;
/// Bytes of decoded pictures kept for all clips together.
pub const FRAME_BUDGET: usize = 256 << 20;
/// Browser decoders kept open at once before idle ones are closed (hardware decoders are a scarce resource too).
pub const MAX_DECODERS: usize = 6;

/// Decoded pictures of all clips, keyed by (clip, presentation time).
pub struct FrameCache<V> {
    map: BTreeMap<(u32, i64), Entry<V>>,
    bytes: usize,
    clock: u64,
}

struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
}

impl<V> Default for FrameCache<V> {
    fn default() -> Self {
        Self { map: BTreeMap::new(), bytes: 0, clock: 0 }
    }
}

impl<V: Clone> FrameCache<V> {
    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Pictures held.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Pictures held for one clip.
    pub fn count(&self, source: u32) -> usize {
        self.map.range((source, i64::MIN)..=(source, i64::MAX)).count()
    }

    /// The picture at `pts`, marked as just used.
    pub fn get(&mut self, source: u32, pts: i64) -> Option<V> {
        self.clock += 1;
        let used = self.clock;
        let e = self.map.get_mut(&(source, pts))?;
        e.used = used;
        Some(e.value.clone())
    }

    /// Store a picture (replacing the one at that time), then trim: the clip to [`FRAMES_PER_SOURCE`] pictures nearest
    /// `want`, and all clips to `budget` bytes, least recently used first. The picture just stored is never the one dropped.
    pub fn insert(&mut self, source: u32, pts: i64, value: V, bytes: usize, want: i64, budget: usize) {
        self.clock += 1;
        if let Some(old) = self.map.insert((source, pts), Entry { value, bytes, used: self.clock }) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        self.trim_source(source, want, (source, pts));
        self.trim_budget(budget, (source, pts));
    }

    fn trim_source(&mut self, source: u32, want: i64, keep: (u32, i64)) {
        while self.count(source) > FRAMES_PER_SOURCE {
            let far = self
                .map
                .range((source, i64::MIN)..=(source, i64::MAX))
                .map(|(k, _)| *k)
                .filter(|k| *k != keep)
                .max_by_key(|k| (k.1 - want).abs());
            let Some(far) = far else { break };
            self.remove(far);
        }
    }

    /// Drop least recently used pictures until `budget` bytes are held (never `keep`).
    pub fn trim_budget(&mut self, budget: usize, keep: (u32, i64)) {
        while self.bytes > budget {
            let oldest = self.map.iter().filter(|(k, _)| **k != keep).min_by_key(|(_, e)| e.used).map(|(k, _)| *k);
            let Some(oldest) = oldest else { break };
            self.remove(oldest);
        }
    }

    fn remove(&mut self, key: (u32, i64)) {
        if let Some(e) = self.map.remove(&key) {
            self.bytes -= e.bytes;
        }
    }

    /// Drop every picture of a clip (it was closed).
    pub fn remove_source(&mut self, source: u32) {
        let keys: Vec<_> = self.map.range((source, i64::MIN)..=(source, i64::MAX)).map(|(k, _)| *k).collect();
        for k in keys {
            self.remove(k);
        }
    }
}

/// An open decoder as the eviction sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Open {
    pub id: u32,
    /// Working towards a frame someone is waiting for, or copying a picture out: not to be closed.
    pub busy: bool,
    /// Milliseconds timestamp of its last output (older goes first).
    pub last_output: f64,
}

/// The decoder to close so another can open: none while fewer than `max` are open or all are busy, else the idle one that
/// delivered longest ago.
pub fn pick_victim(open: &[Open], max: usize) -> Option<u32> {
    if open.len() < max {
        return None;
    }
    open.iter().filter(|o| !o.busy).min_by(|a, b| a.last_output.total_cmp(&b.last_output)).map(|o| o.id)
}
