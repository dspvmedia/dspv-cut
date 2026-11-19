//! Scene Grade (`scene_grade`): grading on the clip's scene-linear picture, before the highlight
//! roll-off (the design: `docs/scene-referred-grading-design.md` of the DSPV Cut project).
//!
//! For log media in a plain Rec. 709 sequence the input transform rolls the highlights
//! off at decode, so every effect works on display-referred values. A clip whose Scene Grade is
//! active ([`active`]) decodes without that tone map (`InputTransform::new_scene`) and the effect
//! applies the same tone map as its last step, after the grade:
//!
//! ```text
//! white balance, exposure, gains (one 3×3, scene-linear)
//!   → contrast around a pivot and luminance zones, on log2 luminance, applied as a ratio Y'/Y
//!   → tone map (BT.2390 on max(R,G,B), the roll-off the input transform skipped)
//!   → white level → highlight desaturation ("path to white")
//! ```
//!
//! The gamut compression of the input transform keeps max(R,G,B) and the tone map is a ratio on it,
//! so the two commute: a Scene Grade at its defaults reproduces the picture the clip has without it.
//!
//! The parameters are evaluated into [`SceneParams`] ([`eval`]) with every hostile value clamped, and
//! run per pixel by [`grade_pixel`] (the CPU reference, `effects::scene_grade`) and by the WGSL
//! op of the same name. The maths follows the numpy prototype of the design (`scene.py`), which
//! produced the signed-off Step 0 look.

use filmcraft_color::ToneMap;
use filmcraft_color::spaces::{Gamut, Mat3, inverse, mul, mul_vec};
use filmcraft_project::{EffectInstance, ParamValue, TrackItem};

use crate::effects::{FxCtx, choice, f};

/// The effect's id.
pub const ID: &str = "scene_grade";

/// 18 % grey, where the contrast pivots and the zones are measured from.
pub const MID_GREY: f32 = 0.18;
/// The Source choice that builds the clip's input transform as the house log curve.
pub const SOURCE_NLOG: u32 = 1;
/// The Kelvin that leaves the picture alone (D65).
pub const NEUTRAL_KELVIN: f64 = 6500.0;
/// Duv (distance from the Planckian locus in CIE 1960 uv) of a Tint of ±100.
const TINT_DUV: f64 = 0.03;
/// The grade's output luminance, in stops from 18 % grey, is held within this.
pub const MAX_STOPS: f32 = 40.0;
/// The most a white balance changes any cone channel (see [`wb_matrix`]).
const MAX_WB_GAIN: f64 = 16.0;
/// Smallest zone width, in stops, that is still a zone (a narrower one is switched off).
const MIN_ZONE_WIDTH: f32 = 1e-3;
/// Share of the contrast slope the zones may take away at most: the grade's slope stays at least
/// `1 - ZONE_SLOPE_SHARE` of the Contrast value, so it is monotone for any setting.
const ZONE_SLOPE_SHARE: f32 = 0.9;

/// One luminance zone: a raised-cosine bump on log2 luminance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    /// Stops from 18 % grey.
    pub center: f32,
    /// Half-width in stops: the bump reaches zero this far from the centre.
    pub width: f32,
    /// Stops added at the centre.
    pub exposure: f32,
}

/// An evaluated Scene Grade (see [`eval`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneParams {
    /// White balance, exposure and per-channel gains as one linear 3×3 (row-major).
    pub matrix: [[f32; 3]; 3],
    pub contrast: f32,
    /// log2(pivot / 18 %).
    pub pivot_stops: f32,
    /// Dark, shadow and highlight zones, exposures already limited so the grade stays monotone.
    pub zones: [Zone; 3],
    /// Output white level, multiplied in after the tone map.
    pub white: f32,
    /// Highlight desaturation (0 = off).
    pub desat: f32,
    /// Source peak (cd/m²) of the tone map applied at the end; `None`: the clip's input transform
    /// already applied it (or this source is not tone mapped) and there is none here.
    pub tone_peak_nits: Option<f32>,
}

