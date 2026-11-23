//! The Speed Editor's USB HID protocol: key codes, LED bits, report layouts and the
//! authentication handshake. Pure functions over byte slices, no I/O.
//!
//! Ported from Sylvain Munaut's reverse-engineered description (`bmd.py` in
//! <https://github.com/smunaut/blackmagic-misc>, Copyright (C) 2021 Sylvain Munaut,
//! Apache-2.0; see the crate README and the workspace `NOTICE`).
//!
//! Reports used:
//! - input 3: jog wheel (`mode: u8`, `value: i32 LE`, one unknown byte);
//! - input 4: up to six held key codes (`u16 LE` each, 0 = empty slot); a new report every
//!   time the set of held keys changes, no auto-repeat;
//! - input 7: battery (`charging: u8`, `level: u8` 0–100);
//! - output 2: key LEDs (`u32 LE` bitfield); output 3: jog mode; output 4: jog-mode LEDs (`u8`);
//! - feature 6: the authentication handshake. Until it completes the device sends no reports.

/// Blackmagic Design's USB vendor id.
pub const VENDOR_ID: u16 = 0x1edb;
/// DaVinci Resolve Speed Editor's USB product id.
pub const PRODUCT_ID: u16 = 0xda0e;

/// Length of every feature report 6 exchanged during authentication (report id included).
pub const AUTH_REPORT_LEN: usize = 10;
/// Largest input report the device sends (we read into a buffer of this size).
pub const MAX_INPUT_REPORT: usize = 64;
/// Most keys the device reports as held at once.
pub const MAX_HELD_KEYS: usize = 6;

macro_rules! keys {
    ($($name:ident = $code:literal, $label:literal, $led:expr;)*) => {
        /// A key of the Speed Editor, named after its printed label.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Key {
            $(#[doc = $label] $name,)*
        }

        impl Key {
            /// Every key, in key-code order of the table.
            pub const ALL: &'static [Key] = &[$(Key::$name,)*];

            /// The key for a code from input report 4 (`None` for 0 and unknown codes).
            pub fn from_code(code: u16) -> Option<Key> {
                match code {
                    $($code => Some(Key::$name),)*
                    _ => None,
                }
            }

            /// The code the device reports for this key.
            pub fn code(self) -> u16 {
                match self {
                    $(Key::$name => $code,)*
                }
            }

            /// The label printed on the key.
            pub fn label(self) -> &'static str {
                match self {
                    $(Key::$name => $label,)*
                }
            }

            /// The bit of the key's LED in output report 2, if the key has one.
            pub fn led(self) -> Option<u32> {
                match self {
                    $(Key::$name => $led,)*
                }
            }
        }
    };
}

keys! {
    SmartInsert = 0x01, "SMART INSRT", None;
    Append = 0x02, "APPND", None;
    RippleOverwrite = 0x03, "RIPL O/WR", None;
    CloseUp = 0x04, "CLOSE UP", Some(led::CLOSE_UP);
    PlaceOnTop = 0x05, "PLACE ON TOP", None;
    SourceOverwrite = 0x06, "SRC O/WR", None;
    In = 0x07, "IN", None;
    Out = 0x08, "OUT", None;
    TrimIn = 0x09, "TRIM IN", None;
    TrimOut = 0x0a, "TRIM OUT", None;
    Roll = 0x0b, "ROLL", None;
    SlipSource = 0x0c, "SLIP SRC", None;
    SlipDest = 0x0d, "SLIP DEST", None;
    TransitionDuration = 0x0e, "TRANS DUR", None;
    Cut = 0x0f, "CUT", Some(led::CUT);
    Dissolve = 0x10, "DIS", Some(led::DIS);
    SmoothCut = 0x11, "SMTH CUT", Some(led::SMTH_CUT);
    Source = 0x1a, "SOURCE", None;
    Timeline = 0x1b, "TIMELINE", None;
    Shuttle = 0x1c, "SHTL", None;
    Jog = 0x1d, "JOG", None;
    Scroll = 0x1e, "SCRL", None;
    SyncBin = 0x1f, "SYNC BIN", None;
    Transition = 0x22, "TRANS", Some(led::TRANS);
    VideoOnly = 0x25, "VIDEO ONLY", Some(led::VIDEO_ONLY);
    AudioOnly = 0x26, "AUDIO ONLY", Some(led::AUDIO_ONLY);
    RippleDelete = 0x2b, "RIPL DEL", None;
    AudioLevel = 0x2c, "AUDIO LEVEL", None;
    FullView = 0x2d, "FULL VIEW", None;
    Snap = 0x2e, "SNAP", Some(led::SNAP);
    Split = 0x2f, "SPLIT", None;
    LiveOverwrite = 0x30, "LIVE O/WR", Some(led::LIVE_OWR);
    Escape = 0x31, "ESC", None;
    Cam1 = 0x33, "CAM1", Some(led::CAM1);
    Cam2 = 0x34, "CAM2", Some(led::CAM2);
    Cam3 = 0x35, "CAM3", Some(led::CAM3);
    Cam4 = 0x36, "CAM4", Some(led::CAM4);
    Cam5 = 0x37, "CAM5", Some(led::CAM5);
    Cam6 = 0x38, "CAM6", Some(led::CAM6);
    Cam7 = 0x39, "CAM7", Some(led::CAM7);
    Cam8 = 0x3a, "CAM8", Some(led::CAM8);
    Cam9 = 0x3b, "CAM9", Some(led::CAM9);
    StopPlay = 0x3c, "STOP/PLAY", None;
}

