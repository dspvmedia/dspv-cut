//! Scene Grade: white balance, monotone zones, colour constancy, and the clip-level colour
//! management that goes with it (identity on every log space, no double tone map, the Source
//! override, the GPU plan). Synthetic frames only.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, ColorPipeline, ColorSpace, InputTransform, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{AudioBuffer, Chroma, PixelData, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource};
use filmcraft_project::{EffectInstance, ItemId, ItemKind, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use super::*;
use crate::plan::{FramePlan, plan_frame};
use crate::{Image, RenderOptions, SourceMap, render_sequence};

const FPS: FrameRate = FrameRate::FPS_24;

fn neutral_cx(peak: Option<f32>) -> FxCtx<'static> {
    FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
        scene_peak_nits: peak,
    }
}

fn grade(params: &[(&str, f64)]) -> EffectInstance {
    let mut e = find_effect(ID).expect("scene_grade is registered").instance();
    for (k, v) in params {
        e.params.get_mut(*k).unwrap_or_else(|| panic!("{k}")).value = ParamValue::Float(*v);
    }
    e
}

/// A small deterministic generator for the property tests.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// A random but valid setting of every parameter, extreme zone exposures included.
fn random_grade(r: &mut Rng) -> EffectInstance {
    let mut p: Vec<(&str, f64)> = vec![
        ("kelvin", r.range(1700.0, 20_000.0)),
        ("tint", r.range(-100.0, 100.0)),
        ("exposure", r.range(-4.0, 4.0)),
        ("gain_r", r.range(0.3, 3.0)),
        ("gain_g", r.range(0.3, 3.0)),
        ("gain_b", r.range(0.3, 3.0)),
        ("contrast", r.range(0.1, 4.0)),
        ("pivot", r.range(0.02, 1.0)),
    ];
    for z in ["dark", "shadow", "highlight"] {
        let k = |s: &str| Box::leak(format!("{z}_{s}").into_boxed_str()) as &str;
        p.push((k("center"), r.range(-8.0, 8.0)));
        p.push((k("width"), r.range(0.1, 6.0)));
        p.push((k("exposure"), r.range(-6.0, 6.0)));
    }
    grade(&p)
}

// ---- white balance