impl SceneParams {
    /// Whether the contrast / zone stage changes anything (at Contrast 1 with no zone exposure
    /// the log remap is the identity, so it is skipped rather than rounded through log2 / exp2).
    pub fn luminance_stage(&self) -> bool {
        self.contrast != 1.0 || self.zones.iter().any(|z| z.exposure != 0.0)
    }

    /// Every number finite (the GPU shader is only handed such an op).
    pub fn is_finite(&self) -> bool {
        self.matrix.iter().flatten().all(|v| v.is_finite())
            && self.zones.iter().all(|z| z.center.is_finite() && z.width.is_finite() && z.exposure.is_finite())
            && [self.contrast, self.pivot_stops, self.white, self.desat].iter().all(|v| v.is_finite())
            && self.tone_peak_nits.is_none_or(f32::is_finite)
    }
}

/// A parameter at the evaluation time, clamped to `lo..=hi`; a non-finite value (a hostile project
/// file or control-channel value) falls back to `default`.
fn param(e: &EffectInstance, id: &str, cx: &FxCtx, (lo, hi): (f32, f32), default: f32) -> f32 {
    let v = f(e, id, cx);
    if v.is_finite() { v.clamp(lo, hi) } else { default }
}

/// Evaluate `e` at `cx.t`. The tone map's source peak comes from the clip's colour management
/// (`FxCtx::scene_peak_nits`), never from a parameter.
pub fn eval(e: &EffectInstance, cx: &FxCtx) -> SceneParams {
    let kelvin = param(e, "kelvin", cx, (1667.0, 25_000.0), NEUTRAL_KELVIN as f32) as f64;
    let tint = param(e, "tint", cx, (-100.0, 100.0), 0.0) as f64;
    let exposure = param(e, "exposure", cx, (-16.0, 16.0), 0.0);
    let gain = |id: &str| param(e, id, cx, (0.0, 64.0), 1.0);
    let contrast = param(e, "contrast", cx, (0.05, 8.0), 1.0);
    let pivot = param(e, "pivot", cx, (1e-3, 16.0), MID_GREY);
    let zone = |name: &str, d: (f32, f32)| {
        let width = param(e, &format!("{name}_width"), cx, (0.0, 24.0), d.1);
        let center = param(e, &format!("{name}_center"), cx, (-24.0, 24.0), d.0);
        // a zone with no width is no zone (the raised cosine divides by it)
        let exposure = if width < MIN_ZONE_WIDTH { 0.0 } else { param(e, &format!("{name}_exposure"), cx, (-12.0, 12.0), 0.0) };
        Zone { center, width, exposure }
    };
    let mut zones = [zone("dark", (-3.0, 2.0)), zone("shadow", (-1.4, 2.5)), zone("highlight", (5.5, 3.0))];
    limit_zones(&mut zones, contrast);

    let a = wb_matrix(kelvin, tint);
    let scale = [gain("gain_r"), gain("gain_g"), gain("gain_b")].map(|g| g as f64 * (exposure as f64).exp2());
    let mut matrix = [[0.0f32; 3]; 3];
    for (r, row) in matrix.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = (scale[r] * a[r][c]) as f32;
        }
    }
    SceneParams {
        matrix,
        contrast,
        pivot_stops: (pivot / MID_GREY).log2(),
        zones,
        white: param(e, "white", cx, (0.0, 16.0), 1.0),
        desat: param(e, "desat", cx, (0.0, 1.0), 0.0),
        tone_peak_nits: cx.scene_peak_nits.filter(|p| p.is_finite() && *p > 0.0),
    }
}

/// Slope of a zone's bump at `s` stops: d/ds of `exposure · ½(1 + cos(π (s − center) / width))`.
fn zone_slope(z: &Zone, s: f32) -> f32 {
    let u = (s - z.center) / z.width;
    if z.width < MIN_ZONE_WIDTH || u.abs() >= 1.0 { 0.0 } else { -z.exposure * 0.5 * std::f32::consts::PI * (std::f32::consts::PI * u).sin() / z.width }
}