/// Key LED bits of output report 2.
pub mod led {
    pub const CLOSE_UP: u32 = 1 << 0;
    pub const CUT: u32 = 1 << 1;
    pub const DIS: u32 = 1 << 2;
    pub const SMTH_CUT: u32 = 1 << 3;
    pub const TRANS: u32 = 1 << 4;
    pub const SNAP: u32 = 1 << 5;
    pub const CAM7: u32 = 1 << 6;
    pub const CAM8: u32 = 1 << 7;
    pub const CAM9: u32 = 1 << 8;
    pub const LIVE_OWR: u32 = 1 << 9;
    pub const CAM4: u32 = 1 << 10;
    pub const CAM5: u32 = 1 << 11;
    pub const CAM6: u32 = 1 << 12;
    pub const VIDEO_ONLY: u32 = 1 << 13;
    pub const CAM1: u32 = 1 << 14;
    pub const CAM2: u32 = 1 << 15;
    pub const CAM3: u32 = 1 << 16;
    pub const AUDIO_ONLY: u32 = 1 << 17;
    /// Every defined bit.
    pub const ALL: u32 = (1 << 18) - 1;
}

/// Jog-mode LED bits of output report 4.
pub mod jog_led {
    pub const JOG: u8 = 1 << 0;
    pub const SHTL: u8 = 1 << 1;
    pub const SCRL: u8 = 1 << 2;
}

/// How the jog wheel reports its position (output report 3, echoed in input report 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JogMode {
    /// Each report carries the movement since the previous one.
    Relative,
    /// The position since the mode was set, about −4096…4096 over half a turn either way.
    AbsoluteContinuous,
    /// Reported as relative, like [`JogMode::Relative`].
    Relative2,
    /// As [`JogMode::AbsoluteContinuous`] with a small dead band around zero.
    AbsoluteDeadZero,
}

impl JogMode {
    pub fn from_byte(b: u8) -> Option<JogMode> {
        Some(match b {
            0 => JogMode::Relative,
            1 => JogMode::AbsoluteContinuous,
            2 => JogMode::Relative2,
            3 => JogMode::AbsoluteDeadZero,
            _ => return None,
        })
    }

    pub fn byte(self) -> u8 {
        match self {
            JogMode::Relative => 0,
            JogMode::AbsoluteContinuous => 1,
            JogMode::Relative2 => 2,
            JogMode::AbsoluteDeadZero => 3,
        }
    }

    /// Whether values are positions (as opposed to movements since the last report).
    pub fn is_absolute(self) -> bool {
        matches!(self, JogMode::AbsoluteContinuous | JogMode::AbsoluteDeadZero)
    }
}

/// One parsed input report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// Jog wheel movement or position, by mode. An unknown mode byte is kept as `None`.
    Jog { mode: Option<JogMode>, value: i32 },
    /// The keys held now, in report order, without empty slots or unknown codes.
    Keys(Vec<Key>),
    /// Battery state (Bluetooth models report it over USB too).
    Battery { charging: bool, level: u8 },
}

/// Parse an input report (first byte: report id). Short, unknown or malformed reports give
/// `None`; nothing here can panic on device data.
pub fn parse_report(data: &[u8]) -> Option<Report> {
    let (&id, rest) = data.split_first()?;
    match id {
        3 => {
            let mode = *rest.first()?;
            let value = rest.get(1..5)?;
            let value = i32::from_le_bytes([*value.first()?, *value.get(1)?, *value.get(2)?, *value.get(3)?]);
            Some(Report::Jog { mode: JogMode::from_byte(mode), value })
        }
        4 => {
            let slots = rest.get(..MAX_HELD_KEYS * 2)?;
            let mut keys = Vec::with_capacity(MAX_HELD_KEYS);
            for pair in slots.as_chunks::<2>().0 {
                let code = u16::from_le_bytes(*pair);
                if let Some(k) = Key::from_code(code)
                    && !keys.contains(&k)
                {
                    keys.push(k);
                }
            }
            Some(Report::Keys(keys))
        }
        7 => {
            let charging = *rest.first()? != 0;
            let level = (*rest.get(1)?).min(100);
            Some(Report::Battery { charging, level })
        }
        _ => None,
    }
}

