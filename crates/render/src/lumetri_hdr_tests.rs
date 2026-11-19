//! Lumetri in HDR working spaces (known cd/m² values through PQ and HLG) and HSL Secondary ▸
//! Refine (Denoise, Blur of the key).

use filmcraft_color::{GradeSpace, REFERENCE_WHITE_NITS, WorkingSpace};
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_time::Tick;

use crate::Image;
use crate::effects::{FxCtx, apply};

const RW: f32 = REFERENCE_WHITE_NITS as f32;

fn cx(working: WorkingSpace) -> FxCtx<'static> {
    FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        frame_rate: 24.0,
        working,
        scene_peak_nits: None,
    }
}

fn lumetri(params: &[(&str, ParamValue)]) -> EffectInstance {
    let mut e = find_effect("lumetri").unwrap().instance();
    for (k, v) in params {
        e.params.get_mut(*k).unwrap_or_else(|| panic!("no param {k}")).value = v.clone();
    }
    e
}

/// A row of grey pixels at these cd/m², graded; returns their cd/m² afterwards.
fn grade(working: WorkingSpace, nits: &[f32], e: &EffectInstance) -> Vec<f32> {
    let mut img = Image::new(nits.len(), 1);
    for (i, n) in nits.iter().enumerate() {
        img.px[i * 4..i * 4 + 4].copy_from_slice(&[n / RW, n / RW, n / RW, 1.0]);
    }
    apply(&mut img, e, &cx(working));
    (0..nits.len()).map(|i| img.px[i * 4 + 1] * RW).collect()
}

fn close(got: &[f32], want: &[f32], rel: f32) {
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= w.abs() * rel + 0.02, "got {got:?}, want {want:?}");
    }
}

const F: fn(f64) -> ParamValue = ParamValue::Float;
const LEVELS: [f32; 6] = [1.0, 26.0, 100.0, 203.0, 1000.0, 4000.0];

#[test]
fn identity_keeps_hdr_highlights_in_pq_and_hlg() {
    let e = lumetri(&[]);
    close(&grade(WorkingSpace::Rec2100Pq, &LEVELS, &e), &LEVELS, 2e-3);
    // HLG signal tops out around 12× its peak's scene light: 4000 cd/m² is still inside
    close(&grade(WorkingSpace::Rec2100Hlg, &LEVELS, &e), &LEVELS, 2e-3);
    // the same grade in a Rec. 709 sequence clips at SDR white (1.0 = 203 cd/m² here)
    let sdr = grade(WorkingSpace::Rec709, &[1000.0], &e);
    assert!((sdr[0] - RW).abs() < 0.5, "{sdr:?}");
}

#[test]
fn exposure_is_stops_of_light_in_hdr() {
    let e = lumetri(&[("exposure", F(1.0))]);
    let want: Vec<f32> = LEVELS.iter().map(|n| n * 2.0).collect();
    close(&grade(WorkingSpace::Rec2100Pq, &LEVELS, &e), &want, 3e-3);
    close(&grade(WorkingSpace::Rec2100Hlg, &LEVELS[..5], &e), &want[..5], 3e-3);
    let e = lumetri(&[("exposure", F(-2.0))]);
    close(&grade(WorkingSpace::Rec2100Pq, &[400.0, 4000.0], &e), &[100.0, 1000.0], 3e-3);
}