#[test]
fn kelvin_follows_the_planckian_locus() {
    // A (2856 K) and 5000 K against the published chromaticities
    for (k, x, y) in [(2856.0, 0.4476, 0.4074), (5000.0, 0.3451, 0.3516), (3200.0, 0.4234, 0.3990)] {
        let [px, py] = planck_xy(k);
        assert!((px - x).abs() < 3e-3 && (py - y).abs() < 3e-3, "{k} K: ({px:.4}, {py:.4}) vs ({x}, {y})");
    }
    // the locus is continuous where its spline pieces meet
    for k in [2222.0, 4000.0] {
        let (a, b) = (planck_xy(k - 0.01), planck_xy(k + 0.01));
        assert!((a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4, "{k}");
    }
    // 6500 K is the working white exactly; a positive Tint moves towards green, a negative one away
    let d65 = filmcraft_color::spaces::D65;
    let w = scene_white_xy(6500.0, 0.0, d65);
    assert!((w[0] - d65[0]).abs() < 1e-12 && (w[1] - d65[1]).abs() < 1e-12);
    let (g, m) = (scene_white_xy(5000.0, 60.0, d65), scene_white_xy(5000.0, -60.0, d65));
    let on = scene_white_xy(5000.0, 0.0, d65);
    assert!(g[1] > on[1] && m[1] < on[1], "tint: {g:?} {on:?} {m:?}");
}

#[test]
fn white_balance_is_the_identity_at_6500_and_neutralises_the_scene_white() {
    assert_eq!(wb_matrix(6500.0, 0.0), [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    let to_xyz = Gamut::Bt709.to_xyz();
    let to_rgb = inverse(&to_xyz);
    for (k, tint) in [(2700.0, 0.0), (3200.0, 25.0), (4500.0, -40.0), (9000.0, 10.0), (12_000.0, 0.0)] {
        let [x, y] = scene_white_xy(
            k,
            tint,
            [to_xyz[0].iter().sum::<f64>() / to_xyz.iter().flatten().sum::<f64>(), to_xyz[1].iter().sum::<f64>() / to_xyz.iter().flatten().sum::<f64>()],
        );
        // the scene's white, as linear Rec. 709, comes out grey
        let white = mul_vec(&to_rgb, [x / y, 1.0, (1.0 - x - y) / y]);
        let out = mul_vec(&wb_matrix(k, tint), white);
        let mean = (out[0] + out[1] + out[2]) / 3.0;
        for c in out {
            assert!((c / mean - 1.0).abs() < 1e-3, "{k} K tint {tint}: {out:?}");
        }
        assert!((mean - 1.0).abs() < 0.15, "luminance roughly kept: {mean}");
    }
    // a warm light is corrected by cooling the picture: red down, blue up
    let tungsten = wb_matrix(2700.0, 0.0);
    assert!(tungsten[0][0] < 1.0 && tungsten[2][2] > 1.0, "{tungsten:?}");
}

// ---- monotone zones

/// The grade's curve on log2 luminance, as `grade_pixel` evaluates it.
fn curve(p: &SceneParams, s: f32) -> f32 {
    let mut s2 = p.pivot_stops + (s - p.pivot_stops) * p.contrast;
    for z in &p.zones {
        if z.exposure != 0.0 {
            let u = ((s - z.center) / z.width).clamp(-1.0, 1.0);
            s2 += z.exposure * 0.5 * (1.0 + (std::f32::consts::PI * u).cos());
        }
    }
    s2
}

#[test]
fn the_curve_is_monotone_for_any_setting() {
    let mut r = Rng(7);
    for case in 0..400 {
        let e = random_grade(&mut r);
        let p = eval(&e, &neutral_cx(None));
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=4000 {
            let s = -16.0 + 32.0 * i as f32 / 4000.0;
            let v = curve(&p, s);
            assert!(v >= prev - 1e-4, "case {case}: not monotone at {s} stops ({prev} -> {v}), params {p:?}");
            prev = v;
        }
    }
    // extreme exposures are limited, not rejected: a lone zone at 6 stops of exposure on a flat curve
    let e = grade(&[("contrast", 1.0), ("shadow_exposure", 6.0), ("shadow_width", 0.5), ("dark_exposure", -6.0), ("dark_width", 0.2)]);
    let p = eval(&e, &neutral_cx(None));
    for z in &p.zones {
        assert!(z.exposure.abs() <= 0.9 * 2.0 * z.width / std::f32::consts::PI + 1e-6, "{z:?}");
    }
}

#[test]
fn a_zone_without_width_is_off() {
    for width in [0.0, -1.0] {
        let e = grade(&[("highlight_width", width), ("highlight_exposure", 3.0), ("contrast", 1.0)]);
        let p = eval(&e, &neutral_cx(None));
        assert!(p.is_finite(), "{width}: {p:?}");
        assert_eq!(p.zones[2].exposure, 0.0, "{width}");
        assert!(!p.luminance_stage(), "{width}");
    }
    // a width that is not a number is the default width, never a division by it
    let e = grade(&[("highlight_width", f64::NAN), ("highlight_exposure", 3.0)]);
    assert!(eval(&e, &neutral_cx(None)).is_finite());
}

#[test]
fn luminance_is_monotone_along_a_grey_ramp() {
    let tone = filmcraft_color::shared_tone_map(3000.0);
    let mut r = Rng(11);
    for case in 0..200 {
        let mut e = random_grade(&mut r);
        e.params.get_mut("white").unwrap().value = ParamValue::Float(r.range(0.5, 1.2));
        e.params.get_mut("desat").unwrap().value = ParamValue::Float(r.range(0.0, 1.0));
        for peak in [None, Some(3000.0f32)] {
            let p = eval(&e, &neutral_cx(peak));
            let mut prev = -1.0f32;
            for i in 0..=2000 {
                // a grey ramp over 20 stops, from 1e-4 to above the tone map's source peak
                let v = 1e-4 * (1.0e5f32).powf(i as f32 / 2000.0);
                let out = grade_pixel([v; 3], &p, peak.map(|_| &*tone));
                let y = filmcraft_color::luma709(out[0], out[1], out[2]);
                // before the tone map the grade is monotone to rounding; the tone map's own 4096-entry table
                // wobbles by about 1e-4 of its value near the peak (`the_tone_map_table_ripples_by_itself`)
                let slack = if peak.is_some() { 1e-3 } else { 1e-5 };
                assert!(y >= prev - slack * prev.abs().max(1.0), "case {case} peak {peak:?}: luminance falls at input {v} ({prev} -> {y}), params {p:?}");
                prev = y;
            }
        }
    }
}

/// The ripple the monotone test allows for is the engine's tone map table, which has it without any grade: a
/// grey ramp through `ToneMap::apply` alone.
#[test]
fn the_tone_map_table_ripples_by_itself() {
    let tone = filmcraft_color::shared_tone_map(3000.0);
    let mut prev = 0.0f32;
    let mut worst = 0.0f32;
    for i in 0..=20_000 {
        let v = 1e-4 * (1.5e5f32).powf(i as f32 / 20_000.0);
        let y = tone.apply([v; 3])[0];
        worst = worst.max((prev - y) / prev.max(1e-6));
        prev = y;
    }
    assert!(worst < 5e-4, "the table ripples by {worst:.2e}");
}

#[test]
fn one_chromaticity_stays_one_below_the_knee() {
    let tone = filmcraft_color::shared_tone_map(3000.0);
    let xy = |c: [f32; 3]| {
        let s = c[0] + c[1] + c[2];
        (c[0] / s, c[1] / s)
    };
    let mut r = Rng(23);
    let mut measured = 0;
    for case in 0..200 {
        let e = random_grade(&mut r);
        let p = eval(&e, &neutral_cx(Some(3000.0)));
        // a ramp of one chromaticity (a warm, a cool and a green one in turn)
        let chroma = [[1.0, 0.6, 0.3], [0.4, 0.7, 1.0], [0.5, 1.0, 0.4]][case % 3];
        let mut first = None;
        for i in 0..=400 {
            let v = 1e-5 * (1.0e6f32).powf(i as f32 / 400.0);
            let out = grade_pixel(chroma.map(|c| c * v), &p, Some(&tone));
            // the tone map is the identity up to its knee: the ratio on max(R,G,B) is 1 there
            let before = grade_pixel(chroma.map(|c| c * v), &SceneParams { tone_peak_nits: None, ..p }, None);
            let max_nits = before[0].max(before[1]).max(before[2]) * 203.0;
            // (BT.2390 with a 3000 cd/m² source starts its knee at about 90 cd/m²)
            if max_nits > 80.0 || out.iter().sum::<f32>().abs() < 1e-9 {
                continue;
            }
            let (x, y) = xy(out);
            let (x0, y0) = *first.get_or_insert((x, y));
            assert!((x - x0).abs() < 2e-3 && (y - y0).abs() < 2e-3, "case {case}: chromaticity moved at input {v}: ({x}, {y}) vs ({x0}, {y0}), params {p:?}");
        }
        measured += first.is_some() as usize;
    }
    // (a very flat contrast lifts the whole ramp above the knee: those cases have nothing to measure)
    assert!(measured >= 120, "only {measured} of 200 settings had a ramp below the knee");
}

// ---- the DSPV interior preset

#[test]
fn the_dspv_interior_preset_is_valid_and_monotone() {
    let list = presets();
    assert_eq!(list.len(), 1);
    assert_eq!(find_preset(" dspv INTERIOR "), Some(&DSPV_INTERIOR));
    assert!(find_preset("nope").is_none());
    let e = DSPV_INTERIOR.instance().expect("effect");
    let def = find_effect(ID).expect("effect");
    for (id, v) in DSPV_INTERIOR.params {
        assert_eq!(e.params[*id].value, ParamValue::Float(*v), "{id}");
        assert!(def.param(id).is_some_and(|d| matches!(d.kind, filmcraft_project::ParamKind::Float { min, max, .. } if (min..=max).contains(v))), "{id}");
    }
    // what belongs to the clip is left alone
    for id in ["kelvin", "tint", "exposure", "gain_r", "gain_g", "gain_b", "pivot"] {
        assert_eq!(e.params[id].value, def.param(id).expect("param").default, "{id}");
    }
    let p = eval(&e, &neutral_cx(Some(3000.0)));
    // Step 0's zones come through as set: the monotone limit does not touch them
    assert_eq!(p.zones.map(|z| z.exposure), [-1.0, 1.5, -1.0]);
    assert!(p.luminance_stage());
    let mut prev = f32::NEG_INFINITY;
    for i in 0..=4000 {
        let v = curve(&p, -16.0 + 32.0 * i as f32 / 4000.0);
        assert!(v >= prev, "preset curve falls");
        prev = v;
    }
    // applied to an existing Scene Grade it keeps the clip's own settings
    let mut mine = grade(&[("kelvin", 3200.0), ("exposure", -0.5)]);
    DSPV_INTERIOR.apply_to(&mut mine);
    assert_eq!(mine.params["kelvin"].value, ParamValue::Float(3200.0));
    assert_eq!(mine.params["contrast"].value, ParamValue::Float(1.45));
}

#[test]
fn defaults_are_neutral() {
    let p = eval(&find_effect(ID).expect("effect").instance(), &neutral_cx(None));
    assert_eq!(p.matrix, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    assert!(!p.luminance_stage());
    assert_eq!((p.contrast, p.white, p.desat), (1.0, 1.0, 0.0));
    // nothing changes a pixel (no tone map: the clip is not tone mapped)
    for c in [[0.0, 0.0, 0.0], [0.18; 3], [0.5, 0.2, 0.9], [3.0, 1.0, 0.1]] {
        assert_eq!(grade_pixel(c, &p, None), c);
    }
}

/// Highlight desaturation follows the shoulder only: a colour below the knee keeps its saturation
/// whatever the White Level (the press is measured on the roll-off, before the white level), and a
/// colour the shoulder compresses moves towards its luma.
#[test]
fn highlight_desaturation_leaves_colours_below_the_knee_alone() {
    let peak = 3000.0;
    let tone = filmcraft_color::shared_tone_map(f64::from(peak));
    let p = eval(&grade(&[("desat", 1.0), ("white", 0.92)]), &neutral_cx(Some(peak)));
    // a saturated mid tone, well under the knee: the roll-off times the white level (to the
    // table's own ripple), not pulled towards grey by desat · (1 − white)
    let mid = [0.20, 0.10, 0.03];
    let rolled = tone.apply(mid);
    let got = grade_pixel(mid, &p, Some(&tone));
    for k in 0..3 {
        assert!((got[k] - rolled[k] * 0.92).abs() <= 5e-4 * rolled[0], "{got:?} vs {rolled:?}");
    }
    // a bright orange the shoulder presses: less saturated than the plain roll-off
    let hot = [6.0, 3.0, 0.5];
    let plain = tone.apply(hot).map(|v| v * 0.92);
    let got = grade_pixel(hot, &p, Some(&tone));
    let spread = |c: [f32; 3]| (c[0] - c[2]) / c[0].max(1e-9);
    assert!(spread(got) < spread(plain) - 0.01, "{got:?} vs {plain:?}");
}

#[test]
fn hostile_parameters_give_a_finite_grade() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e30, -1e30] {
        let mut e = find_effect(ID).expect("effect").instance();
        for p in e.params.values_mut() {
            if matches!(p.value, ParamValue::Float(_)) {
                p.value = ParamValue::Float(bad);
            }
        }
        let p = eval(&e, &neutral_cx(Some(f32::NAN)));
        assert!(p.is_finite(), "{bad}: {p:?}");
        let out = grade_pixel([0.2, 0.4, 0.1], &p, None);
        assert!(out.iter().all(|v| v.is_finite()), "{bad}: {out:?}");
        // the lens falloff too, at every place in the frame
        let mut img = test_image(9, 5);
        grade_image(&mut img, &p);
        assert!(img.px.iter().all(|v| v.is_finite()), "{bad}: {:?}", p.falloff);
    }
}

// ---- lens falloff

/// A small picture of varied colours, scene-linear and premultiplied, with a half-transparent row.
fn test_image(w: usize, h: usize) -> Image {
    let mut r = Rng(7);
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for _ in 0..w {
            let a = if y == 1 { 0.5 } else { 1.0 };
            let v = 4.0f64.powf(r.range(-6.0, 1.5));
            px.extend([r.range(0.2, 1.0) * v * a, r.range(0.2, 1.0) * v * a, r.range(0.2, 1.0) * v * a, a].map(|c| c as f32));
        }
    }
    Image { w, h, px }
}

/// The falloff by its definition, in f64: `2^(stops · r^power)`, r from the centre of the pixel to the
/// optical centre (offset in fractions of the frame) over half the frame's diagonal.
fn reference_gain(stops: f64, power: f64, center: [f64; 2], (x, y): (usize, usize), (w, h): (usize, usize)) -> f64 {
    let (w, h) = (w as f64, h as f64);
    let dx = x as f64 + 0.5 - (0.5 + center[0]) * w;
    let dy = y as f64 + 0.5 - (0.5 + center[1]) * h;
    let r = dx.hypot(dy) / (0.5 * w.hypot(h));
    (stops * r.powf(power)).exp2()
}

/// Falloff Amount 0 is off whatever the other Lens settings: the picture is bit for bit the one of
/// the same grade without the Lens parameters (an instance saved before they existed).
#[test]
fn falloff_amount_zero_is_bit_identical() {
    let look = [("contrast", 1.45), ("shadow_exposure", 1.5), ("kelvin", 3100.0), ("white", 0.92), ("desat", 0.5)];
    let mut with = look.to_vec();
    with.extend([("falloff_amount", 0.0), ("falloff_power", 5.0), ("falloff_center_x", 20.0), ("falloff_center_y", -30.0)]);
    let mut without = grade(&look);
    for id in ["falloff_amount", "falloff_power", "falloff_center_x", "falloff_center_y"] {
        assert!(without.params.remove(id).is_some(), "{id}");
    }
    for peak in [None, Some(3000.0)] {
        let (a, b) = (eval(&grade(&with), &neutral_cx(peak)), eval(&without, &neutral_cx(peak)));
        assert!(!a.falloff.active() && b.falloff == Falloff::OFF, "{:?} {:?}", a.falloff, b.falloff);
        let (mut ia, mut ib) = (test_image(31, 17), test_image(31, 17));
        grade_image(&mut ia, &a);
        grade_image(&mut ib, &b);
        assert!(ia.px.iter().zip(&ib.px).all(|(x, y)| x.to_bits() == y.to_bits()), "{peak:?}");
    }
    // and a neutral grade leaves the picture exactly as it was
    let mut img = test_image(31, 17);
    let before = img.px.clone();
    grade_image(&mut img, &eval(&grade(&[]), &neutral_cx(None)));
    assert!(img.px.iter().zip(&before).all(|(x, y)| x.to_bits() == y.to_bits()));
}

/// An instance saved before the Lens parameters existed reads Power 2 (the definition's default)
/// once only Falloff Amount is set on it.
#[test]
fn an_old_instance_takes_falloff_amount_with_the_default_power() {
    let mut e = grade(&[]);
    for id in ["falloff_amount", "falloff_power", "falloff_center_x", "falloff_center_y"] {
        e.params.remove(id);
    }
    let mut amount = find_effect(ID).expect("effect").instance().params.remove("falloff_amount").expect("falloff_amount");
    amount.value = ParamValue::Float(1.0);
    e.params.insert("falloff_amount".into(), amount);
    let p = eval(&e, &neutral_cx(None));
    assert_eq!(p.falloff, Falloff { stops: 1.0, power: 2.0, center: [0.0, 0.0] });
}

/// The gain matches its definition: 1 at the optical centre, 2^stops at the corners of a centred
/// falloff (within half a pixel of radius), and 1 again under a moved centre.
#[test]
fn the_falloff_gain_follows_its_definition() {
    let (w, h) = (1921usize, 1081usize);
    let f = Falloff { stops: 1.5, power: 2.0, center: [0.0, 0.0] };
    let g = |f: &Falloff, x: usize, y: usize| f.gain(x as f32, y as f32, w as f32, h as f32);
    assert_eq!(g(&f, w / 2, h / 2), 1.0, "the centre pixel");
    for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        assert!((g(&f, x, y) / 1.5f32.exp2() - 1.0).abs() < 2e-3, "corner {x},{y}: {}", g(&f, x, y));
    }
    let moved = Falloff { center: [0.25, -0.1], power: 2.7, stops: -1.2 };
    // (0.75 w, 0.4 h) is the moved centre: 1440.75, 432.4, nearest pixel centre 1440, 432
    assert!((g(&moved, 1440, 432) - 1.0).abs() < 1e-5, "{}", g(&moved, 1440, 432));
    for (x, y) in [(0, 0), (77, 1000), (1900, 3), (960, 540), (1440, 1080)] {
        let want = reference_gain(-1.2, 2.7, [0.25, -0.1], (x, y), (w, h));
        let got = g(&moved, x, y) as f64;
        assert!((got / want - 1.0).abs() < 1e-5, "{x},{y}: {got} vs {want}");
    }
}