/// Keep the grade monotone for any setting. The curve on log2 luminance has slope
/// `contrast + Σ zone slopes`; a zone's slope is at most `|exposure| · π / (2 · width)`, so each
/// exposure is held to `0.9 · 2 · width · contrast / π`, and the zones together (they overlap) are
/// scaled down until the slope of their sum, which is found on a fine sampling of each zone's
/// span, never takes more than 90 % of the contrast: the slope stays above a tenth of it.
fn limit_zones(zones: &mut [Zone; 3], contrast: f32) {
    let per_zone = ZONE_SLOPE_SHARE * 2.0 * contrast / std::f32::consts::PI;
    for z in zones.iter_mut() {
        let bound = per_zone * z.width;
        z.exposure = z.exposure.clamp(-bound, bound);
    }
    const STEPS: usize = 256;
    let mut steepest_drop = 0.0f32;
    for z in zones.iter().filter(|z| z.exposure != 0.0) {
        for i in 0..=STEPS {
            let s = z.center - z.width + 2.0 * z.width * i as f32 / STEPS as f32;
            let sum: f32 = zones.iter().map(|o| zone_slope(o, s)).sum();
            steepest_drop = steepest_drop.max(-sum);
        }
    }
    let allowed = ZONE_SLOPE_SHARE * contrast;
    if steepest_drop > allowed {
        let k = allowed / steepest_drop;
        for z in zones.iter_mut() {
            z.exposure *= k;
        }
    }
}

// ---- white balance

/// Planckian locus xy for `kelvin` (Kim et al., a cubic spline fit valid from 1667 K to 25 000 K).
fn planck_xy(kelvin: f64) -> [f64; 2] {
    let t = kelvin.clamp(1667.0, 25_000.0);
    let (t2, t3) = (t * t, t * t * t);
    let x = if t <= 4000.0 {
        -0.2661239e9 / t3 - 0.2343589e6 / t2 + 0.8776956e3 / t + 0.179910
    } else {
        -3.0258469e9 / t3 + 2.1070379e6 / t2 + 0.2226347e3 / t + 0.240390
    };
    let y = if t <= 2222.0 {
        -1.1063814 * x * x * x - 1.34811020 * x * x + 2.18555832 * x - 0.20219683
    } else if t <= 4000.0 {
        -0.9549476 * x * x * x - 1.37418593 * x * x + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x * x * x - 5.87338670 * x * x + 3.75112997 * x - 0.37001483
    };
    [x, y]
}

/// CIE 1960 uv of an xy chromaticity, and back.
fn xy_to_uv([x, y]: [f64; 2]) -> [f64; 2] {
    let d = -2.0 * x + 12.0 * y + 3.0;
    [4.0 * x / d, 6.0 * y / d]
}
fn uv_to_xy([u, v]: [f64; 2]) -> [f64; 2] {
    let d = 2.0 * u - 8.0 * v + 4.0;
    [3.0 * u / d, 2.0 * v / d]
}

/// The chromaticity of the scene's white for a Kelvin and a Tint. The Planckian locus is shifted
/// so that 6500 K is exactly the D65 white of the working primaries (the camera-RAW convention:
/// Kelvin is read against daylight), and Tint moves the white along the normal of the locus, in
/// uv, towards green when positive (`TINT_DUV` of Duv at ±100). The corrected image goes the
/// other way: a higher Kelvin warms it, a positive Tint adds magenta.
pub fn scene_white_xy(kelvin: f64, tint: f64, d65: [f64; 2]) -> [f64; 2] {
    let p65 = planck_xy(NEUTRAL_KELVIN);
    let shift = |xy: [f64; 2]| [xy[0] + d65[0] - p65[0], xy[1] + d65[1] - p65[1]];
    let uv = xy_to_uv(shift(planck_xy(kelvin)));
    if tint == 0.0 {
        return uv_to_xy(uv);
    }
    let h = 25.0;
    let (a, b) = (xy_to_uv(planck_xy(kelvin - h)), xy_to_uv(planck_xy(kelvin + h)));
    let (tu, tv) = (b[0] - a[0], b[1] - a[1]);
    let len = tu.hypot(tv).max(1e-12);
    // the normal on the side of the locus with more green (larger v)
    let (mut nu, mut nv) = (-tv / len, tu / len);
    if nv < 0.0 {
        (nu, nv) = (-nu, -nv);
    }
    let duv = tint / 100.0 * TINT_DUV;
    uv_to_xy([uv[0] + nu * duv, uv[1] + nv * duv])
}