#[test]
fn whites_and_contrast_scale_with_hdr_white() {
    // Whites +100 stretches the signal so 85 % of HDR White becomes HDR White
    for white in [1000.0f32, 4000.0] {
        let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, white);
        let e = lumetri(&[("whites", F(100.0)), ("hdr_white", F(white as f64))]);
        let src = gs.nits_of_signal(0.85);
        let got = grade(WorkingSpace::Rec2100Pq, &[0.0, src], &e);
        assert!(got[0].abs() < 1e-3, "black stays black");
        assert!((got[1] - white).abs() < white * 3e-3, "HDR White {white}: {src} cd/m² → {} (want {white})", got[1]);
    }
    // Contrast pivots on the middle of the HDR White range and leaves speculars above it alone
    let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, 1000.0);
    let (mid, dark, bright) = (gs.nits_of_signal(0.5), gs.nits_of_signal(0.3), gs.nits_of_signal(0.7));
    let e = lumetri(&[("contrast", F(60.0))]);
    let got = grade(WorkingSpace::Rec2100Pq, &[mid, dark, bright, 4000.0], &e);
    assert!((got[0] - mid).abs() < mid * 3e-3, "pivot {mid} → {}", got[0]);
    assert!(got[1] < dark && got[2] > bright, "{got:?}");
    assert!((got[3] - 4000.0).abs() < 4000.0 * 3e-3, "specular kept: {}", got[3]);
    // the same in HLG: reference white is 75 % of a 1000 cd/m² display's signal
    let hlg = GradeSpace::new(WorkingSpace::Rec2100Hlg, 1000.0);
    assert!((hlg.signal_of_nits(203.0) - 0.75).abs() < 2e-3);
    let e = lumetri(&[("whites", F(100.0))]);
    let got = grade(WorkingSpace::Rec2100Hlg, &[hlg.nits_of_signal(0.85)], &e);
    assert!((got[0] - 1000.0).abs() < 4.0, "{got:?}");
}

#[test]
fn hdr_specular_sets_highlights_above_hdr_white() {
    let e = lumetri(&[("hdr_specular", F(-100.0))]);
    let got = grade(WorkingSpace::Rec2100Pq, &[203.0, 1000.0, 4000.0], &e);
    close(&got, &[203.0, 1000.0, 1000.0], 3e-3);
    let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, 1000.0);
    let e = lumetri(&[("hdr_specular", F(50.0))]);
    let got = grade(WorkingSpace::Rec2100Pq, &[1000.0, 4000.0], &e);
    let want = gs.nits_of_signal(1.0 + (gs.signal_of_nits(4000.0) - 1.0) * 1.5);
    close(&got, &[1000.0, want], 3e-3);
    // no effect in SDR (the slider is HDR-only)
    let sdr = grade(WorkingSpace::Rec709, &[150.0], &lumetri(&[("hdr_specular", F(-100.0))]));
    assert!((sdr[0] - 150.0).abs() < 0.2);
}

#[test]
fn curves_span_the_hdr_range() {
    // a luma curve lifting the middle: 0.5 → 0.6 of the HDR Range
    let curve = ParamValue::Curve(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]);
    for range in [1000.0f32, 2000.0] {
        let gs = GradeSpace::new(WorkingSpace::Rec2100Pq, range);
        let e = lumetri(&[("curve_luma", curve.clone()), ("curves_hdr_range", F(range as f64))]);
        let got = grade(WorkingSpace::Rec2100Pq, &[gs.nits_of_signal(0.5), range, 6000.0], &e);
        close(&got, &[gs.nits_of_signal(0.6), range, 6000.0], 4e-3);
    }
}

/// A 40×40 image: grey background, a red 16×16 square, and isolated red speckles.
fn keyed_image() -> Image {
    let mut img = Image::filled(40, 40, [0.2, 0.2, 0.2, 1.0]);
    let red = [0.8, 0.02, 0.02, 1.0];
    for y in 12..28 {
        for x in 12..28 {
            img.px[(y * 40 + x) * 4..][..4].copy_from_slice(&red);
        }
    }
    for (x, y) in [(3, 3), (35, 6), (5, 33), (33, 34), (20, 4)] {
        img.px[(y * 40 + x) * 4..][..4].copy_from_slice(&red);
    }
    img
}

fn key(denoise: f64, blur: f64) -> Image {
    let mut img = keyed_image();
    let e = lumetri(&[
        ("hsl_on", ParamValue::Bool(true)),
        ("hsl_hue", F(0.0)),
        ("hsl_hue_range", F(40.0)),
        ("hsl_show_mask", ParamValue::Choice(3)),
        ("hsl_denoise", F(denoise)),
        ("hsl_blur", F(blur)),
    ]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec709));
    img
}

