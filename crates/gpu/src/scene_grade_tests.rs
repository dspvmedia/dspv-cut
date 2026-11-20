//! Scene Grade on the GPU: the WGSL op against the CPU reference (`FxOp::SceneGrade`), and a neutral
//! Scene Grade against the plain input transform for every log colour space. Synthetic frames only.

use super::blend_tests::device;
use super::*;
use filmcraft_color::{ColorInfo, ColorPipeline, ColorSpace, InputTransform, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{Chroma, PixelData};
use filmcraft_geom::Affine;
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_render::Blend;
use filmcraft_render::effects::FxCtx;
use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::{FramePlan, LayerFx, PlanLayer, execute_cpu};
use filmcraft_time::Tick;

fn cx(peak: Option<f32>) -> FxCtx<'static> {
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
    let mut e = find_effect("scene_grade").expect("scene_grade is registered").instance();
    for (k, v) in params {
        e.params.get_mut(*k).unwrap_or_else(|| panic!("{k}")).value = ParamValue::Float(*v);
    }
    e
}

fn half(v: f32) -> f32 {
    super::blend_tests::f16_to_f32(f32_to_f16(v))
}

/// Scene-linear premultiplied picture: from 1e-4 to above 14 (an N-Log white) along x, in chroma bands down y,
/// alpha 1 with a partial band. Every value is representable in half floats.
fn scene_picture(w: u32, h: u32) -> Vec<f32> {
    let chroma = [[1.0, 1.0, 1.0], [1.0, 0.7, 0.4], [0.4, 0.7, 1.0], [0.5, 1.0, 0.5], [1.0, 0.2, 0.1], [0.2, 0.3, 1.0]];
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let v = 1e-4 * (1.5e5f32).powf(x as f32 / (w - 1) as f32);
            let c = chroma[(y as usize) % chroma.len()];
            let a = if y % 7 == 3 { 0.5 } else { 1.0 };
            px.extend_from_slice(&[c[0] * v * a, c[1] * v * a, c[2] * v * a, a].map(half));
        }
    }
    px
}