/// The linear Rec. 709 matrix that takes a scene lit by `kelvin` / `tint` to a D65 white: a
/// Bradford chromatic adaptation as one 3×3 (it is the same in any linear space of these
/// primaries). The identity at 6500 K with Tint 0.
pub fn wb_matrix(kelvin: f64, tint: f64) -> Mat3 {
    const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if kelvin == NEUTRAL_KELVIN && tint == 0.0 {
        return IDENTITY;
    }
    const BRADFORD: Mat3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let to_xyz = Gamut::Bt709.to_xyz();
    // the working white (the XYZ of RGB 1,1,1) and its chromaticity
    let dst_xyz = mul_vec(&to_xyz, [1.0, 1.0, 1.0]);
    let sum = dst_xyz[0] + dst_xyz[1] + dst_xyz[2];
    let d65 = [dst_xyz[0] / sum, dst_xyz[1] / sum];
    let [x, y] = scene_white_xy(kelvin, tint, d65);
    let y = y.max(0.01);
    let src_xyz = [x / y, 1.0, (1.0 - x - y) / y];
    let (src, dst) = (mul_vec(&BRADFORD, src_xyz), mul_vec(&BRADFORD, dst_xyz));
    let to_lms = mul(&BRADFORD, &to_xyz);
    // A white past the spectral locus (the deepest orange pushed far towards green) has a cone response
    // at or below zero; each channel's gain is held to a sixteenfold change, which tungsten at 1700 K needs
    // in blue and no real light needs more of, so such a setting stays a plausible correction
    let gain = |i: usize| (dst[i] / src[i].max(1e-3)).clamp(1.0 / MAX_WB_GAIN, MAX_WB_GAIN);
    let scale: Mat3 = [[gain(0), 0.0, 0.0], [0.0, gain(1), 0.0], [0.0, 0.0, gain(2)]];
    mul(&inverse(&to_lms), &mul(&scale, &to_lms))
}

// ---- the grade

/// One pixel of the grade (straight, scene-linear working colour in, the graded colour out), with
/// the tone map `tone` (when the clip's input transform left it to the effect). `params.tone_peak_nits`
/// says whether there is one; `tone` must then be the map for that peak.
#[inline]
pub fn grade_pixel(c: [f32; 3], p: &SceneParams, tone: Option<&ToneMap>) -> [f32; 3] {
    let m = &p.matrix;
    let mut c =
        [m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2], m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2], m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2]];
    if p.luminance_stage() {
        let y = filmcraft_color::luma709(c[0], c[1], c[2]).max(1e-6);
        let s = (y / MID_GREY).log2();
        let mut s2 = p.pivot_stops + (s - p.pivot_stops) * p.contrast;
        for z in &p.zones {
            if z.exposure != 0.0 {
                let u = ((s - z.center) / z.width).clamp(-1.0, 1.0);
                s2 += z.exposure * 0.5 * (1.0 + (std::f32::consts::PI * u).cos());
            }
        }
        // (clamped: a hostile setting must not overflow to infinity; 40 stops is 2e11 × 18 %)
        let k = MID_GREY * s2.clamp(-MAX_STOPS, MAX_STOPS).exp2() / y;
        c = c.map(|v| v * k);
    }
    let before = c[0].max(c[1]).max(c[2]);
    let mut out = match tone {
        Some(t) => t.apply(c),
        None => c,
    };
    out = out.map(|v| v * p.white);
    // path to white: how hard the shoulder pressed this pixel pulls it towards its own luma; only
    // where a tone map ran (without a shoulder there is nothing to follow)
    if p.desat > 0.0 && tone.is_some() {
        let press = (1.0 - out[0].max(out[1]).max(out[2]) / before.max(1e-9)).clamp(0.0, 1.0);
        let f = p.desat * press;
        let yo = filmcraft_color::luma709(out[0], out[1], out[2]);
        out = out.map(|v| v + (yo - v) * f);
    }
    out
}

