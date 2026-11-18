use super::item::{ItemKind, MediaClip, ProjectItem};
use super::sequence::{conformed_size, sane_par};
use super::{BTreeMap, ClipId, EffectInstance, ItemId, Param, ParamValue, Project, TimeRange, TrackItem, Vec2, effect};
use filmcraft_geom::Affine;

/// Resolve NaN "auto" point defaults (frame centre / source centre) in an effect instance.
pub fn resolve_auto_points(e: &mut EffectInstance, frame: (u32, u32), source: (u32, u32)) {
    resolve_auto_points_sized(e, frame, Some(source));
}

/// [`resolve_auto_points`] for a source whose size may not be known yet (`None`): the points
/// measured in the source (`anchor`) then stay "auto".
fn resolve_auto_points_sized(e: &mut EffectInstance, frame: (u32, u32), source: Option<(u32, u32)>) {
    for (k, p) in e.params.iter_mut() {
        if let ParamValue::Vec2(v) = &mut p.value {
            let Some((w, h)) = (if k == "anchor" { source } else { Some(frame) }) else { continue };
            if let Some((fx, fy)) = effect::auto_point(&e.effect, k) {
                if v.x.is_nan() {
                    v.x = w as f64 * fx;
                }
                if v.y.is_nan() {
                    v.y = h as f64 * fy;
                }
            }
            if v.x.is_nan() {
                v.x = w as f64 / 2.0;
            }
            if v.y.is_nan() {
                v.y = if k == "end" { h as f64 } else { h as f64 / 2.0 };
            }
        }
    }
    let _ = Vec2::ZERO;
}

impl Project {
    /// Lazy upgrade for graphic layers written before they had stable uids: assign a unique
    /// [`crate::graphic_design::LayerExtra::uid`] to every uid-less or duplicated graphic layer
    /// in sequences and source graphics.
    ///
    /// Called when a project file is opened, before undo history starts, so every later undo
    /// snapshot carries the same identity. The field is already optional in the file schema, so
    /// this changes no schema version and older builds still read the file. Returns whether any
    /// layer changed.
    pub fn ensure_graphic_layer_uids(&mut self) -> bool {
        let mut changed = false;
        for item in self.items.values_mut() {
            if let ItemKind::Sequence(seq) = &mut item.kind {
                let seq = std::sync::Arc::make_mut(seq);
                for track in seq.video_tracks.iter_mut().chain(seq.audio_tracks.iter_mut()) {
                    for it in &mut track.items {
                        changed |= crate::graphic::ensure_layer_uids(&mut it.effects);
                    }
                }
            }
        }
        for graphic in self.source_graphics.values_mut() {
            changed |= crate::graphic::ensure_layer_uids(&mut graphic.layers);
        }
        changed
    }

    /// Available picture and sound, resolving a subclip to its parent media. Missing parents,
    /// cycles and overly deep subclip chains carry neither stream.
    pub fn source_streams(&self, item: ItemId) -> (bool, bool) {
        let Some(pi) = self.item(item) else { return (false, false) };
        match &pi.kind {
            ItemKind::Subclip { .. } => self.resolve_media(item).map_or((false, false), |(_, m, _)| (m.info.has_video(), m.info.has_audio())),
            _ => (pi.has_video(), pi.has_audio()),
        }
    }

    /// Resolve media through at most sixteen items, preserving the outermost subclip range.
    /// Missing parents, cycles and longer chains are unavailable media.
    pub fn resolve_media(&self, item: ItemId) -> Option<(ItemId, &MediaClip, Option<TimeRange>)> {
        let mut id = item;
        let mut range = None;
        for _ in 0..16 {
            match &self.item(id)?.kind {
                ItemKind::Media(media) => return Some((id, media, range)),
                ItemKind::Subclip { parent, range: span, .. } => {
                    range.get_or_insert(*span);
                    id = *parent;
                }
                _ => return None,
            }
        }
        None
    }

