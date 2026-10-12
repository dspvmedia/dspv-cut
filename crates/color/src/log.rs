//! Camera log curves, implemented from the manufacturers' published specifications.
//!
//! Every curve is a pair `encode(linear) → signal` / `decode(signal) → linear`, where *linear* is
//! scene-linear reflectance (18 % grey = 0.18) and *signal* is the curve's own normalised domain:
//!
//! * [`Domain::CodeValue`]: the specification is written in 10-bit code values / 1023 (full
//!   scale). Sony, Panasonic, ARRI and DJI publish their curves this way (S-Log3 black = 95/1023).
//! * [`Domain::Ire`]: the specification is written in "IRE" (video-range normalised: 0 = code 64,
//!   1 = code 940). Canon publishes Canon Log 2/3 this way; Apple Log is treated the same.
//!
//! [`signal_from_normalized`] / [`normalized_from_signal`] convert between the decoder's
//! normalised value (which already accounts for the file's range flag) and the curve domain.
//! Formulas and sources are listed in the crate README (§ Camera log curves).

use crate::Range;

/// The normalisation a curve's published formula uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    /// 10-bit code value / 1023.
    CodeValue,
    /// Video-range normalised (code 64 → 0, code 940 → 1).
    Ire,
}

/// A camera log encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LogCurve {
    /// Sony S-Log3.
    SLog3,
    /// Panasonic V-Log.
    VLog,
    /// Canon Log 2.
    CLog2,
    /// Canon Log 3.
    CLog3,
    /// ARRI LogC3 (ALEXA classic/Mini/LF, EI 800).
    LogC3,
    /// ARRI LogC4 (ALEXA 35).
    LogC4,
    /// Apple Log (iPhone 15 Pro and later).
    AppleLog,
    /// DJI D-Log.
    DLog,
    /// Nikon N-Log (Z-series cameras, N-RAW and H.265/ProRes log).
    NLog,
    /// RED Log3G10 (R3D, and Nikon N-RAW decoded through the RED SDK).
    Log3G10,
}

impl LogCurve {
    pub const ALL: [LogCurve; 10] = [
        LogCurve::SLog3,
        LogCurve::VLog,
        LogCurve::CLog2,
        LogCurve::CLog3,
        LogCurve::LogC3,
        LogCurve::LogC4,
        LogCurve::AppleLog,
        LogCurve::DLog,
        LogCurve::NLog,
        LogCurve::Log3G10,
    ];

    pub fn domain(self) -> Domain {
        match self {
            LogCurve::CLog2 | LogCurve::CLog3 | LogCurve::AppleLog => Domain::Ire,
            _ => Domain::CodeValue,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LogCurve::SLog3 => "S-Log3",
            LogCurve::VLog => "V-Log",
            LogCurve::CLog2 => "Canon Log 2",
            LogCurve::CLog3 => "Canon Log 3",
            LogCurve::LogC3 => "ARRI LogC3",
            LogCurve::LogC4 => "ARRI LogC4",
            LogCurve::AppleLog => "Apple Log",
            LogCurve::DLog => "DJI D-Log",
            LogCurve::NLog => "Nikon N-Log",
            LogCurve::Log3G10 => "RED Log3G10",
        }
    }

    /// Scene-linear → signal (curve domain).
    pub fn encode(self, x: f64) -> f64 {
        match self {
            LogCurve::SLog3 => slog3_encode(x),
            LogCurve::VLog => vlog_encode(x),
            LogCurve::CLog2 => clog2_encode(x),
            LogCurve::CLog3 => clog3_encode(x),
            LogCurve::LogC3 => logc3_encode(x),
            LogCurve::LogC4 => logc4_encode(x),
            LogCurve::AppleLog => apple_log_encode(x),
            LogCurve::DLog => dlog_encode(x),
            LogCurve::NLog => nlog_encode(x),
            LogCurve::Log3G10 => log3g10_encode(x),
        }
    }

    /// Signal (curve domain) → scene-linear.
    pub fn decode(self, y: f64) -> f64 {
        match self {
            LogCurve::SLog3 => slog3_decode(y),
            LogCurve::VLog => vlog_decode(y),
            LogCurve::CLog2 => clog2_decode(y),
            LogCurve::CLog3 => clog3_decode(y),
            LogCurve::LogC3 => logc3_decode(y),
            LogCurve::LogC4 => logc4_decode(y),
            LogCurve::AppleLog => apple_log_decode(y),
            LogCurve::DLog => dlog_decode(y),
            LogCurve::NLog => nlog_decode(y),
            LogCurve::Log3G10 => log3g10_decode(y),
        }
    }