/// A flat field seen through the lens (multiplied by the inverse of the falloff) comes back flat,
/// with and without the roll-off and under a look, for centred and moved falloffs, on frames of any
/// shape; the correction is the same whatever the working image's resolution.
#[test]
fn a_flat_field_under_the_lens_comes_back_flat() {
    let lenses = [(1.0, 2.0, [0.0, 0.0]), (2.5, 2.0, [0.0, 0.0]), (1.7, 3.5, [0.06, -0.04]), (0.8, 1.2, [-0.2, 0.15])];
    for (w, h) in [(97usize, 61usize), (61, 97), (40, 40), (193, 121)] {
        for (stops, power, center) in lenses {
            for (look, peak) in [(vec![], None), (vec![("contrast", 1.45), ("shadow_exposure", 1.5), ("white", 0.92), ("desat", 0.5)], Some(3000.0))] {
                let mut params = look.clone();
                params.extend([
                    ("falloff_amount", stops),
                    ("falloff_power", power),
                    ("falloff_center_x", center[0] * 100.0),
                    ("falloff_center_y", center[1] * 100.0),
                ]);
                let p = eval(&grade(&params), &neutral_cx(peak));
                let flat = [0.18f64, 0.15, 0.11];
                let mut px = Vec::with_capacity(w * h * 4);
                for y in 0..h {
                    for x in 0..w {
                        let k = 1.0 / reference_gain(stops, power, center, (x, y), (w, h));
                        px.extend([flat[0] * k, flat[1] * k, flat[2] * k, 1.0].map(|c| c as f32));
                    }
                }
                let mut img = Image { w, h, px };
                grade_image(&mut img, &p);
                let first = [img.px[0], img.px[1], img.px[2]];
                let worst = img.px.chunks(4).flat_map(|q| (0..3).map(move |c| (q[c] / first[c] - 1.0).abs())).fold(0.0f32, f32::max);
                assert!(worst < 2e-5, "{w}x{h} {stops} {power} {center:?} {peak:?}: not flat by {worst:.2e}");
            }
        }
    }
}