    /// After the media item `media` was turned clockwise by `delta` quarter turns (Interpret
    /// Footage ▸ Rotation; `old_size` is its picture size before the turn): bring its clips in
    /// sequences along. For every clip of `media` (or of a subclip of it), what is measured in the
    /// clip's own pixels moves to where that part of the picture now is: the Motion anchor point
    /// (so the picture turns in place, about its anchor), every effect mask, and the edges of the
    /// Crop effect. Other effects keep their settings. With `fold`, a clip whose Motion ▸ Rotation is not animated and is exactly the same
    /// turn was already stood upright by hand in its sequence: that rotation becomes 0 (and, with
    /// Uniform Scale off, its two scales swap), so the clip looks exactly as it did before instead
    /// of turning twice. Media with pixels that are not square is never folded. Returns (clips
    /// folded, clips that turned with the footage).
    pub fn turn_sequence_clips(&mut self, media: ItemId, old_size: (u32, u32), delta: u8, fold: bool) -> (usize, usize) {
        let delta = delta % 4;
        if delta == 0 {
            return (0, 0);
        }
        let fold = fold && self.source_par(media).is_none_or(|p| p == (1, 1));
        let uses_media = |p: &Project, item: ItemId| p.resolve_media(item).is_some_and(|(id, _, _)| id == media);
        // which clips: decided on the shared project, before a sequence is detached for the edit
        let uses: Vec<(ItemId, Vec<ClipId>)> = self
            .sequences()
            .filter_map(|i| {
                let clips: Vec<ClipId> =
                    i.as_sequence()?.video_tracks.iter().flat_map(|t| &t.items).filter(|ti| uses_media(self, ti.item)).map(|ti| ti.id).collect();
                (!clips.is_empty()).then_some((i.id, clips))
            })
            .collect();
        let (mut folded, mut turned) = (0, 0);
        for (seq_id, clips) in uses {
            let Some(seq) = self.sequence_mut(seq_id) else { continue };
            for ti in seq.video_tracks.iter_mut().flat_map(|t| &mut t.items).filter(|ti| clips.contains(&ti.id)) {
                if turn_clip(ti, old_size, delta, fold) {
                    folded += 1;
                } else {
                    turned += 1;
                }
            }
        }
        (folded, turned)
    }

    /// Size in pixels of what an item shows: a media clip's picture, a sequence's frame, an
    /// adjustment layer or graphic; a subclip has the size of its parent. `None` without
    /// picture. This is the size the renderer centres a clip's "auto" anchor in.
    pub fn source_size(&self, item: ItemId) -> Option<(u32, u32)> {
        let mut id = item;
        // subclips of subclips: bounded, a damaged project could point a subclip at itself
        for _ in 0..16 {
            return match &self.item(id)?.kind {
                ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)),
                ItemKind::Sequence(q) => Some((q.settings.width, q.settings.height)),
                ItemKind::AdjustmentLayer { width, height, .. } | ItemKind::Graphic { width, height, .. } => Some((*width, *height)),
                ItemKind::Subclip { parent, .. } => {
                    id = *parent;
                    continue;
                }
            };
        }
        None
    }

    /// Pixel aspect ratio of what an item shows, in lowest terms: a media clip's
    /// ([`MediaClip::pixel_aspect`], Interpret Footage first), a nested sequence's, a subclip its
    /// parent's. `None` for items drawn in the pixels of whatever sequence they are in (graphics,
    /// adjustment layers) and for an item that doesn't exist.
    pub fn source_par(&self, item: ItemId) -> Option<(u32, u32)> {
        let mut id = item;
        // subclips of subclips: bounded, as in `source_size`
        for _ in 0..16 {
            return match &self.item(id)?.kind {
                ItemKind::Media(m) => Some(m.pixel_aspect()),
                ItemKind::Sequence(q) => Some(sane_par(q.settings.par)),
                ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => None,
                ItemKind::Subclip { parent, .. } => {
                    id = *parent;
                    continue;
                }
            };
        }
        None
    }

    /// [`Project::source_size`] in the pixels of a frame whose pixel aspect is `frame_par`
    /// ([`conformed_size`]): what Scale to Frame Size, Set to Frame Size and Fit / Fill fit into
    /// the frame. Items without a pixel aspect of their own take the frame's.
    pub fn conformed_source_size(&self, item: ItemId, frame_par: (u32, u32)) -> Option<(f64, f64)> {
        let size = self.source_size(item)?;
        Some(conformed_size(size, self.source_par(item).unwrap_or(frame_par), frame_par))
    }

    /// Resolve the "auto" (NaN) points of the clips in the sequences `sequences` selects, for
    /// clips that did not go through a placing command (an interchange import builds them with
    /// the effect defaults). Points measured in the frame use the sequence's size. `anchor` is
    /// measured in the clip's source ([`Project::source_size`], a subclip through its parent) and
    /// is resolved only when `size_is_final` says so for that source item (the media item for a
    /// subclip); otherwise it stays "auto", which the renderer centres in whatever size the media
    /// turns out to have. Points that are not NaN are never touched, so this can run again when
    /// more sizes are known.
    pub fn resolve_placed_auto_points(&mut self, sequences: impl Fn(ItemId) -> bool, size_is_final: impl Fn(&ProjectItem) -> bool) {
        let seq_ids: Vec<ItemId> = self.items.values().filter(|i| matches!(i.kind, ItemKind::Sequence(_)) && sequences(i.id)).map(|i| i.id).collect();
        for seq_id in seq_ids {
            let Some(seq) = self.sequence(seq_id) else { continue };
            // the source size of each clip that has an unresolved point, looked up before the edit
            let sources: BTreeMap<ClipId, Option<(u32, u32)>> = seq
                .all_tracks()
                .flat_map(|t| t.items.iter())
                .filter(|c| c.effects.iter().any(has_auto_point))
                .map(|c| (c.id, self.final_source_size(c.item, &size_is_final)))
                .collect();
            if sources.is_empty() {
                continue;
            }
            let Some(seq) = self.sequence_mut(seq_id) else { continue };
            let frame = (seq.settings.width, seq.settings.height);
            for clip in seq.all_tracks_mut().flat_map(|t| t.items.iter_mut()) {
                let Some(source) = sources.get(&clip.id) else { continue };
                for e in &mut clip.effects {
                    resolve_auto_points_sized(e, frame, *source);
                }
            }
        }
    }

    /// [`Project::source_size`] of `item` when the item that carries the size (the media item of
    /// a subclip) passes `size_is_final`.
    fn final_source_size(&self, item: ItemId, size_is_final: &impl Fn(&ProjectItem) -> bool) -> Option<(u32, u32)> {
        let mut id = item;
        for _ in 0..16 {
            let it = self.item(id)?;
            match &it.kind {
                ItemKind::Subclip { parent, .. } => id = *parent,
                _ => return if size_is_final(it) { self.source_size(id) } else { None },
            }
        }
        None
    }
}

