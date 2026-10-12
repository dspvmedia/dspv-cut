//! `set_default_gpu_rendering` changes what export settings start with, for the whole process. This file is its own test binary
//! (one process, one test), so changing the process-wide default cannot leak into any other test.
use filmcraft_export::{ExportSettings, GpuRendering, default_gpu_rendering, set_default_gpu_rendering};

#[test]
fn the_default_can_be_changed_for_the_process_and_explicit_values_still_win() {
    assert_eq!(default_gpu_rendering(), GpuRendering::Off, "off until a program says otherwise");
    assert_eq!(ExportSettings::default().gpu_rendering, GpuRendering::Off);

    set_default_gpu_rendering(GpuRendering::Auto);
    assert_eq!(default_gpu_rendering(), GpuRendering::Auto);
    assert_eq!(GpuRendering::default(), GpuRendering::Auto);
    // new settings, and saved settings or presets that do not say, start with the new default
    assert_eq!(ExportSettings::default().gpu_rendering, GpuRendering::Auto);
    let unsaid: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264"})).unwrap();
    assert_eq!(unsaid.gpu_rendering, GpuRendering::Auto);
    // what an export, a preset or a command says explicitly is not overridden
    let off: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264", "gpuRendering": "off"})).unwrap();
    assert_eq!(off.gpu_rendering, GpuRendering::Off);
    let auto: ExportSettings = serde_json::from_value(serde_json::json!({"format": "h264", "gpuRendering": "auto"})).unwrap();
    assert_eq!(auto.gpu_rendering, GpuRendering::Auto);

    set_default_gpu_rendering(GpuRendering::Off);
    assert_eq!(ExportSettings::default().gpu_rendering, GpuRendering::Off);
}