/// The correction keeps hue (a pixel's R:G:B is unchanged, it is one gain per pixel), grows
/// monotonically from the centre to the corner for a positive amount and falls for a negative one,
/// and a brighter input stays brighter at every place.
#[test]
fn the_falloff_is_monotone_and_keeps_hue() {
    let (w, h) = (161usize, 91usize);
    for stops in [2.0f64, -2.0] {
        let p = eval(&grade(&[("falloff_amount", stops), ("falloff_power", 2.0)]), &neutral_cx(None));
        let src = test_image(w, h);
        let mut img = src.clone();
        grade_image(&mut img, &p);
        for (a, b) in src.px.chunks(4).zip(img.px.chunks(4)) {
            let k = [b[0] / a[0], b[1] / a[1], b[2] / a[2]];
            assert!((k[1] / k[0] - 1.0).abs() < 1e-5 && (k[2] / k[0] - 1.0).abs() < 1e-5, "hue moved: {a:?} → {b:?}");
        }
        // along the diagonal from the centre to the top-left corner
        let mut last = p.falloff.gain((w / 2) as f32, (h / 2) as f32, w as f32, h as f32);
        for i in 1..=80 {
            let (x, y) = ((w / 2) as f32 * (1.0 - i as f32 / 80.0), (h / 2) as f32 * (1.0 - i as f32 / 80.0));
            let g = p.falloff.gain(x.floor(), y.floor(), w as f32, h as f32);
            assert!(if stops > 0.0 { g >= last } else { g <= last }, "{stops}: step {i}: {last} then {g}");
            last = g;
        }
        assert!(if stops > 0.0 { last > 3.5 } else { last < 0.3 }, "{stops}: corner gain {last}");
        // brighter in, brighter out, at the corner
        let at = |v: f32| {
            let mut one = Image { w, h, px: [v, v, v, 1.0].repeat(w * h) };
            grade_image(&mut one, &p);
            one.px[0]
        };
        let ramp: Vec<f32> = (0..40).map(|i| at(1e-4 * 1.4f32.powi(i))).collect();
        assert!(ramp.windows(2).all(|q| q[1] > q[0]), "{stops}: {ramp:?}");
    }
}