    /// Scene-linear value of the maximum signal (code 1023), used as the source peak for tone
    /// mapping.
    pub fn peak_linear(self) -> f64 {
        let top = match self.domain() {
            Domain::CodeValue => 1.0,
            Domain::Ire => (1023.0 - 64.0) / 876.0,
        };
        self.decode(top)
    }
}

/// Decoder-normalised value (0..1 in the file's range) → curve-domain signal.
pub fn signal_from_normalized(v: f64, range: Range, d: Domain) -> f64 {
    match (range, d) {
        (Range::Limited, Domain::CodeValue) => (64.0 + 876.0 * v) / 1023.0,
        (Range::Full, Domain::Ire) => (1023.0 * v - 64.0) / 876.0,
        _ => v,
    }
}

/// Curve-domain signal → decoder-normalised value (0..1 in the file's range).
pub fn normalized_from_signal(s: f64, range: Range, d: Domain) -> f64 {
    match (range, d) {
        (Range::Limited, Domain::CodeValue) => (1023.0 * s - 64.0) / 876.0,
        (Range::Full, Domain::Ire) => (64.0 + 876.0 * s) / 1023.0,
        _ => s,
    }
}

// ---------------------------------------------------------------- Sony S-Log3
// Sony, "Technical Summary for S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3" (2014).

pub fn slog3_encode(x: f64) -> f64 {
    if x >= 0.011_25 { (420.0 + ((x + 0.01) / (0.18 + 0.01)).log10() * 261.5) / 1023.0 } else { (x * (171.210_294_692_9 - 95.0) / 0.011_25 + 95.0) / 1023.0 }
}
pub fn slog3_decode(y: f64) -> f64 {
    if y >= 171.210_294_692_9 / 1023.0 {
        10f64.powf((y * 1023.0 - 420.0) / 261.5) * (0.18 + 0.01) - 0.01
    } else {
        (y * 1023.0 - 95.0) * 0.011_25 / (171.210_294_692_9 - 95.0)
    }
}

// ---------------------------------------------------------------- Panasonic V-Log
// Panasonic, "V-Log/V-Gamut Reference Manual" (rev. 1.0, 2014).

const VLOG_B: f64 = 0.008_73;
const VLOG_C: f64 = 0.241_514;
const VLOG_D: f64 = 0.598_206;

pub fn vlog_encode(x: f64) -> f64 {
    if x < 0.01 { 5.6 * x + 0.125 } else { VLOG_C * (x + VLOG_B).log10() + VLOG_D }
}
pub fn vlog_decode(y: f64) -> f64 {
    if y < 0.181 { (y - 0.125) / 5.6 } else { 10f64.powf((y - VLOG_D) / VLOG_C) - VLOG_B }
}

// ---------------------------------------------------------------- Canon Log 2 / 3
// Canon, "Canon Log Gamma Curves — Description of the Canon Log, Canon Log 2 and Canon Log 3"
// (white paper, 2018). The formulas take x = reflectance / 0.9 and give IRE.

pub fn clog2_encode(r: f64) -> f64 {
    let x = r / 0.9;
    if x < 0.0 { -0.241_360_77 * (-x * 87.099_375_46 + 1.0).log10() + 0.092_864_125 } else { 0.241_360_77 * (x * 87.099_375_46 + 1.0).log10() + 0.092_864_125 }
}
pub fn clog2_decode(y: f64) -> f64 {
    let x = if y < 0.092_864_125 {
        -(10f64.powf((0.092_864_125 - y) / 0.241_360_77) - 1.0) / 87.099_375_46
    } else {
        (10f64.powf((y - 0.092_864_125) / 0.241_360_77) - 1.0) / 87.099_375_46
    };
    x * 0.9
}

pub fn clog3_encode(r: f64) -> f64 {
    let x = r / 0.9;
    if x < -0.014 {
        -0.367_268_45 * (-x * 14.983_25 + 1.0).log10() + 0.127_839_01
    } else if x <= 0.014 {
        1.975_479_8 * x + 0.125_122_19
    } else {
        0.367_268_45 * (x * 14.983_25 + 1.0).log10() + 0.122_405_37
    }
}
pub fn clog3_decode(y: f64) -> f64 {
    let x = if y < 0.097_465_473 {
        -(10f64.powf((0.127_839_01 - y) / 0.367_268_45) - 1.0) / 14.983_25
    } else if y <= 0.152_778_91 {
        (y - 0.125_122_19) / 1.975_479_8
    } else {
        (10f64.powf((y - 0.122_405_37) / 0.367_268_45) - 1.0) / 14.983_25
    };
    x * 0.9
}