/// Output report 2: light the key LEDs whose bits are set.
pub fn led_report(bits: u32) -> [u8; 5] {
    let b = (bits & led::ALL).to_le_bytes();
    [2, b[0], b[1], b[2], b[3]]
}

/// Output report 4: light the jog-mode LEDs whose bits are set.
pub fn jog_led_report(bits: u8) -> [u8; 2] {
    [4, bits & (jog_led::JOG | jog_led::SHTL | jog_led::SCRL)]
}

/// Output report 3: set the jog wheel's reporting mode.
pub fn jog_mode_report(mode: JogMode) -> [u8; 7] {
    // mode, a 32-bit zero (the reference position), and an unknown byte the reference sends as 255
    [3, mode.byte(), 0, 0, 0, 0, 255]
}

/// Steps of the feature-report-6 handshake. The device challenges the host; the host answers
/// with [`auth_response`]. The host's own challenge to the device is sent as zero and the
/// device's answer is not checked: we only need the device to start reporting.
pub mod auth {
    use super::AUTH_REPORT_LEN;

    /// Host → device: reset the handshake.
    pub const RESET: [u8; AUTH_REPORT_LEN] = [6, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    /// Host → device: the host's challenge (zero).
    pub const HOST_CHALLENGE: [u8; AUTH_REPORT_LEN] = [6, 1, 0, 0, 0, 0, 0, 0, 0, 0];

    /// Host → device: our response to the device's challenge.
    pub fn reply(response: u64) -> [u8; AUTH_REPORT_LEN] {
        let r = response.to_le_bytes();
        [6, 3, r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]
    }

    /// The 8-byte payload of a feature report the device returned at handshake step `step`
    /// (0: its challenge, 2: its response, 4: the status), or why it is not one.
    pub fn payload(data: &[u8], step: u8) -> Result<u64, String> {
        match (data.first(), data.get(1)) {
            (Some(6), Some(s)) if *s == step => {}
            _ => return Err(format!("Speed Editor authentication failed at step {step}: unexpected reply {:02x?}", data.get(..2).unwrap_or(data))),
        }
        let p = data.get(2..AUTH_REPORT_LEN).ok_or_else(|| format!("Speed Editor authentication failed at step {step}: short reply ({} bytes)", data.len()))?;
        let mut b = [0u8; 8];
        b.copy_from_slice(p);
        Ok(u64::from_le_bytes(b))
    }

    /// Seconds until the device wants a new handshake, from the status payload (step 4). Its
    /// low 16 bits; 0 means unknown.
    pub fn timeout_secs(status: u64) -> u16 {
        (status & 0xffff) as u16
    }
}

fn rol8(v: u64) -> u64 {
    v.rotate_left(56)
}

/// The host's response to the device's authentication challenge.
pub fn auth_response(challenge: u64) -> u64 {
    const EVEN: [u64; 8] = [
        0x3ae1206f97c10bc8,
        0x2a9ab32bebf244c6,
        0x20a6f8b8df9adf0a,
        0xaf80ece52cfc1719,
        0xec2ee2f7414fd151,
        0xb055adfd73344a15,
        0xa63d2e3059001187,
        0x751bf623f42e0dde,
    ];
    const ODD: [u64; 8] = [
        0x3e22b34f502e7fde,
        0x24656b981875ab1c,
        0xa17f3456df7bf8c3,
        0x6df72e1941aef698,
        0x72226f011e66ab94,
        0x3831a3c606296b42,
        0xfd7ff81881332c89,
        0x61a3f6474ff236c6,
    ];
    const MASK: u64 = 0xa79a63f585d37bf0;
    let n = (challenge & 7) as usize;
    // rol8 applied n times is a rotation by 56·n bits
    let mut v = challenge.rotate_left(((56 * n) % 64) as u32);
    let k = if (v & 1) == ((0x78u64 >> n) & 1) {
        EVEN.get(n).copied().unwrap_or(0)
    } else {
        v ^= rol8(v);
        ODD.get(n).copied().unwrap_or(0)
    };
    v ^ (rol8(v) & MASK) ^ k
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straight transcription of the reference algorithm (loops and all), to check the
    /// rotation shortcut in [`auth_response`]. Kept in the reference's own shape on purpose.
    #[allow(clippy::identity_op, clippy::manual_rotate, clippy::needless_late_init)]
    fn reference(challenge: u64) -> u64 {
        fn rol8(v: u64) -> u64 {
            ((v << 56) | (v >> 8)) & 0xffff_ffff_ffff_ffff
        }
        let even = [
            0x3ae1206f97c10bc8u64,
            0x2a9ab32bebf244c6,
            0x20a6f8b8df9adf0a,
            0xaf80ece52cfc1719,
            0xec2ee2f7414fd151,
            0xb055adfd73344a15,
            0xa63d2e3059001187,
            0x751bf623f42e0dde,
        ];
        let odd = [
            0x3e22b34f502e7fdeu64,
            0x24656b981875ab1c,
            0xa17f3456df7bf8c3,
            0x6df72e1941aef698,
            0x72226f011e66ab94,
            0x3831a3c606296b42,
            0xfd7ff81881332c89,
            0x61a3f6474ff236c6,
        ];
        let n = (challenge & 7) as usize;
        let mut v = challenge;
        for _ in 0..n {
            v = rol8(v);
        }
        let k;
        if (v & 1) == ((0x78 >> n) & 1) {
            k = even[n];
        } else {
            v ^= rol8(v);
            k = odd[n];
        }
        v ^ (rol8(v) & 0xa79a63f585d37bf0) ^ k
    }

    #[test]
    fn auth_matches_reference_for_every_rotation_and_parity() {
        let mut x: u64 = 0x9e3779b97f4a7c15;
        for i in 0..4096u64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let c = (x & !7) | (i & 7);
            assert_eq!(auth_response(c), reference(c), "challenge {c:#x}");
        }
        for c in [0, 1, 7, u64::MAX, 0x8000_0000_0000_0000] {
            assert_eq!(auth_response(c), reference(c));
        }
    }