/// Turning a frame a quarter turn turns a centred falloff with it: the radius is measured on the
/// upright picture the effect sees, over the half-diagonal, which a quarter turn keeps.
#[test]
fn a_centred_falloff_turns_with_the_frame() {
    let (w, h) = (64usize, 36usize);
    let f = Falloff { stops: 1.3, power: 2.4, center: [0.0, 0.0] };
    for y in 0..h {
        for x in 0..w {
            // clockwise: (x, y) of the w×h frame lands at (h - 1 - y, x) of the h×w one
            let (a, b) = (f.gain(x as f32, y as f32, w as f32, h as f32), f.gain((h - 1 - y) as f32, x as f32, h as f32, w as f32));
            assert_eq!(a.to_bits(), b.to_bits(), "{x},{y}");
        }
    }
}

// ---- the clip's colour management

struct Still {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
}

impl MediaSource for Still {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        Ok(self.frame.clone())
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, 2, frames))
    }
}

const W: usize = 128;
const H: usize = 6;

/// A 16-bit full-range ramp from below black to above white, under neutral and pushed chroma
/// (what an N-RAW decode delivers).
fn log_frame() -> Arc<VideoFrame> {
    let top = 65_535u32;
    let mid = 32_768i64;
    let off = (top / 3) as i64;
    let chroma = [(mid, mid), (mid, mid), (mid - off, mid + off), (mid + off, mid - off), (mid, mid + off), (mid, mid - off)];
    let y: Vec<u16> = (0..H).flat_map(|_| (0..W).map(|x| (x as u32 * top / (W as u32 - 1)) as u16)).collect();
    let cb: Vec<u16> = (0..H).flat_map(|r| std::iter::repeat_n(chroma[r].0 as u16, W)).collect();
    let cr: Vec<u16> = (0..H).flat_map(|r| std::iter::repeat_n(chroma[r].1 as u16, W)).collect();
    let color = ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Bt709, primaries: Primaries::Bt2020, range: Range::Full };
    Arc::new(VideoFrame {
        width: W as u32,
        height: H as u32,
        data: PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C444, bits: 16, alpha: None },
        color,
        par: (1, 1),
        pts: Tick::ZERO,
    })
}