// ---------------------------------------------------------------- ARRI LogC3 (EI 800)
// ARRI, "ALEXA Log C Curve — Usage in VFX" (2017), table for EI 800, scene-linear variant.

const LOGC3: (f64, f64, f64, f64, f64, f64, f64) = (0.010_591, 5.555_556, 0.052_272, 0.247_190, 0.385_537, 5.367_655, 0.092_809);

pub fn logc3_encode(x: f64) -> f64 {
    let (cut, a, b, c, d, e, f) = LOGC3;
    if x > cut { c * (a * x + b).log10() + d } else { e * x + f }
}
pub fn logc3_decode(t: f64) -> f64 {
    let (cut, a, b, c, d, e, f) = LOGC3;
    if t > e * cut + f { (10f64.powf((t - d) / c) - b) / a } else { (t - f) / e }
}

// ---------------------------------------------------------------- ARRI LogC4
// ARRI, "ARRI LogC4 Logarithmic Color Space — Specification" (2022).

fn logc4_consts() -> (f64, f64, f64, f64, f64) {
    let a = (2f64.powi(18) - 16.0) / 117.45;
    let b = (1023.0 - 95.0) / 1023.0;
    let c = 95.0 / 1023.0;
    let s = (7.0 * std::f64::consts::LN_2 * 2f64.powf(7.0 - 14.0 * c / b)) / (a * b);
    let t = (2f64.powf(14.0 * (-c / b) + 6.0) - 64.0) / a;
    (a, b, c, s, t)
}
pub fn logc4_encode(x: f64) -> f64 {
    let (a, b, c, s, t) = logc4_consts();
    if x >= t { ((a * x + 64.0).log2() - 6.0) / 14.0 * b + c } else { (x - t) / s }
}
pub fn logc4_decode(y: f64) -> f64 {
    let (a, b, c, s, t) = logc4_consts();
    if y >= 0.0 { (2f64.powf(14.0 * (y - c) / b + 6.0) - 64.0) / a } else { y * s + t }
}

// ---------------------------------------------------------------- Apple Log
// Apple, "Apple Log Profile" white paper (2023).

const APPLE: (f64, f64, f64, f64, f64, f64) = (-0.056_410_88, 0.01, 47.287_112_36, 0.009_640_52, 0.085_504_79, 0.693_369_45);

pub fn apple_log_encode(r: f64) -> f64 {
    let (r0, rt, c, beta, gamma, delta) = APPLE;
    if r >= rt {
        gamma * (r + beta).log2() + delta
    } else if r >= r0 {
        c * (r - r0) * (r - r0)
    } else {
        0.0
    }
}
pub fn apple_log_decode(p: f64) -> f64 {
    let (r0, rt, c, beta, gamma, delta) = APPLE;
    let pt = c * (rt - r0) * (rt - r0);
    if p >= pt {
        2f64.powf((p - delta) / gamma) - beta
    } else if p >= 0.0 {
        (p / c).sqrt() + r0
    } else {
        r0
    }
}

// ---------------------------------------------------------------- DJI D-Log
// DJI, "White Paper on D-Log and D-Gamut of DJI Cinema Color System" (2017).

pub fn dlog_encode(x: f64) -> f64 {
    if x <= 0.0078 { 6.025 * x + 0.0929 } else { (x * 0.9892 + 0.0108).log10() * 0.256_663 + 0.584_555 }
}
pub fn dlog_decode(y: f64) -> f64 {
    if y <= 0.14 { (y - 0.0929) / 6.025 } else { (10f64.powf(3.896_16 * y - 2.277_52) - 0.0108) / 0.9892 }
}

// ---------------------------------------------------------------- Nikon N-Log
// Nikon, "N-Log Specification Document" version 1.0.0 (2018-09-01). Reflectance y (18 % grey =
// 0.18, "stop 0") to the 10-bit code value x:
//     y < 0.328:  x = 650 · (y + 0.0075)^(1/3)        else:  x = 150 · ln(y) + 619
// and back (the document's decode branch point is code 452):
//     x < 452:    y = (x / 650)^3 − 0.0075            else:  y = exp((x − 619) / 150)
// Colour gamut BT.2020 primaries, D65 white point. The document's two encode branches meet within
// 0.1 code value of each other at y = 0.328 and the decode branch point is rounded to code 452, so
// encode and decode are inverse to better than 4·10⁻⁴ in reflectance, not exactly (the seam test
// below pins that down). Both functions follow the document as written.