#[test]
fn hsl_refine_denoise_removes_speckles() {
    let m = |img: &Image, x: usize, y: usize| filmcraft_color::linear_to_srgb(img.get(x, y)[1]);
    let raw = key(0.0, 0.0);
    assert!(m(&raw, 3, 3) > 0.9 && m(&raw, 20, 20) > 0.9 && m(&raw, 8, 20) < 0.01, "the key: speckles and square");
    let clean = key(100.0, 0.0);
    for (x, y) in [(3, 3), (35, 6), (5, 33), (33, 34), (20, 4)] {
        assert!(m(&clean, x, y) < 0.05, "speckle at {x},{y}: {}", m(&clean, x, y));
    }
    assert!(m(&clean, 20, 20) > 0.95, "the square stays keyed");
    // half denoise: half way
    let half = key(50.0, 0.0);
    assert!((0.3..0.7).contains(&m(&half, 3, 3)), "{}", m(&half, 3, 3));
}

#[test]
fn hsl_refine_blur_softens_the_key() {
    let soft = |img: &Image| (0..40).filter(|&x| (0.05..0.95).contains(&img.get(x, 20)[1])).count();
    let hard = key(0.0, 0.0);
    assert_eq!(soft(&hard), 0, "a hard key");
    let blurred = key(0.0, 25.0);
    let n = soft(&blurred);
    assert!(n >= 6, "soft edge pixels: {n}");
    // the key's total is kept (blur only moves it around)
    let total = |img: &Image| (0..40 * 40).map(|i| filmcraft_color::linear_to_srgb(img.px[i * 4 + 1])).sum::<f32>();
    assert!((total(&blurred) - total(&hard)).abs() / total(&hard) < 0.05);
    // the correction follows the refined key: a hue shift inside, soft at the edge
    let mut img = keyed_image();
    let e =
        lumetri(&[("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(0.0)), ("hsl_hue_range", F(40.0)), ("hsl_hue_shift", F(120.0)), ("hsl_denoise", F(100.0))]);
    apply(&mut img, &e, &cx(WorkingSpace::Rec709));
    let inside = img.get(20, 20);
    assert!(inside[1] > inside[0], "red turned green inside: {inside:?}");
    let speck = img.get(3, 3);
    assert!(speck[0] > speck[1], "speckle left alone after denoise: {speck:?}");
}

// HSL Secondary ▸ Correction ▸ Exposure / Contrast

/// A tan patch (HSL hue 30°, saturation 30 %, lightness 45 %, the Hawkscrest floor) beside a
/// warm white (hue 30°, saturation 5 %, lightness 85 %, the walls), in display (sRGB) values.
/// HSL saturation grows near white, so a wall that reads 5 % is only ±0.75 % per channel.
const TAN: [f32; 3] = [0.585, 0.45, 0.315];

fn tan_and_white() -> Image {
    let mut img = Image::new(2, 1);
    let warm_white = filmcraft_color::hsl_to_rgb(30.0 / 360.0, 0.05, 0.85);
    for (i, c) in [TAN, warm_white].iter().enumerate() {
        let lin = c.map(filmcraft_color::srgb_to_linear);
        img.px[i * 4..i * 4 + 4].copy_from_slice(&[lin[0], lin[1], lin[2], 1.0]);
    }
    img
}

/// A floor key like Hawkscrest's: centred on 30° (range 40°), saturation from 15 %.
fn floor_key(extra: &[(&str, ParamValue)]) -> EffectInstance {
    let mut p = vec![("hsl_on", ParamValue::Bool(true)), ("hsl_hue", F(30.0)), ("hsl_hue_range", F(40.0)), ("hsl_sat_min", F(15.0))];
    p.extend(extra.iter().cloned());
    lumetri(&p)
}

fn graded(e: &EffectInstance) -> Image {
    let mut img = tan_and_white();
    apply(&mut img, e, &cx(WorkingSpace::Rec709));
    img
}

fn display(img: &Image, x: usize) -> [f32; 3] {
    let p = img.get(x, 0);
    [p[0], p[1], p[2]].map(filmcraft_color::linear_to_srgb)
}

#[test]
fn hsl_exposure_and_contrast_at_zero_render_older_grades_bit_identical() {
    let with = floor_key(&[("hsl_exposure", F(0.0)), ("hsl_contrast", F(0.0)), ("hsl_sat", F(120.0))]);
    let mut without = with.clone();
    without.params.remove("hsl_exposure");
    without.params.remove("hsl_contrast");
    assert_eq!(graded(&with).px, graded(&without).px);
}

#[test]
fn hsl_exposure_brightens_the_keyed_tan_and_leaves_the_warm_white() {
    let base = graded(&floor_key(&[]));
    let lifted = graded(&floor_key(&[("hsl_exposure", F(1.0))]));
    // the tan is fully inside the key: its display value goes up by 2^(1/2.4), hue kept
    let (b, l) = (display(&base, 0), display(&lifted, 0));
    let g = 2f32.powf(1.0 / 2.4);
    for k in 0..3 {
        assert!((l[k] - b[k] * g).abs() < 2e-3, "channel {k}: {b:?} -> {l:?}, want ×{g}");
    }
    // the warm white is below Saturation Min: not one bit moves
    assert_eq!(base.get(1, 0), lifted.get(1, 0));
    // a darker exposure works the same way down
    let dark = display(&graded(&floor_key(&[("hsl_exposure", F(-1.0))])), 0);
    assert!((dark[1] - b[1] / g).abs() < 2e-3, "{dark:?}");
}

#[test]
fn hsl_contrast_moves_the_keyed_luma_along_the_basic_curve_and_keeps_hue() {
    let base = display(&graded(&floor_key(&[])), 0);
    let more = display(&graded(&floor_key(&[("hsl_contrast", F(100.0))])), 0);
    let less = display(&graded(&floor_key(&[("hsl_contrast", F(-100.0))])), 0);
    let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    // the tan sits below mid grey: more contrast darkens it, less lifts it toward 0.5
    assert!(luma(more) < luma(base) - 0.005 && luma(less) > luma(base) + 0.005, "{base:?} {more:?} {less:?}");
    // a scale, not a per-channel curve: the channel ratios hold
    for c in [more, less] {
        assert!((c[0] / c[2] - base[0] / base[2]).abs() < 1e-3, "{c:?} vs {base:?}");
    }
    // the warm white stays put
    assert_eq!(graded(&floor_key(&[])).get(1, 0), graded(&floor_key(&[("hsl_contrast", F(50.0))])).get(1, 0));
}

#[test]
fn hsl_show_mask_views_ignore_exposure_and_contrast() {
    for mode in 1..=3 {
        let plain = graded(&floor_key(&[("hsl_show_mask", ParamValue::Choice(mode))]));
        let corrected = graded(&floor_key(&[("hsl_show_mask", ParamValue::Choice(mode)), ("hsl_exposure", F(2.0)), ("hsl_contrast", F(60.0))]));
        assert_eq!(plain.px, corrected.px, "Show Mask mode {mode}");
    }
}

#[test]
fn hsl_exposure_keeps_hdr_headroom() {
    // in PQ the tan's corrected value is scaled back by the pixel's HDR scale, not clipped at 1
    let e = floor_key(&[("hsl_exposure", F(1.0))]);
    let mut img = Image::new(1, 1);
    let lin = TAN.map(filmcraft_color::srgb_to_linear).map(|v| v * 4.0);
    img.px.copy_from_slice(&[lin[0], lin[1], lin[2], 1.0]);
    let before = img.get(0, 0);
    apply(&mut img, &e, &cx(WorkingSpace::Rec2100Pq));
    let after = img.get(0, 0);
    assert!(after[0] > before[0] * 1.2 && after[0].is_finite(), "{before:?} -> {after:?}");
}