// ---- which effect is the clip's Scene Grade

/// A masked effect applies to part of the picture only: it cannot stand in for the clip's roll-off.
fn masked(e: &EffectInstance) -> bool {
    e.masks.iter().any(|m| m.mode != filmcraft_project::MaskMode::None)
}

/// The clip's Scene Grade, the effect that takes part in its colour management: the first enabled
/// `scene_grade`, if it has no mask. A project with several keeps working: the others grade where
/// they sit in the stack but never roll the highlights off.
pub fn active(item: &TrackItem) -> Option<&EffectInstance> {
    item.effects.iter().find(|e| e.effect == ID && e.enabled).filter(|e| !masked(e))
}

/// The clip's standard effects in the order they run: the active Scene Grade first (it grades the
/// scene before anything else sees the picture, whatever its index), the rest as they are stacked.
pub fn render_order(item: &TrackItem) -> Vec<&EffectInstance> {
    let standard = |e: &&EffectInstance| e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e);
    let first = active(item);
    first.into_iter().chain(item.effects.iter().filter(standard).filter(|e| first.is_none_or(|a| !std::ptr::eq(a, *e)))).collect()
}

/// `cx` for evaluating `e`: only the clip's active Scene Grade may apply the roll-off.
pub fn ctx_for<'a>(cx: &FxCtx<'a>, e: &EffectInstance, active: Option<&EffectInstance>) -> FxCtx<'a> {
    let own = active.is_some_and(|a| std::ptr::eq(a, e));
    FxCtx { scene_peak_nits: if own { cx.scene_peak_nits } else { None }, ..*cx }
}

/// Whether the effect's Source says to read the clip as N-Log.
pub fn reads_nlog(e: &EffectInstance) -> bool {
    choice(e, "source") == SOURCE_NLOG
}

// ---- presets

/// A named setting of some of Scene Grade's parameters. The rest (Source, Kelvin, Tint, exposure
/// and gains: what belongs to the clip) are left as they are.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenePreset {
    pub name: &'static str,
    pub description: &'static str,
    /// Parameter id and value, each a float.
    pub params: &'static [(&'static str, f64)],
}

impl ScenePreset {
    /// A Scene Grade instance configured as the preset (the rest at their neutral defaults).
    pub fn instance(&self) -> Option<EffectInstance> {
        let mut e = filmcraft_project::find_effect(ID)?.instance();
        self.apply_to(&mut e);
        Some(e)
    }

    /// Set the preset's parameters on `e`, leaving every other parameter alone.
    pub fn apply_to(&self, e: &mut EffectInstance) {
        for (id, v) in self.params {
            if let Some(p) = e.params.get_mut(*id) {
                p.value = ParamValue::Float(*v);
            }
        }
    }
}

/// Step 0's starting values for the interiors (the design's "DSPV interior" look): highlight
/// desaturation 0.5, white level 0.92, contrast 1.45 and a dark, a shadow and a highlight zone.
pub const DSPV_INTERIOR: ScenePreset = ScenePreset {
    name: "DSPV interior",
    description: "Step 0 starting point for interiors: contrast 1.45, dark, shadow and highlight zones, white level 0.92, highlight desaturation 0.5",
    params: &[
        ("desat", 0.5),
        ("white", 0.92),
        ("contrast", 1.45),
        ("highlight_center", 5.5),
        ("highlight_width", 3.0),
        ("highlight_exposure", -1.0),
        ("dark_center", -3.0),
        ("dark_width", 2.0),
        ("dark_exposure", -1.0),
        ("shadow_center", -1.4),
        ("shadow_width", 2.5),
        ("shadow_exposure", 1.5),
    ],
};

/// The built-in Scene Grade presets.
pub fn presets() -> &'static [ScenePreset] {
    &[DSPV_INTERIOR]
}

/// A built-in preset by name (case-insensitive).
pub fn find_preset(name: &str) -> Option<&'static ScenePreset> {
    presets().iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests;