pub fn nlog_encode(x: f64) -> f64 {
    let code = if x < 0.328 { 650.0 * (x + 0.0075).cbrt() } else { 150.0 * x.ln() + 619.0 };
    code / 1023.0
}
pub fn nlog_decode(y: f64) -> f64 {
    let code = y * 1023.0;
    if code < 452.0 {
        let r = code / 650.0;
        r * r * r - 0.0075
    } else {
        ((code - 619.0) / 150.0).exp()
    }
}

// ---------------------------------------------------------------- RED Log3G10
// RED Digital Cinema, "White Paper on REDWideGamutRGB and Log3G10" (2017). Scene-linear x (18 %
// grey = 0.18 → 1/3, 10 stops above grey = 184.322 → 1.0), full-scale 0..1 signal. An offset c
// puts the toe at x = −0.01, and below that the curve continues as a straight line of gradient g
// (the slope of the log section at the toe).

const LOG3G10_A: f64 = 0.224_282;
const LOG3G10_B: f64 = 155.975_327;
const LOG3G10_C: f64 = 0.01;
const LOG3G10_G: f64 = 15.1927;

pub fn log3g10_encode(x: f64) -> f64 {
    let v = x + LOG3G10_C;
    if v < 0.0 { v * LOG3G10_G } else { LOG3G10_A * (v * LOG3G10_B + 1.0).log10() }
}
pub fn log3g10_decode(y: f64) -> f64 {
    if y < 0.0 { y / LOG3G10_G - LOG3G10_C } else { (10f64.powf(y / LOG3G10_A) - 1.0) / LOG3G10_B - LOG3G10_C }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_every_curve() {
        for c in LogCurve::ALL {
            for i in 0..=400 {
                // −1 % … 40× mid grey
                let x = -0.01 + (i as f64 / 400.0).powi(3) * 8.0;
                // Nikon's published N-Log branches do not meet exactly at 0.328 (see `nlog_seam_is_tiny`).
                if c == LogCurve::NLog && (0.327..0.330).contains(&x) {
                    continue;
                }
                let y = c.encode(x);
                let back = c.decode(y);
                assert!((back - x).abs() <= 1e-6 * x.abs().max(1.0) + 1e-9, "{c:?}: {x} → {y} → {back}");
            }
            // monotone signal
            let mut prev = f64::NEG_INFINITY;
            for i in 0..=1000 {
                let y = c.encode(i as f64 / 100.0);
                assert!(y > prev, "{c:?} not increasing at {i}");
                prev = y;
            }
        }
    }

    /// Reference points from the published specifications (code values are 10-bit).
    #[test]
    fn published_reference_values() {
        let cv = |c: LogCurve, x: f64| match c.domain() {
            Domain::CodeValue => c.encode(x) * 1023.0,
            Domain::Ire => 64.0 + c.encode(x) * 876.0,
        };
        let close = |a: f64, b: f64, tol: f64, what: &str| assert!((a - b).abs() <= tol, "{what}: {a} vs {b}");
        // Sony: 0 % → 95, 18 % → 420, 90 % → 598 (Technical Summary, table 1).
        close(cv(LogCurve::SLog3, 0.0), 95.0, 0.01, "S-Log3 0%");
        close(cv(LogCurve::SLog3, 0.18), 420.0, 0.01, "S-Log3 18%");
        close(cv(LogCurve::SLog3, 0.9), 598.0, 0.5, "S-Log3 90%");
        // Panasonic: 0 % → 128, 18 % → 433, 90 % → 602 (V-Log manual, table).
        close(cv(LogCurve::VLog, 0.0), 128.0, 0.5, "V-Log 0%");
        close(cv(LogCurve::VLog, 0.18), 433.0, 0.5, "V-Log 18%");
        close(cv(LogCurve::VLog, 0.9), 602.0, 0.5, "V-Log 90%");
        // ARRI LogC3 EI 800: 18 % → 0.391 (= code 400).
        close(LogCurve::LogC3.encode(0.18), 0.391, 0.0006, "LogC3 18%");
        close(cv(LogCurve::LogC3, 0.18), 400.0, 0.5, "LogC3 18% code");
        // ARRI LogC4: 18 % → 0.2784 (spec example), 0 → 95/1023.
        close(LogCurve::LogC4.encode(0.18), 0.278_4, 0.0005, "LogC4 18%");
        close(LogCurve::LogC4.encode(0.0), 95.0 / 1023.0, 1e-9, "LogC4 0");
        // Canon: 18 % grey → 39.2 % IRE (Log 2, ±0.7) and 34.3 % IRE (Log 3).
        close(LogCurve::CLog2.encode(0.18) * 100.0, 39.2, 0.7, "C-Log2 18%");
        close(LogCurve::CLog3.encode(0.18) * 100.0, 34.3, 0.2, "C-Log3 18%");
        // Apple Log: 18 % grey → 0.488 (white paper figure), black 0 → R0 floor.
        close(LogCurve::AppleLog.encode(0.18), 0.488, 0.001, "Apple Log 18%");
        close(LogCurve::AppleLog.encode(-0.056_410_88), 0.0, 1e-12, "Apple Log R0");
        // DJI D-Log: 0 → 0.0929, 18 % → 0.398 (white paper).
        close(LogCurve::DLog.encode(0.0), 0.0929, 1e-9, "D-Log 0");
        close(LogCurve::DLog.encode(0.18), 0.398, 0.002, "D-Log 18%");
        // Nikon N-Log: 18 % grey → code 372 (650·(0.1875)^(1/3) = 372.03, about 36 % of full scale),
        // black 0 → 650·0.0075^(1/3) = 127.2, and the log branch passes through 150·ln(1)+619 = 619
        // at reflectance 1.0.
        close(cv(LogCurve::NLog, 0.18), 372.03, 0.01, "N-Log 18%");
        close(cv(LogCurve::NLog, 0.0), 127.2, 0.1, "N-Log 0");
        close(cv(LogCurve::NLog, 1.0), 619.0, 1e-9, "N-Log 100%");
        // RED Log3G10 (white paper table): −0.01 → 0, 18 % grey → 1/3, 184.322 → 1.0.
        close(LogCurve::Log3G10.encode(-0.01), 0.0, 1e-12, "Log3G10 toe");
        close(LogCurve::Log3G10.encode(0.18), 1.0 / 3.0, 2e-6, "Log3G10 18%");
        close(LogCurve::Log3G10.encode(184.322), 1.0, 2e-6, "Log3G10 white");
        close(LogCurve::Log3G10.peak_linear(), 184.322, 0.01, "Log3G10 peak");
    }

    /// Nikon's published N-Log encode and decode are not exact inverses at the seam (the document
    /// rounds the decode branch point to code 452): bound how far apart they can be.
    #[test]
    fn nlog_seam_is_tiny() {
        let mut worst = 0.0f64;
        for i in 0..=2000 {
            let x = 0.320 + f64::from(i) * 1e-5; // 0.320 … 0.340
            let back = LogCurve::NLog.decode(LogCurve::NLog.encode(x));
            worst = worst.max((back - x).abs());
        }
        assert!(worst < 4e-4, "N-Log seam error {worst}");
        // The signal stays strictly increasing across the seam.
        let mut prev = f64::NEG_INFINITY;
        for i in 0..=2000 {
            let y = LogCurve::NLog.encode(0.320 + f64::from(i) * 1e-5);
            assert!(y > prev, "N-Log encode not increasing at {i}");
            prev = y;
        }
    }

    /// Hostile inputs give finite or infinite numbers, never a panic or NaN.
    #[test]
    fn new_curves_survive_hostile_values() {
        for c in [LogCurve::NLog, LogCurve::Log3G10] {
            for v in [0.0, -0.0, 1e-300, -1e-300, 1e300, -1e300, f64::MAX, f64::MIN, f64::INFINITY, f64::NEG_INFINITY] {
                assert!(!c.encode(v).is_nan(), "{c:?} encode({v}) is NaN");
                assert!(!c.decode(v).is_nan(), "{c:?} decode({v}) is NaN");
            }
        }
    }

    #[test]
    fn domain_conversion_roundtrip() {
        for r in [Range::Limited, Range::Full] {
            for d in [Domain::CodeValue, Domain::Ire] {
                for v in [0.0, 0.25, 0.8, 1.0] {
                    assert!((normalized_from_signal(signal_from_normalized(v, r, d), r, d) - v).abs() < 1e-12);
                }
            }
        }
        // limited-range code 420 is S-Log3 mid grey
        let norm = (420.0 - 64.0) / 876.0;
        let s = signal_from_normalized(norm, Range::Limited, Domain::CodeValue);
        assert!((LogCurve::SLog3.decode(s) - 0.18).abs() < 1e-6);
    }
}