/// Settings from neutral to extreme, with and without the tone map.
fn settings() -> Vec<(Vec<(&'static str, f64)>, Option<f32>)> {
    let interior = vec![
        ("desat", 0.5),
        ("white", 0.92),
        ("contrast", 1.45),
        ("highlight_exposure", -1.0),
        ("dark_exposure", -1.0),
        ("shadow_exposure", 1.5),
        ("kelvin", 3100.0),
        ("tint", 12.0),
        ("exposure", -0.7),
    ];
    let mut v = vec![
        (vec![], None),
        (vec![], Some(3000.0)),
        (interior.clone(), Some(3000.0)),
        (interior.clone(), None),
        (vec![("kelvin", 2200.0), ("tint", -60.0), ("gain_r", 1.3), ("gain_b", 0.6), ("exposure", 1.5), ("contrast", 0.6), ("pivot", 0.05)], Some(1500.0)),
        (vec![("kelvin", 14_000.0), ("tint", 90.0), ("contrast", 3.0), ("shadow_exposure", 5.0), ("highlight_exposure", 4.0), ("white", 1.2)], Some(4000.0)),
        (vec![("highlight_width", 0.0), ("highlight_exposure", 3.0), ("desat", 1.0), ("white", 0.5)], Some(3000.0)),
    ];
    v.push((vec![("exposure", -4.0), ("desat", 0.8), ("contrast", 2.0), ("dark_center", -6.0), ("dark_width", 0.3), ("dark_exposure", -5.0)], Some(203.5)));
    // lens falloff: a correction under the interior look, a darkening off-centre falloff, and the extreme
    // (centre at a corner, the highest power and amount) that runs into the gain's 40-stop clamp
    let mut lens = interior;
    lens.extend([("falloff_amount", 2.0), ("falloff_power", 2.0)]);
    v.push((lens, Some(3000.0)));
    v.push((vec![("falloff_amount", -2.0), ("falloff_power", 3.0), ("falloff_center_x", 10.0), ("falloff_center_y", -20.0)], None));
    v.push((vec![("falloff_amount", 6.0), ("falloff_power", 8.0), ("falloff_center_x", 50.0), ("falloff_center_y", -50.0)], None));
    v.push((vec![("falloff_amount", 6.0), ("falloff_power", 8.0), ("falloff_center_x", -50.0), ("falloff_center_y", 50.0), ("exposure", -3.0)], Some(3000.0)));
    v
}

/// The op on the GPU against the CPU reference, on the working image read back as f32, for every setting.
#[test]
fn scene_grade_gpu_op_matches_the_cpu_reference() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    assert!(c.fx.is_some(), "the effect stage did not build on this device");
    let (w, h) = (131u32, 25u32);
    let px = scene_picture(w, h);
    let frame = VideoFrame::rgba_f32(w, h, px.clone());
    let mut worst_all = 0f32;
    for (params, peak) in settings() {
        let op = FxOp::eval(&grade(&params), &cx(peak), w as usize, h as usize).expect("scene_grade has a GPU op");
        assert!(op.gpu_ok(), "{params:?}");
        let mut cpu = filmcraft_render::Image { w: w as usize, h: h as usize, px: px.clone() };
        op.apply(&mut cpu);
        let (_, _, gpu) = c.effect_image(&frame, &LayerFx { size: (w, h), decimation: 1, ops: vec![op] }).expect("effect image");
        let mut worst = 0f32;
        for (a, b) in cpu.px.iter().zip(&gpu) {
            assert!(b.is_finite(), "{params:?} {peak:?}: non-finite GPU value");
            worst = worst.max((a - b).abs() / (1.0 + a.abs()));
        }
        eprintln!("{params:?} {peak:?}: max relative difference {worst:.2e}");
        assert!(worst < 5e-4, "{params:?} {peak:?}: GPU differs from the CPU by {worst:.2e}");
        worst_all = worst_all.max(worst);
    }
    eprintln!("worst over all settings: {worst_all:.2e}");
}

fn code_diff(a: &[u8], b: &[u8]) -> u8 {
    a.chunks(4).zip(b.chunks(4)).flat_map(|(p, q)| (0..3).map(move |k| p[k].abs_diff(q[k]))).max().unwrap_or(0)
}

/// The composited picture, 8-bit: GPU against the CPU plan, within one code, for a float source through the effect.
#[test]
fn scene_grade_composited_gpu_picture_is_within_one_code_of_the_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (131u32, 25u32);
    let frame = Arc::new(VideoFrame::rgba_f32(w, h, scene_picture(w, h)));
    for (params, peak) in settings() {
        let op = FxOp::eval(&grade(&params), &cx(peak), w as usize, h as usize).expect("op");
        let layer = PlanLayer {
            fx: Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops: vec![op] })),
            ..PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal)
        };
        let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
        let cpu = execute_cpu(&plan).over_black_rgba8();
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let worst = code_diff(&cpu, &gpu);
        assert!(worst <= 1, "{params:?} {peak:?}: GPU differs from the CPU by {worst} codes");
    }
}

/// 16-bit full-range Y'CbCr: luma from below black to above white, under neutral and pushed chroma (an N-RAW decode).
fn log_frame(w: u32, h: u32) -> Arc<VideoFrame> {
    let top = 65_535u32;
    let mid = 32_768i64;
    let off = (top / 3) as i64;
    let chroma = [(mid, mid), (mid, mid), (mid - off, mid + off), (mid + off, mid - off), (mid, mid + off), (mid, mid - off)];
    let y: Vec<u16> = (0..h).flat_map(|_| (0..w).map(|x| (x * top / (w - 1)) as u16)).collect();
    let cb: Vec<u16> = (0..h).flat_map(|r| std::iter::repeat_n(chroma[r as usize % 6].0 as u16, w as usize)).collect();
    let cr: Vec<u16> = (0..h).flat_map(|r| std::iter::repeat_n(chroma[r as usize % 6].1 as u16, w as usize)).collect();
    let color = ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Bt709, primaries: Primaries::Bt2020, range: Range::Full };
    Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C444, bits: 16, alpha: None },
        color,
        par: (1, 1),
        pts: Default::default(),
    })
}