    #[test]
    fn key_codes_round_trip_and_are_unique() {
        let mut codes: Vec<u16> = Key::ALL.iter().map(|k| k.code()).collect();
        for k in Key::ALL {
            assert_eq!(Key::from_code(k.code()), Some(*k));
        }
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), Key::ALL.len());
        assert_eq!(Key::from_code(0), None);
        assert_eq!(Key::from_code(0xffff), None);
    }

    #[test]
    fn led_bits_are_distinct() {
        let mut seen = 0u32;
        for k in Key::ALL {
            if let Some(b) = k.led() {
                assert_eq!(seen & b, 0, "{k:?} shares an LED bit");
                seen |= b;
            }
        }
        assert_eq!(seen, led::ALL);
    }

    #[test]
    fn parses_keys_report() {
        let r = parse_report(&[4, 0x07, 0, 0x3c, 0, 0, 0, 0x07, 0, 0x99, 0x99, 0, 0]);
        assert_eq!(r, Some(Report::Keys(vec![Key::In, Key::StopPlay])));
        assert_eq!(parse_report(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Some(Report::Keys(vec![])));
    }

    #[test]
    fn parses_jog_and_battery_reports() {
        assert_eq!(parse_report(&[3, 0, 0xfe, 0xff, 0xff, 0xff, 0]), Some(Report::Jog { mode: Some(JogMode::Relative), value: -2 }));
        assert_eq!(parse_report(&[3, 9, 1, 0, 0, 0]), Some(Report::Jog { mode: None, value: 1 }));
        assert_eq!(parse_report(&[7, 1, 250]), Some(Report::Battery { charging: true, level: 100 }));
    }

    #[test]
    fn hostile_reports_never_panic() {
        assert_eq!(parse_report(&[]), None);
        for id in 0..=255u8 {
            for len in 0..20 {
                let mut v = vec![0xffu8; len];
                if let Some(first) = v.first_mut() {
                    *first = id;
                }
                let _ = parse_report(&v);
            }
        }
        assert_eq!(parse_report(&[4, 1, 0, 2]), None, "truncated key report");
        assert_eq!(parse_report(&[3, 0, 1, 2]), None, "truncated jog report");
    }

    #[test]
    fn output_reports() {
        assert_eq!(led_report(led::SNAP | led::CAM1), [2, 0x20, 0x40, 0, 0]);
        assert_eq!(led_report(u32::MAX), led_report(led::ALL));
        assert_eq!(jog_led_report(0xff), [4, 7]);
        assert_eq!(jog_mode_report(JogMode::AbsoluteDeadZero), [3, 3, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn auth_payloads() {
        let reply = auth::reply(0x0102030405060708);
        assert_eq!(reply, [6, 3, 8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(auth::payload(&[6, 0, 8, 7, 6, 5, 4, 3, 2, 1], 0), Ok(0x0102030405060708));
        assert!(auth::payload(&[6, 2, 0, 0, 0, 0, 0, 0, 0, 0], 0).is_err());
        assert!(auth::payload(&[6, 4, 0], 4).is_err());
        assert!(auth::payload(&[], 0).is_err());
        assert_eq!(auth::timeout_secs(auth::payload(&[6, 4, 0x58, 0x02, 0, 0, 0, 0, 0, 0], 4).unwrap_or(0)), 600);
    }
}
