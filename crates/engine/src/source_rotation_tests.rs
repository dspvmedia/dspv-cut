//! Interpret Footage ▸ Rotation: footage a camera stored on its side (no display rotation in the
//! file) stands upright wherever it is read — the pool's source for the monitors and thumbnails,
//! its proxy, sequences and exports — clips already turned by hand in a sequence do not turn
//! twice, import rules set it at import, and files made from the clip (proxies) stay stored the
//! way the clip's own file is.

use std::path::Path;
use std::sync::Arc;

use filmcraft_media::{DemoScene, FrameRequest, MediaSource};
use filmcraft_project::{ClipId, ItemId, ParamValue, SequenceSettings, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};

/// The lying picture: wider than high, as the camera stored it.
const W: u32 = 96;
const H: u32 = 56;

fn size(s: &Session, item: ItemId) -> (u32, u32) {
    let v = s.project.item(item).unwrap().as_media().unwrap().info.video.clone().unwrap();
    (v.width, v.height)
}

fn rotation(s: &Session, item: ItemId) -> u8 {
    s.project.item(item).unwrap().as_media().unwrap().rotation()
}

/// Frame 2 of `src` as RGBA8 with its size.
fn frame(src: &dyn MediaSource) -> (u32, u32, Vec<u8>) {
    let rate = src.info().frame_rate();
    let f = src.video_frame(FrameRequest::full(rate.tick_of(2))).unwrap();
    (f.width, f.height, f.to_rgba8())
}

/// Frame 2 of the file at `path` as stored, turned clockwise by `turns` quarter turns.
fn stored_frame_turned(s: &Session, path: &Path, turns: u8) -> (u32, u32, Vec<u8>) {
    let src = s.media.open_file(&path.to_string_lossy(), &*s.services).unwrap();
    let rate = src.info().frame_rate();
    let f = src.video_frame(FrameRequest::full(rate.tick_of(2))).unwrap().rotated(turns);
    (f.width, f.height, f.to_rgba8())
}

fn turn(s: &mut Session, item: ItemId, degrees: u32) -> Value {
    s.execute("clip.interpretFootage", json!({"items": [item.0], "rotation": degrees})).unwrap()
}

/// A second sequence, standing (H x W), with the lying clip turned upright by hand: Motion
/// rotation -90, as a clip is fixed without a source rotation. Made active. Returns its clip.
fn standing_sequence_fixed_by_hand(s: &mut Session, item: ItemId) -> ClipId {
    let mut p = (*s.project).clone();
    let rate = p.item(item).unwrap().as_media().unwrap().frame_rate();
    let seq = p.new_sequence("Reel", SequenceSettings { width: H, height: W, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let d = p.item(item).unwrap().duration();
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, d), rate).unwrap();
    for e in &mut ti.effects {
        filmcraft_project::resolve_auto_points(e, (H, W), (W, H));
    }
    ti.effect_mut("motion").unwrap().params.get_mut("rotation").unwrap().value = ParamValue::Float(-90.0);
    // a grade through a mask and a crop, both placed in the lying picture's own pixels, well off its centre:
    // one corner region made much darker, and a strip taken off the lying picture's left edge
    let mut dark = filmcraft_project::find_effect("brightness_contrast").unwrap().instance();
    dark.params.get_mut("brightness").unwrap().value = ParamValue::Float(-80.0);
    let corner = filmcraft_project::MaskPath::ellipse(
        filmcraft_geom::Vec2::new(f64::from(W) * 0.25, f64::from(H) * 0.3),
        filmcraft_geom::Vec2::new(f64::from(W) * 0.2, f64::from(H) * 0.2),
    );
    dark.masks.push(filmcraft_project::Mask::new("corner", corner));
    ti.effects.push(dark);
    let mut crop = filmcraft_project::find_effect("crop").unwrap().instance();
    crop.params.get_mut("left").unwrap().value = ParamValue::Float(15.0);
    crop.params.get_mut("bottom").unwrap().value = ParamValue::Float(5.0);
    ti.effects.push(crop);
    let id = ti.id;
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences.push(seq);
    id
}