fn has_auto_point(e: &EffectInstance) -> bool {
    e.params.values().any(|p| matches!(&p.value, ParamValue::Vec2(v) if v.x.is_nan() || v.y.is_nan()))
}

/// [`Project::turn_sequence_clips`] for one clip: what is measured in the clip's own pixels
/// follows the picture (the Motion anchor point, every effect mask, the edges of Crop), and with
/// `fold` a Motion rotation equal to the turn is moved out. True when it was folded.
fn turn_clip(ti: &mut TrackItem, old_size: (u32, u32), delta: u8, fold: bool) -> bool {
    let (w, h) = (f64::from(old_size.0), f64::from(old_size.1));
    // where a point of the lying picture is in the turned one
    let to_turned = match delta {
        1 => Affine { a: 0.0, b: 1.0, c: -1.0, d: 0.0, e: h, f: 0.0 },
        2 => Affine { a: -1.0, b: 0.0, c: 0.0, d: -1.0, e: w, f: h },
        _ => Affine { a: 0.0, b: -1.0, c: 1.0, d: 0.0, e: 0.0, f: w },
    };
    let mut folded = false;
    for e in &mut ti.effects {
        for m in &mut e.masks {
            m.path.map_values(|v| match v {
                ParamValue::Path(path) => ParamValue::Path(path.transformed(&to_turned)),
                other => other,
            });
        }
        match e.effect.as_str() {
            "motion" => folded = turn_motion(e, &to_turned, delta, fold),
            "crop" => {
                // the edge a side of the lying picture has become: clockwise, its left edge is the top one
                let from: [&str; 4] = match delta {
                    1 => ["left", "top", "right", "bottom"],
                    2 => ["bottom", "left", "top", "right"],
                    _ => ["right", "bottom", "left", "top"],
                };
                let old: Vec<Option<Param>> = from.iter().map(|k| e.params.remove(*k)).collect();
                for (k, p) in ["top", "right", "bottom", "left"].iter().zip(old) {
                    if let Some(p) = p {
                        e.params.insert((*k).to_string(), p);
                    }
                }
            }
            _ => {}
        }
    }
    folded
}

/// [`turn_clip`] for the Motion effect: true when its rotation was the turn and has been moved
/// out (folded).
fn turn_motion(m: &mut EffectInstance, to_turned: &Affine, delta: u8, fold: bool) -> bool {
    if let Some(a) = m.param_mut("anchor") {
        // an "auto" (NaN) anchor stays the centre
        a.map_values(|v| match v {
            ParamValue::Vec2(p) if !p.x.is_nan() && !p.y.is_nan() => ParamValue::Vec2(to_turned.apply(p)),
            other => other,
        });
    }
    let is_turn = m.param("rotation").is_some_and(|r| {
        !r.is_animated() && matches!(r.value, ParamValue::Float(deg) if deg.is_finite() && (deg.rem_euclid(360.0) - f64::from(delta) * 90.0).abs() < 1e-6)
    });
    if !(fold && m.enabled && is_turn) {
        return false;
    }
    if let Some(r) = m.param_mut("rotation") {
        r.value = ParamValue::Float(0.0);
    }
    // Scale is applied before Rotation: the height scale of the lying picture is the width scale of the standing one
    let uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
    if !uniform
        && delta % 2 == 1
        && let (Some(s), Some(sw)) = (m.params.remove("scale"), m.params.remove("scale_width"))
    {
        m.params.insert("scale".into(), sw);
        m.params.insert("scale_width".into(), s);
    }
    true
}