/// An ordinary 8-bit Rec. 709 camera picture.
fn sdr_frame() -> Arc<VideoFrame> {
    let y: Vec<u8> = (0..W * H).map(|i| (16 + (i % W) * 219 / (W - 1)) as u8).collect();
    let u: Vec<u8> = (0..W * H).map(|i| (90 + i / W * 20) as u8).collect();
    let v: Vec<u8> = (0..W * H).map(|i| (170 - i / W * 20) as u8).collect();
    Arc::new(VideoFrame {
        width: W as u32,
        height: H as u32,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C444, alpha: None },
        color: ColorInfo::REC709,
        par: (1, 1),
        pts: Tick::ZERO,
    })
}

/// One clip of `frame`, interpreted as `interpret`, with `effect` on it.
fn rig(frame: Arc<VideoFrame>, interpret: Option<ColorSpace>, effect: Option<EffectInstance>) -> (Project, ItemId, SourceMap) {
    let mut p = Project::new("scene grade");
    let mut map = SourceMap::default();
    let seq = p.new_sequence("s", SequenceSettings { width: W as u32, height: H as u32, frame_rate: FPS, ..Default::default() }, 1, 1, None);
    let g = GeneratorSource::new(Generator::ColorMatte { color: [0.0, 0.0, 0.0, 1.0] }, frame.width, frame.height, FPS, Tick(10 * TICKS_PER_SECOND));
    let info = g.info().clone();
    let interpretation = filmcraft_project::Interpretation { color_space: interpret, ..Default::default() };
    let item = p.add_item(
        "clip",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
            interpret: interpretation,
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let mut clip = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(48)), FPS).expect("clip");
    clip.scale_to_frame = true;
    clip.effects.extend(effect);
    if let Some(t) = p.sequence_mut(seq).and_then(|s| s.video_tracks.get_mut(0)) {
        t.items.push(clip);
    }
    map.0.insert(item, Arc::new(Still { info, frame }));
    (p, seq, map)
}

fn render(p: &Project, seq: ItemId, map: &SourceMap) -> Image {
    render_sequence(p, seq, Tick::ZERO, RenderOptions::default(), map)
}