fn motion(s: &Session, clip: ClipId) -> (f64, filmcraft_geom::Vec2) {
    let it = s.project.sequences().filter_map(|i| i.as_sequence()).find_map(|q| q.find_item(clip)).unwrap().1;
    let m = it.effect("motion").unwrap();
    let rot = match m.params["rotation"].value {
        ParamValue::Float(v) => v,
        ref v => panic!("{v:?}"),
    };
    let anchor = match m.params["anchor"].value {
        ParamValue::Vec2(v) => v,
        ref v => panic!("{v:?}"),
    };
    (rot, anchor)
}

#[test]
fn a_turned_clip_is_upright_wherever_it_is_read_and_undo_lays_it_down_again() {
    let root = tmp_dir("turn-source");
    let a = root.join("lying.mov");
    make_movie(&a, DemoScene::OceanSunset, W, H, 6);
    let (mut s, items, _) = session_with(&[&a]);
    let item = items[0];
    let lying = stored_frame_turned(&s, &a, 0);
    assert_eq!((lying.0, lying.1), (W, H));
    assert_eq!(frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap()), lying, "as stored before any rotation");
    let key = crate::media_pool::media_key(s.project.item(item).unwrap().as_media().unwrap());

    let r = turn(&mut s, item, 270);
    assert_eq!(r["rotation"], json!(270), "{r}");
    assert_eq!((rotation(&s, item), size(&s, item)), (3, (H, W)), "the item reports the standing size");
    let m = s.project.item(item).unwrap().as_media().unwrap().clone();
    assert_eq!(m.file_info().video.map(|v| (v.width, v.height)), Some((W, H)), "the file's own size is still known");
    assert_ne!(crate::media_pool::media_key(&m), key, "thumbnails and caches keyed by the media key are made again");

    // what the Source monitor and the thumbnails read (the pool's source), the full-resolution source an export
    // reads, and a background job's own copy of the file: all the stored picture turned three quarter turns
    let upright = stored_frame_turned(&s, &a, 3);
    assert_eq!((upright.0, upright.1), (H, W));
    assert_ne!(upright.2, lying.2);
    let shown = s.media.source_for(&s.project, item, &*s.services).unwrap();
    assert_eq!(shown.info().video.as_ref().map(|v| (v.width, v.height)), Some((H, W)));
    assert_eq!(frame(&*shown), upright, "monitor / thumbnail source");
    assert_eq!(frame(&*s.media.full_res_source(&s.project, item, &*s.services).unwrap()), upright, "full-resolution source");
    assert_eq!(frame(&*s.media.own_source(&s.project, item, &*s.services).unwrap()), upright, "a background job's own source");
    // sound is untouched
    assert_eq!(shown.audio(0, 64, 48_000).unwrap().frames(), 64);

    // the other turns, and back to none
    for (deg, turns, sz) in [(90u32, 1u8, (H, W)), (180, 2, (W, H)), (0, 0, (W, H))] {
        turn(&mut s, item, deg);
        assert_eq!((rotation(&s, item), size(&s, item)), (turns, sz), "{deg}");
        assert_eq!(frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap()), stored_frame_turned(&s, &a, turns), "{deg}");
    }
    // one undo step each
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((rotation(&s, item), size(&s, item)), (2, (W, H)));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((rotation(&s, item), size(&s, item)), (3, (H, W)));
    assert_eq!(frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap()), upright);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((rotation(&s, item), size(&s, item)), (0, (W, H)));
    assert_eq!(frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap()), lying);

    // values that are not a quarter turn change nothing
    for bad in [json!(45), json!(360), json!(-90), json!("90"), json!(90.5)] {
        assert!(s.execute("clip.interpretFootage", json!({"items": [item.0], "rotation": bad})).is_err(), "{bad}");
    }
    assert_eq!(rotation(&s, item), 0);
    // a generated clip has no stored picture to turn
    let matte = s.execute("file.newColorMatte", json!({"color": "#ff0000", "width": 64, "height": 36, "seconds": 1.0})).unwrap()["item"].as_u64().unwrap();
    assert!(s.execute("clip.interpretFootage", json!({"items": [matte], "rotation": 90})).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_project_saved_with_a_rotation_opens_with_it_and_older_projects_have_none() {
    let root = tmp_dir("turn-save");
    let a = root.join("lying.mov");
    make_movie(&a, DemoScene::Dunes, W, H, 4);
    let (mut s, items, _) = session_with(&[&a]);
    let plain = serde_json::to_value(&*s.project).unwrap().to_string();
    assert!(!plain.contains("\"rotation\":3") && !plain.contains("\"rotation\": 3"));
    let item = s.project.item(items[0]).unwrap().clone();
    let v = serde_json::to_value(&item).unwrap();
    assert!(
        v.to_string().contains("interpret") && v.pointer("/kind/Media/interpret/rotation").is_none(),
        "no rotation is written for a clip that has none: {v}"
    );
    // a project from before the option (no such field) reads as no rotation
    let old: filmcraft_project::ProjectItem = serde_json::from_value(v).unwrap();
    assert_eq!(old.as_media().unwrap().rotation(), 0);
    turn(&mut s, items[0], 270);
    let v = serde_json::to_value(s.project.item(items[0]).unwrap()).unwrap();
    assert_eq!(v.pointer("/kind/Media/interpret/rotation"), Some(&json!(3)), "{v}");
    let back: filmcraft_project::ProjectItem = serde_json::from_value(v).unwrap();
    assert_eq!((back.as_media().unwrap().rotation(), back.as_media().unwrap().info.video.as_ref().map(|v| (v.width, v.height))), (3, Some((H, W))));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_clip_stood_upright_by_hand_in_a_sequence_keeps_its_look_and_others_turn_in_place() {
    let root = tmp_dir("turn-fold");
    let a = root.join("lying.mov");
    make_movie(&a, DemoScene::OceanSunset, W, H, 6);
    // `session_with` puts the clip, untouched, in a lying (W x H) sequence: it shows on its side there
    let (mut s, items, lying_seq) = session_with(&[&a]);
    let item = items[0];
    let untouched = s.project.sequence(lying_seq).unwrap().video_tracks[0].items[0].id;
    let lying_before = frame_rgba(&mut s, 2, 1.0);
    let by_hand = standing_sequence_fixed_by_hand(&mut s, item);
    assert_eq!(motion(&s, by_hand), (-90.0, filmcraft_geom::Vec2::new(f64::from(W) / 2.0, f64::from(H) / 2.0)));
    let before = frame_rgba(&mut s, 2, 1.0);
    assert_eq!((before.0, before.1), (H as usize, W as usize));

    let r = turn(&mut s, item, 270);
    assert_eq!((r["sequenceClipsFolded"].clone(), r["sequenceClipsTurned"].clone()), (json!(1), json!(1)), "{r}");
    // the hand-made turn moved into the clip: no Motion rotation left, the anchor is the centre of the standing picture
    assert_eq!(motion(&s, by_hand), (0.0, filmcraft_geom::Vec2::new(f64::from(H) / 2.0, f64::from(W) / 2.0)));
    let after = frame_rgba(&mut s, 2, 1.0);
    let q = psnr(&after.2, &before.2);
    assert!(q > 40.0, "the clip fixed by hand looks as it did (picture, masked grade and crop), not turned twice: {q:.1} dB");
    // the mask and the crop are doing something (without them the frame is another picture), and they moved with the picture
    let it = s.project.sequences().filter_map(|i| i.as_sequence()).find_map(|q| q.find_item(by_hand)).unwrap().1.clone();
    let crop = it.effect("crop").unwrap();
    let side = |k: &str| match crop.params[k].value {
        ParamValue::Float(v) => v,
        ref v => panic!("{v:?}"),
    };
    // three quarter turns clockwise: the lying picture's left edge is the standing picture's bottom edge, its bottom the right
    assert_eq!((side("bottom"), side("right"), side("left"), side("top")), (15.0, 5.0, 0.0, 0.0));
    let src = frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap());
    assert!(psnr(&after.2, &src.2) < 30.0, "the masked grade and the crop still show");

    // the clip nobody had fixed turns with the footage, in place (about its anchor, the picture's centre)
    assert_eq!(motion(&s, untouched), (0.0, filmcraft_geom::Vec2::new(f64::from(H) / 2.0, f64::from(W) / 2.0)));
    s.state.active_sequence = Some(lying_seq);
    let lying_after = frame_rgba(&mut s, 2, 1.0);
    assert!(psnr(&lying_after.2, &lying_before.2) < 25.0, "it no longer shows on its side");
    let img = s.render_program(1.0).unwrap();
    let alpha = |x: usize| img.get(x, img.h / 2)[3];
    assert!(alpha(2) < 0.01 && alpha(img.w - 3) < 0.01 && alpha(img.w / 2) > 0.99, "the standing picture is centred in the lying frame");

    // one undo step brings back the clip's rotation and both sequences
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(rotation(&s, item), 0);
    assert_eq!(motion(&s, by_hand).0, -90.0);
    assert_eq!(frame_rgba(&mut s, 2, 1.0), lying_before);

    // asked not to fold, the hand-made rotation stays: the clip turns with the footage like any other
    let r = s.execute("clip.interpretFootage", json!({"items": [item.0], "rotation": 270, "foldSequenceRotation": false})).unwrap();
    assert_eq!((r["sequenceClipsFolded"].clone(), r["sequenceClipsTurned"].clone()), (json!(0), json!(2)), "{r}");
    assert_eq!(motion(&s, by_hand).0, -90.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rotation_that_is_not_the_turn_or_is_animated_is_left_alone() {
    let root = tmp_dir("turn-nofold");
    let a = root.join("lying.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 4);
    let (mut s, items, _) = session_with(&[&a]);
    let clip = standing_sequence_fixed_by_hand(&mut s, items[0]);
    // turned the other way by hand: not the turn being set, so it is not taken out
    let r = turn(&mut s, items[0], 90);
    assert_eq!((r["sequenceClipsFolded"].clone(), motion(&s, clip).0), (json!(0), -90.0), "{r}");
    // 270 more is the same picture as -90 by hand was fixing: 90 + 180 = 270 in all, but the step now is 180
    let r = turn(&mut s, items[0], 270);
    assert_eq!((r["sequenceClipsFolded"].clone(), motion(&s, clip).0), (json!(0), -90.0), "{r}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_proxy_turns_with_its_clip_and_a_proxy_made_from_it_is_stored_like_the_clip() {
    let root = tmp_dir("turn-proxy");
    let a = root.join("lying.mov");
    make_movie(&a, DemoScene::OceanSunset, W, H, 6);
    // the camera's own proxy: the same picture, smaller, stored on its side as well
    let cam = root.join("cam-proxy.mov");
    make_movie(&cam, DemoScene::OceanSunset, W / 2, H / 2, 6);
    let (mut s, items, _) = session_with(&[&a]);
    let item = items[0];
    turn(&mut s, item, 270);
    // attaching checks the proxy against the picture as stored: a lying proxy fits a turned clip
    s.execute("media.attachProxies", json!({"item": item.0, "path": cam.to_string_lossy()})).unwrap();
    s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    let shown = s.media.source_for(&s.project, item, &*s.services).unwrap();
    assert_eq!(shown.info().video.as_ref().map(|v| (v.width, v.height)), Some((H, W)), "a proxy reports its clip's size");
    assert_eq!(frame(&*shown), stored_frame_turned(&s, &cam, 3), "the proxy's own frames, turned like the clip");
    assert_eq!(frame(&*s.media.full_res_source(&s.project, item, &*s.services).unwrap()), stored_frame_turned(&s, &a, 3), "export reads the clip");
    // changing the rotation turns the proxy too (its cached source is not reused)
    turn(&mut s, item, 90);
    assert_eq!(frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap()), stored_frame_turned(&s, &cam, 1));
    turn(&mut s, item, 270);

    // a proxy made from the turned clip is written as the clip is stored (lying), so the same rotation stands it up
    s.execute("media.toggleProxies", json!({"enabled": false})).unwrap();
    s.execute("media.detachProxies", json!({"items": [item.0]})).unwrap();
    let r = s.execute("media.createProxies", json!({"items": [item.0], "preset": "prores_proxy_half", "wait": true})).unwrap();
    let out = r["outputs"][0]["path"].as_str().unwrap().to_string();
    let made = s.media.open_file(&out, &*s.services).unwrap();
    assert_eq!(made.info().video.as_ref().map(|v| (v.width, v.height)), Some((W / 2, H / 2)), "stored lying, like the clip's own file");
    let full = frame(&*s.media.full_res_source(&s.project, item, &*s.services).unwrap());
    s.execute("media.toggleProxies", json!({"enabled": true})).unwrap();
    let via_proxy = s.media.source_for(&s.project, item, &*s.services).unwrap();
    let px = frame(&*via_proxy);
    assert_eq!((px.0, px.1), (H / 2, W / 2), "upright, at the proxy's size");
    // the same picture as the clip's, at half size: compare with the clip's upright frame box-filtered 2 x 2
    let half: Vec<u8> = (0..(W / 2) as usize)
        .flat_map(|y| (0..(H / 2) as usize).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let at = |dx: usize, dy: usize, c: usize| u32::from(full.2[((y * 2 + dy) * H as usize + x * 2 + dx) * 4 + c]);
            (0..4).map(move |c| ((at(0, 0, c) + at(1, 0, c) + at(0, 1, c) + at(1, 1, c)) / 4) as u8).collect::<Vec<u8>>()
        })
        .collect();
    let q = psnr(&px.2, &half);
    // ... and not the same picture upside down (what a proxy turned twice, or the wrong way, would show)
    let upside_down: Vec<u8> = half.as_chunks::<4>().0.iter().rev().flatten().copied().collect();
    let wrong = psnr(&px.2, &upside_down);
    eprintln!("made proxy vs clip: {q:.1} dB (upside down: {wrong:.1} dB)");
    assert!(q > 22.0 && q > wrong + 6.0, "the made proxy shows the clip's upright picture: {q:.1} dB (upside down: {wrong:.1} dB)");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn import_rules_turn_new_imports_and_leave_clips_already_in_the_project_alone_unless_asked() {
    let root = tmp_dir("turn-rules");
    let a = root.join("a.mov");
    let b = root.join("b.mov");
    make_movie(&a, DemoScene::Dunes, W, H, 4);
    make_movie(&b, DemoScene::Dunes, W, H, 4);
    let mut s = Session::default();
    let import = |s: &mut Session, p: &Path| ItemId(s.execute("file.import", json!({"paths": [p.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap());
    // imported before the rule: as stored
    let old = import(&mut s, &a);
    assert_eq!((rotation(&s, old), size(&s, old)), (0, (W, H)));

    // a rotation must be a quarter turn; a rule may turn without naming a colour space, but must do something
    for bad in [
        json!([{"extensions": ["mov"], "colorSpace": "rec709", "rotation": 45}]),
        json!([{"extensions": ["mov"]}]),
        json!([{"extensions": ["mov"], "colorSpace": "nope", "rotation": 90}]),
    ] {
        assert!(s.execute("import.setRules", json!({"rules": bad})).is_err(), "{bad}");
    }
    s.execute("import.setRules", json!({"rules": [{"name": "sideways camera", "extensions": ["mov"], "rotation": 270}]})).unwrap();
    assert_eq!(s.execute("import.rules", json!({})).unwrap()["rules"][0]["rotation"], json!(270));

    // a new import is turned in its own undo step
    let new = import(&mut s, &b);
    assert_eq!((rotation(&s, new), size(&s, new)), (3, (H, W)));
    assert_eq!(s.project.item(new).unwrap().as_media().unwrap().interpret.color_space, None, "this rule names no colour space");
    assert_eq!(frame(&*s.media.source_for(&s.project, new, &*s.services).unwrap()), stored_frame_turned(&s, &b, 3));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(new).is_none(), "undo takes back the import, rotation included");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(rotation(&s, new), 3);

    // applying the rules to the project does not turn clips that are already there...
    let r = s.execute("project.applyInterpretRules", json!({})).unwrap();
    assert_eq!((r["rotated"].clone(), rotation(&s, old)), (json!([]), 0), "{r}");
    // ...unless asked to
    let r = s.execute("project.applyInterpretRules", json!({"rotation": true})).unwrap();
    assert_eq!(r["rotated"], json!([{"item": old.0, "rotation": 270}]), "{r}");
    assert_eq!((rotation(&s, old), size(&s, old)), (3, (H, W)));

    // colour space and rotation from one rule; a rule without a rotation writes none (rule files from before stay as they were)
    s.execute(
        "import.setRules",
        json!({"rules": [{"extensions": ["mov"], "colorSpace": "nlog-bt2020", "rotation": 90}, {"extensions": ["mp4"], "colorSpace": "rec709"}]}),
    )
    .unwrap();
    let rules = s.execute("import.rules", json!({})).unwrap();
    assert!(rules["rules"][1].get("rotation").is_none(), "{rules}");
    let c = root.join("c.mov");
    make_movie(&c, DemoScene::Dunes, W, H, 4);
    let both = import(&mut s, &c);
    assert_eq!(
        (rotation(&s, both), s.project.item(both).unwrap().as_media().unwrap().interpret.color_space),
        (1, Some(filmcraft_color::ColorSpace::NLogBt2020))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_turned_clip_relinks_to_a_copy_of_its_file_and_exports_upright() {
    let root = tmp_dir("turn-relink");
    let a = root.join("lying.mov");
    make_movie(&a, DemoScene::OceanSunset, W, H, 6);
    let (mut s, items, _) = session_with(&[&a]);
    let item = items[0];
    turn(&mut s, item, 270);
    // the copy is compared with the picture as stored: no "video differs", and the item stays upright
    let moved = root.join("moved");
    std::fs::create_dir_all(&moved).unwrap();
    std::fs::copy(&a, moved.join("lying.mov")).unwrap();
    s.execute("media.relink", json!({"item": item.0, "path": moved.join("lying.mov").to_string_lossy()})).unwrap();
    assert_eq!((rotation(&s, item), size(&s, item)), (3, (H, W)));
    assert_eq!(frame(&*s.media.source_for(&s.project, item, &*s.services).unwrap()), stored_frame_turned(&s, &a, 3));

    // a sequence made from the turned clip stands, and its export is the upright picture
    let r = s.execute("file.newSequenceFromClip", json!({"items": [item.0]})).unwrap();
    let seq = ItemId(r["sequence"].as_u64().unwrap());
    let st = s.project.sequence(seq).unwrap().settings.clone();
    assert_eq!((st.width, st.height), (H, W));
    s.state.active_sequence = Some(seq);
    let shown = frame_rgba(&mut s, 2, 1.0);
    let out = root.join("out.mov");
    s.execute("file.exportMedia", json!({"sequence": seq.0, "path": out.to_string_lossy(), "format": "prores", "wait": true})).unwrap();
    let exported = stored_frame_turned(&s, &out, 0);
    assert_eq!((exported.0, exported.1), (H, W), "the export stands");
    let q = psnr(&exported.2, &shown.2);
    let upside_down: Vec<u8> = shown.2.as_chunks::<4>().0.iter().rev().flatten().copied().collect();
    let wrong = psnr(&exported.2, &upside_down);
    eprintln!("export vs program: {q:.1} dB (upside down: {wrong:.1} dB)");
    assert!(q > 30.0 && q > wrong + 10.0, "the export is what the Program monitor shows: {q:.1} dB (upside down: {wrong:.1} dB)");
    let _ = std::fs::remove_dir_all(&root);
}