/// A neutral Scene Grade gives today's picture on every log colour space, on the GPU and on the CPU: the input
/// transform leaves its tone map to the effect, which applies it as its last step.
#[test]
fn a_neutral_scene_grade_is_todays_picture_on_every_log_space() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (256u32, 6u32);
    let frame = log_frame(w, h);
    let mut tested = 0;
    for cs in ColorSpace::ALL.into_iter().filter(|c| c.is_log()) {
        let today_t = Arc::new(InputTransform::new(cs, Range::Full, &ColorPipeline::REC709, None));
        let scene_t = Arc::new(InputTransform::new_scene(cs, Range::Full, &ColorPipeline::REC709, None));
        let peak = scene_t.deferred_tone_peak_nits().map(|p| p as f32);
        assert!(peak.is_some(), "{}", cs.id());
        assert!(scene_t.gpu_params().is_some_and(|p| p.tone.is_none()), "{}", cs.id());
        let op = FxOp::eval(&grade(&[]), &cx(peak), w as usize, h as usize).expect("op");
        let today = FramePlan::Layers {
            width: w as usize,
            height: h as usize,
            layers: vec![PlanLayer { input: Some(today_t), ..PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal) }],
        };
        let scene = FramePlan::Layers {
            width: w as usize,
            height: h as usize,
            layers: vec![PlanLayer {
                input: Some(scene_t),
                fx: Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops: vec![op] })),
                ..PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal)
            }],
        };
        let (cpu_today, cpu_scene) = (execute_cpu(&today).over_black_rgba8(), execute_cpu(&scene).over_black_rgba8());
        c.composite(&today);
        let (_, _, gpu_today) = c.read_output().expect("readback");
        c.composite(&scene);
        let (_, _, gpu_scene) = c.read_output().expect("readback");
        let (cpu, gpu, across) = (code_diff(&cpu_today, &cpu_scene), code_diff(&gpu_today, &gpu_scene), code_diff(&cpu_scene, &gpu_scene));
        eprintln!("{}: scene vs today {cpu} codes on the CPU, {gpu} on the GPU; GPU vs CPU {across}", cs.id());
        assert!(cpu <= 1, "{}: the CPU scene picture differs from today's by {cpu} codes", cs.id());
        assert!(gpu <= 1, "{}: the GPU scene picture differs from today's by {gpu} codes", cs.id());
        tested += 1;
    }
    assert!(tested >= 11, "only {tested} log spaces ran");
}

/// An SDR source is not tone mapped: a neutral Scene Grade changes nothing, and nothing is rolled off twice.
#[test]
fn a_neutral_scene_grade_leaves_an_sdr_source_alone() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (64u32, 32u32);
    let frame = super::blend_tests::yuv_frame(w, h);
    let op = FxOp::eval(&grade(&[]), &cx(None), w as usize, h as usize).expect("op");
    let plain = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal)] };
    let graded = FramePlan::Layers {
        width: w as usize,
        height: h as usize,
        layers: vec![PlanLayer {
            fx: Some(Arc::new(LayerFx { size: (w, h), decimation: 1, ops: vec![op] })),
            ..PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)
        }],
    };
    c.composite(&plain);
    let (_, _, a) = c.read_output().expect("readback");
    c.composite(&graded);
    let (_, _, b) = c.read_output().expect("readback");
    assert!(code_diff(&a, &b) <= 1);
}