fn max_diff(a: &Image, b: &Image) -> f32 {
    assert_eq!((a.w, a.h, a.px.len()), (b.w, b.h, b.px.len()));
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn max_code_diff(a: &Image, b: &Image) -> u8 {
    a.over_black_rgba8().iter().zip(b.over_black_rgba8()).map(|(x, y)| x.abs_diff(y)).max().unwrap_or(0)
}

#[test]
fn a_neutral_scene_grade_reproduces_every_log_spaces_picture() {
    let mut tested = 0;
    for cs in ColorSpace::ALL.into_iter().filter(|c| c.is_log()) {
        let (p, seq, map) = rig(log_frame(), Some(cs), None);
        let today = render(&p, seq, &map);
        let (p, seq, map) = rig(log_frame(), Some(cs), Some(grade(&[])));
        let scene = render(&p, seq, &map);
        let diff = max_diff(&today, &scene);
        assert!(diff < 2e-4, "{}: the scene path differs from today's by {diff}", cs.id());
        assert!(max_code_diff(&today, &scene) <= 1, "{}", cs.id());
        // and the roll-off really is deferred: without it the picture would differ (the log ramp reaches above white)
        let t = InputTransform::new_scene(cs, Range::Full, &ColorPipeline::REC709, None);
        assert!(t.deferred_tone_peak_nits().is_some(), "{}", cs.id());
        assert!(t.gpu_params().is_some_and(|g| g.tone.is_none()), "{}: the scene transform still tone maps on the GPU", cs.id());
        tested += 1;
    }
    assert!(tested >= 11, "only {tested} log colour spaces ran");
}

#[test]
fn a_scene_grade_changes_the_picture_before_the_roll_off() {
    // pulling the exposure down 2 stops on a log clip keeps whites that were above the knee distinct from the walls:
    // the grade is applied to values the roll-off has not compressed yet, so the result differs from the same
    // exposure applied after it (Lumetri's)
    let (p, seq, map) = rig(log_frame(), Some(ColorSpace::NLogBt2020), Some(grade(&[("exposure", -2.0)])));
    let scene = render(&p, seq, &map);
    let (p, seq, map) = rig(log_frame(), Some(ColorSpace::NLogBt2020), None);
    let today = render(&p, seq, &map);
    // the brightest column: today it is white; two stops down on scene values it is still rolled off, below white
    let top = |i: &Image| i.px[(W - 1) * 4];
    assert!(top(&today) >= 0.99, "{}", top(&today));
    assert!(top(&scene) < 0.99 && top(&scene) > 0.3, "{}", top(&scene));
}

#[test]
fn sdr_media_gets_no_second_tone_map() {
    let (p, seq, map) = rig(sdr_frame(), None, None);
    let today = render(&p, seq, &map);
    // neutral: bit for bit what the clip has without it
    let (p2, seq2, map2) = rig(sdr_frame(), None, Some(grade(&[])));
    let neutral = render(&p2, seq2, &map2);
    assert_eq!(today.px, neutral.px);
    // the clip's colour management is the usual one: nothing deferred, so nothing for the effect to tone map
    let item = p2.sequence(seq2).and_then(|s| s.video_tracks[0].items.first()).expect("clip");
    let cc = crate::colorman::clip_color(&p2, item, &sdr_frame(), &ColorPipeline::REC709, true);
    assert_eq!(cc.deferred_tone_peak_nits, None);
    // a real grade works on the picture as it is (an SDR white stays white with white level 1)
    let (p3, seq3, map3) = rig(sdr_frame(), None, Some(grade(&[("exposure", 1.0)])));
    let graded = render(&p3, seq3, &map3);
    let x = W - 1;
    let (a, b) = (today.px[((H / 2) * W + x) * 4], graded.px[((H / 2) * W + x) * 4]);
    assert!((b / a - 2.0).abs() < 1e-3, "one stop up doubles the linear value, with no roll-off: {a} -> {b}");
}

/// A Scene Grade with the given Source choice and exposure.
fn grade_from(source: u32, exposure: f64) -> EffectInstance {
    let mut e = grade(&[("exposure", exposure)]);
    e.params.get_mut("source").expect("source").value = ParamValue::Choice(source);
    e
}

#[test]
fn the_source_override_reads_the_clip_as_n_log_without_touching_the_media() {
    // a master read as Rec. 709 that carries an N-Log signal, graded with Source: N-Log ...
    let (p, seq, map) = rig(log_frame(), Some(ColorSpace::Rec709), Some(grade_from(SOURCE_NLOG, 0.5)));
    let overridden = render(&p, seq, &map);
    // ... is the picture of the same media interpreted as N-Log, with the same grade
    let (q, qseq, qmap) = rig(log_frame(), Some(ColorSpace::NLogBt2020), Some(grade_from(0, 0.5)));
    let by_interpretation = render(&q, qseq, &qmap);
    assert_eq!(overridden.px, by_interpretation.px);
    assert!(overridden.px.iter().any(|v| *v > 0.0));
    // the media item's own interpretation did not change
    let item = p.sequence(seq).expect("sequence").video_tracks[0].items[0].item;
    assert_eq!(crate::colorman::override_of(&p, item), Some(ColorSpace::Rec709));
    // and without the override the same clip is read as Rec. 709: a different picture
    let (p4, seq4, map4) = rig(log_frame(), Some(ColorSpace::Rec709), Some(grade_from(0, 0.5)));
    assert!(max_diff(&render(&p4, seq4, &map4), &overridden) > 0.05);
    // a disabled Scene Grade does nothing, override included; so does a render with effects off
    let mut off = grade_from(SOURCE_NLOG, 0.5);
    off.enabled = false;
    let (p6, seq6, map6) = rig(log_frame(), Some(ColorSpace::Rec709), Some(off));
    let (p5, seq5, map5) = rig(log_frame(), Some(ColorSpace::Rec709), None);
    assert_eq!(render(&p6, seq6, &map6).px, render(&p5, seq5, &map5).px);
    let no_fx = RenderOptions { effects: false, ..RenderOptions::default() };
    assert_eq!(render_sequence(&p, seq, Tick::ZERO, no_fx, &map).px, render_sequence(&p5, seq5, Tick::ZERO, no_fx, &map5).px);
}

#[test]
fn a_masked_scene_grade_does_not_take_over_the_roll_off() {
    // a Scene Grade with a mask grades part of the picture only: it cannot stand in for the whole clip's roll-off,
    // so the clip decodes as ever (the roll-off at input) and the masked grade works on the rolled-off picture
    let mut e = grade(&[("exposure", 1.0)]);
    e.masks.push(filmcraft_project::Mask::new("m", filmcraft_project::MaskPath::rect(0.0, 0.0, W as f64 / 2.0, H as f64)));
    let (p, seq, map) = rig(log_frame(), Some(ColorSpace::NLogBt2020), Some(e));
    let item = p.sequence(seq).and_then(|s| s.video_tracks[0].items.first()).expect("clip");
    assert!(active(item).is_none());
    let cc = crate::colorman::clip_color(&p, item, &log_frame(), &ColorPipeline::REC709, true);
    assert_eq!(cc.deferred_tone_peak_nits, None);
    let out = render(&p, seq, &map);
    assert!(out.px.iter().all(|v| v.is_finite()));
    // the unmasked half is the clip as it is without any effect
    let (q, qseq, qmap) = rig(log_frame(), Some(ColorSpace::NLogBt2020), None);
    let plain = render(&q, qseq, &qmap);
    let right = (W - 4) * 4;
    assert!((out.px[right] - plain.px[right]).abs() < 1e-5);
}

#[test]
fn the_plan_keeps_a_log_clip_with_a_scene_grade_on_the_gpu() {
    let (p, seq, map) = rig(log_frame(), Some(ColorSpace::NLogBt2020), Some(grade(&[("exposure", -1.0)])));
    let plan = plan_frame(&p, seq, Tick::ZERO, RenderOptions::default(), &map);
    let FramePlan::Layers { layers, .. } = &plan else { panic!("the layer fell back to the CPU image") };
    let layer = &layers[0];
    let fx = layer.fx.as_ref().expect("the scene grade runs as GPU effect ops, not on the CPU");
    assert!(matches!(fx.ops.as_slice(), [crate::gpufx::FxOp::SceneGrade(g)] if g.tone_peak_nits.is_some()), "{:?}", fx.ops);
    let input = layer.input.as_ref().expect("the log decode runs on the GPU");
    assert!(input.deferred_tone_peak_nits().is_some());
    assert!(input.gpu_params().is_some_and(|g| g.tone.is_none()));
    // the plan on the CPU is the CPU renderer's picture
    let planned = crate::plan::execute_cpu(&plan);
    assert!(max_diff(&planned, &render(&p, seq, &map)) < 2e-4);
    // the Source override reaches the plan too: a Rec. 709 item read as N-Log is planned like an N-Log one
    let mut e = grade(&[("exposure", -1.0)]);
    e.params.get_mut("source").expect("source").value = ParamValue::Choice(SOURCE_NLOG);
    let (p2, seq2, map2) = rig(log_frame(), Some(ColorSpace::Rec709), Some(e));
    let plan2 = plan_frame(&p2, seq2, Tick::ZERO, RenderOptions::default(), &map2);
    let FramePlan::Layers { layers: l2, .. } = &plan2 else { panic!("the override fell back to the CPU image") };
    assert_eq!(l2[0].input.as_ref().map(|t| t.source), Some(ColorSpace::NLogBt2020));
    assert_eq!(crate::plan::execute_cpu(&plan2).px, crate::plan::execute_cpu(&plan).px);
    // effects off: the ordinary transform, the roll-off at decode
    let off = RenderOptions { effects: false, ..RenderOptions::default() };
    let plan3 = plan_frame(&p, seq, Tick::ZERO, off, &map);
    let FramePlan::Layers { layers: l3, .. } = &plan3 else { panic!("CPU image") };
    assert!(l3[0].fx.is_none() && l3[0].input.as_ref().is_some_and(|t| t.deferred_tone_peak_nits().is_none()));
}

#[test]
fn a_scene_grade_runs_first_wherever_it_is_stacked() {
    let (mut p, seq, map) = rig(log_frame(), Some(ColorSpace::NLogBt2020), None);
    let blur_free = find_effect("brightness_contrast").expect("effect").instance();
    let mut bc = blur_free;
    bc.params.get_mut("brightness").expect("brightness").value = ParamValue::Float(20.0);
    {
        let it = &mut p.sequence_mut(seq).expect("sequence").video_tracks[0].items[0];
        it.effects.push(bc.clone());
        it.effects.push(grade(&[("exposure", -1.0)]));
    }
    let item = &p.sequence(seq).expect("sequence").video_tracks[0].items[0];
    let order: Vec<&str> = render_order(item).iter().map(|e| e.effect.as_str()).collect();
    assert_eq!(order, [ID, "brightness_contrast"]);
    // stacked first or last, the picture is the same
    let last = render(&p, seq, &map);
    let (mut q, qseq, qmap) = rig(log_frame(), Some(ColorSpace::NLogBt2020), None);
    {
        let it = &mut q.sequence_mut(qseq).expect("sequence").video_tracks[0].items[0];
        it.effects.push(grade(&[("exposure", -1.0)]));
        it.effects.push(bc);
    }
    assert_eq!(last.px, render(&q, qseq, &qmap).px);
}

#[test]
fn only_the_first_scene_grade_rolls_the_highlights_off() {
    let (mut p, seq, map) = rig(log_frame(), Some(ColorSpace::NLogBt2020), Some(grade(&[])));
    p.sequence_mut(seq).expect("sequence").video_tracks[0].items[0].effects.push(grade(&[]));
    let (q, qseq, qmap) = rig(log_frame(), Some(ColorSpace::NLogBt2020), Some(grade(&[])));
    // the second one is neutral and adds no second roll-off
    assert!(max_diff(&render(&p, seq, &map), &render(&q, qseq, &qmap)) < 1e-6);
}
