//! What the editor does with the Speed Editor: a pure state machine from parsed reports to
//! [`Action`]s (command ids, shuttle speeds, frame steps), and the LED / jog-mode state the
//! device should show. No I/O and no clocks of its own: the caller passes the time.

use crate::protocol::{JogMode, Key, Report, jog_led, led};

/// What the jog wheel does, chosen with the SHTL / JOG / SCRL keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WheelMode {
    /// Turning away from where the mode was entered plays at a speed set by how far it was
    /// turned, either way; back to the middle stops.
    #[default]
    Shuttle,
    /// Each detent-sized movement steps one frame.
    Jog,
    /// As jog, many frames per turn, for moving through a long timeline.
    Scroll,
}

impl WheelMode {
    /// The device mode that reports what this wheel mode needs.
    pub fn device_mode(self) -> JogMode {
        match self {
            WheelMode::Shuttle => JogMode::AbsoluteDeadZero,
            WheelMode::Jog | WheelMode::Scroll => JogMode::Relative,
        }
    }

    fn led(self) -> u8 {
        match self {
            WheelMode::Shuttle => jog_led::SHTL,
            WheelMode::Jog => jog_led::JOG,
            WheelMode::Scroll => jog_led::SCRL,
        }
    }
}

/// Something for the editor to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run the command with this id (UI or engine), no parameters.
    Invoke(&'static str),
    /// Play at this speed (negative: reverse); `0.0` stops.
    Shuttle(f64),
    /// Move the playhead (or, in trim mode, the selected edit points) by this many frames.
    Step(i64),
}

/// Tuning of the wheel. Units are the device's: about 4096 per half turn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// Wheel units per frame in jog mode.
    pub jog_units_per_frame: i32,
    /// Wheel units per frame in scroll mode.
    pub scroll_units_per_frame: i32,
    /// Absolute shuttle position past which each next speed starts; below the first, stopped.
    pub shuttle_thresholds: [i32; 6],
    /// Speeds (forward; mirrored for reverse) for each threshold passed.
    pub shuttle_speeds: [f64; 6],
    /// Most frames one report may step (a hostile or glitched value cannot run away).
    pub max_step: i64,
    /// Seconds within which a second press of IN / OUT clears the point instead.
    pub double_press: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            jog_units_per_frame: 256,
            scroll_units_per_frame: 32,
            shuttle_thresholds: [200, 800, 1400, 2000, 2700, 3400],
            shuttle_speeds: [0.25, 0.5, 1.0, 2.0, 4.0, 8.0],
            max_step: 600,
            double_press: 0.4,
        }
    }
}

/// The key → command map. Keys not listed do nothing yet (see the crate README).
pub const KEY_COMMANDS: &[(Key, &str)] = &[
    (Key::SmartInsert, "source.insert"),
    (Key::SourceOverwrite, "source.overwrite"),
    (Key::In, "markers.markIn"),
    (Key::Out, "markers.markOut"),
    (Key::TrimIn, "trim.ripplePrevious"),
    (Key::TrimOut, "trim.rippleNext"),
    (Key::Roll, "tool.rolling"),
    (Key::SlipSource, "tool.slip"),
    (Key::SlipDest, "tool.slide"),
    (Key::Cut, "sequence.removeTransition"),
    (Key::Dissolve, "sequence.applyVideoTransition"),
    (Key::Split, "sequence.addEdit"),
    (Key::RippleDelete, "edit.rippleDelete"),
    (Key::Snap, "sequence.snap"),
    (Key::Escape, "edit.undo"),
    (Key::AudioLevel, "markers.add"),
    (Key::FullView, "window.maximizeFrame"),
    (Key::Source, "window.panel.Source"),
    (Key::Timeline, "window.panel.Timeline"),
    (Key::Cam1, "multicam.selectCamera1"),
    (Key::Cam2, "multicam.selectCamera2"),
    (Key::Cam3, "multicam.selectCamera3"),
    (Key::Cam4, "multicam.selectCamera4"),
    (Key::Cam5, "multicam.selectCamera5"),
    (Key::Cam6, "multicam.selectCamera6"),
    (Key::Cam7, "multicam.selectCamera7"),
    (Key::Cam8, "multicam.selectCamera8"),
    (Key::Cam9, "multicam.selectCamera9"),
    (Key::StopPlay, "playback.toggle"),
];

/// Commands a quick second press runs instead (the printed `[CLR]` of IN and OUT).
pub const DOUBLE_PRESS_COMMANDS: &[(Key, &str)] = &[(Key::In, "markers.clearIn"), (Key::Out, "markers.clearOut")];

/// Every command id the controller can emit, for checking against the registry.
pub fn command_ids() -> impl Iterator<Item = &'static str> {
    KEY_COMMANDS.iter().chain(DOUBLE_PRESS_COMMANDS).map(|(_, id)| *id)
}

/// The command a key runs, if any.
pub fn command_for(key: Key) -> Option<&'static str> {
    KEY_COMMANDS.iter().find(|(k, _)| *k == key).map(|(_, id)| *id)
}

fn double_press_command(key: Key) -> Option<&'static str> {
    DOUBLE_PRESS_COMMANDS.iter().find(|(k, _)| *k == key).map(|(_, id)| *id)
}

/// Editor state the LEDs show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lights {
    pub snap: bool,
}

/// What the device should show and how its wheel should report. The device thread writes
/// whatever differs from what it last wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceState {
    pub leds: u32,
    pub jog_leds: u8,
    pub jog_mode: JogMode,
    /// Bumped to make the device re-zero an absolute wheel position (by setting the mode again).
    pub jog_epoch: u32,
}

/// The Speed Editor's behaviour.
#[derive(Debug, Default)]
pub struct Controller {
    pub config: Config,
    held: Vec<Key>,
    wheel: WheelMode,
    jog_accum: i32,
    /// Signed index into `shuttle_speeds` (+1 based); 0 is stopped.
    shuttle_level: i32,
    jog_epoch: u32,
    last_press: Option<(Key, f64)>,
    battery: Option<(bool, u8)>,
}

impl Controller {
    pub fn new(config: Config) -> Self {
        Controller { config, ..Default::default() }
    }

    pub fn wheel(&self) -> WheelMode {
        self.wheel
    }

    /// Keys held now.
    pub fn held(&self) -> &[Key] {
        &self.held
    }

    /// Last battery report: (charging, percent).
    pub fn battery(&self) -> Option<(bool, u8)> {
        self.battery
    }

    /// Whether the wheel is currently driving shuttle playback.
    pub fn shuttling(&self) -> bool {
        self.shuttle_level != 0
    }

    /// Forget held keys and wheel motion (the device went away or came back).
    pub fn reset(&mut self) {
        self.held.clear();
        self.jog_accum = 0;
        self.shuttle_level = 0;
        self.last_press = None;
        self.jog_epoch = self.jog_epoch.wrapping_add(1);
    }

    /// Playback was stopped or started by something other than the wheel (a key, the mouse):
    /// shuttle starts again from the wheel's current position.
    pub fn playback_taken_over(&mut self) {
        if self.shuttle_level != 0 {
            self.shuttle_level = 0;
            self.jog_epoch = self.jog_epoch.wrapping_add(1);
        }
    }

    /// The device state for the current mode and editor state.
    pub fn device_state(&self, lights: Lights) -> DeviceState {
        let mut leds = 0;
        if lights.snap {
            leds |= led::SNAP;
        }
        DeviceState { leds, jog_leds: self.wheel.led(), jog_mode: self.wheel.device_mode(), jog_epoch: self.jog_epoch }
    }

    /// Handle one report at time `now` (seconds, any monotonic origin).
    pub fn handle(&mut self, report: &Report, now: f64) -> Vec<Action> {
        match report {
            Report::Keys(keys) => self.keys(keys, now),
            Report::Jog { mode, value } => self.jog(*mode, *value),
            Report::Battery { charging, level } => {
                self.battery = Some((*charging, *level));
                Vec::new()
            }
        }
    }

    fn keys(&mut self, keys: &[Key], now: f64) -> Vec<Action> {
        let mut out = Vec::new();
        let pressed: Vec<Key> = keys.iter().copied().filter(|k| !self.held.contains(k)).collect();
        self.held = keys.to_vec();
        for key in pressed {
            let mode = match key {
                Key::Shuttle => Some(WheelMode::Shuttle),
                Key::Jog => Some(WheelMode::Jog),
                Key::Scroll => Some(WheelMode::Scroll),
                _ => None,
            };
            if let Some(mode) = mode {
                if self.shuttle_level != 0 {
                    out.push(Action::Shuttle(0.0));
                }
                self.wheel = mode;
                self.jog_accum = 0;
                self.shuttle_level = 0;
                self.jog_epoch = self.jog_epoch.wrapping_add(1);
                self.last_press = None;
                continue;
            }
            let quick_repeat = self.last_press.is_some_and(|(k, t)| k == key && now >= t && now - t <= self.config.double_press);
            let id = match double_press_command(key) {
                Some(clear) if quick_repeat => {
                    self.last_press = None;
                    Some(clear)
                }
                _ => {
                    self.last_press = Some((key, now));
                    command_for(key)
                }
            };
            if key == Key::StopPlay {
                self.playback_taken_over();
            }
            if let Some(id) = id {
                out.push(Action::Invoke(id));
            }
        }
        out
    }

    fn jog(&mut self, mode: Option<JogMode>, value: i32) -> Vec<Action> {
        // Reports from before the last mode change still arrive in the old mode: ignore them.
        let Some(mode) = mode else { return Vec::new() };
        if mode.is_absolute() != self.wheel.device_mode().is_absolute() {
            return Vec::new();
        }
        match self.wheel {
            WheelMode::Shuttle => {
                let level = self.shuttle_level_for(value);
                if level == self.shuttle_level {
                    return Vec::new();
                }
                self.shuttle_level = level;
                vec![Action::Shuttle(self.speed_of(level))]
            }
            WheelMode::Jog | WheelMode::Scroll => {
                let per = (if self.wheel == WheelMode::Jog { self.config.jog_units_per_frame } else { self.config.scroll_units_per_frame }).max(1);
                self.jog_accum = self.jog_accum.saturating_add(value);
                let frames = self.jog_accum / per;
                if frames == 0 {
                    return Vec::new();
                }
                self.jog_accum -= frames.saturating_mul(per);
                let max = self.config.max_step.max(1);
                vec![Action::Step(i64::from(frames).clamp(-max, max))]
            }
        }
    }

    fn shuttle_level_for(&self, value: i32) -> i32 {
        let magnitude = value.saturating_abs();
        let passed = self.config.shuttle_thresholds.iter().take_while(|t| magnitude >= **t).count() as i32;
        if value < 0 { -passed } else { passed }
    }

    fn speed_of(&self, level: i32) -> f64 {
        if level == 0 {
            return 0.0;
        }
        let i = usize::try_from(level.unsigned_abs().saturating_sub(1)).unwrap_or(0);
        let speeds = &self.config.shuttle_speeds;
        let speed = speeds.get(i).or(speeds.last()).copied().filter(|s| s.is_finite()).unwrap_or(1.0);
        if level < 0 { -speed } else { speed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(c: &mut Controller, k: &[Key], now: f64) -> Vec<Action> {
        c.handle(&Report::Keys(k.to_vec()), now)
    }

    #[test]
    fn a_press_runs_its_command_once() {
        let mut c = Controller::default();
        assert_eq!(keys(&mut c, &[Key::Split], 0.0), vec![Action::Invoke("sequence.addEdit")]);
        // still held: nothing; a second key pressed while holding: only the new one
        assert_eq!(keys(&mut c, &[Key::Split], 0.1), vec![]);
        assert_eq!(keys(&mut c, &[Key::Split, Key::Snap], 0.2), vec![Action::Invoke("sequence.snap")]);
        assert_eq!(keys(&mut c, &[], 0.3), vec![]);
        assert_eq!(keys(&mut c, &[Key::Split], 0.4), vec![Action::Invoke("sequence.addEdit")]);
    }

    #[test]
    fn unmapped_keys_do_nothing() {
        let mut c = Controller::default();
        assert_eq!(keys(&mut c, &[Key::PlaceOnTop], 0.0), vec![]);
    }

    #[test]
    fn double_press_clears_in_and_out() {
        let mut c = Controller::default();
        assert_eq!(keys(&mut c, &[Key::In], 0.0), vec![Action::Invoke("markers.markIn")]);
        keys(&mut c, &[], 0.1);
        assert_eq!(keys(&mut c, &[Key::In], 0.2), vec![Action::Invoke("markers.clearIn")]);
        keys(&mut c, &[], 0.25);
        // a third quick press marks again; a slow second press marks too
        assert_eq!(keys(&mut c, &[Key::In], 0.3), vec![Action::Invoke("markers.markIn")]);
        keys(&mut c, &[], 0.4);
        assert_eq!(keys(&mut c, &[Key::Out], 0.5), vec![Action::Invoke("markers.markOut")]);
        keys(&mut c, &[], 0.6);
        assert_eq!(keys(&mut c, &[Key::Out], 2.0), vec![Action::Invoke("markers.markOut")]);
    }

    #[test]
    fn shuttle_speeds_follow_the_wheel_and_stop_at_centre() {
        let mut c = Controller::default();
        let jog = |c: &mut Controller, v| c.handle(&Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: v }, 0.0);
        assert_eq!(jog(&mut c, 50), vec![]);
        assert_eq!(jog(&mut c, 900), vec![Action::Shuttle(0.5)]);
        assert_eq!(jog(&mut c, 1000), vec![], "same speed band: no new action");
        assert_eq!(jog(&mut c, 4096), vec![Action::Shuttle(8.0)]);
        assert_eq!(jog(&mut c, i32::MAX), vec![]);
        assert_eq!(jog(&mut c, 0), vec![Action::Shuttle(0.0)]);
        assert_eq!(jog(&mut c, -250), vec![Action::Shuttle(-0.25)]);
        assert_eq!(jog(&mut c, i32::MIN), vec![Action::Shuttle(-8.0)]);
    }

    #[test]
    fn jog_accumulates_to_whole_frames() {
        let mut c = Controller::default();
        keys(&mut c, &[Key::Jog], 0.0);
        assert_eq!(c.wheel(), WheelMode::Jog);
        let rel = |c: &mut Controller, v| c.handle(&Report::Jog { mode: Some(JogMode::Relative), value: v }, 0.0);
        assert_eq!(rel(&mut c, 100), vec![]);
        assert_eq!(rel(&mut c, 200), vec![Action::Step(1)]);
        assert_eq!(rel(&mut c, -600), vec![Action::Step(-2)]);
        // absolute reports left over from shuttle mode are ignored
        assert_eq!(c.handle(&Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: 4000 }, 0.0), vec![]);
        // runaway values are capped
        assert_eq!(rel(&mut c, i32::MAX), vec![Action::Step(600)]);
    }

    #[test]
    fn scroll_moves_further_per_turn() {
        let mut c = Controller::default();
        keys(&mut c, &[Key::Scroll], 0.0);
        assert_eq!(c.handle(&Report::Jog { mode: Some(JogMode::Relative), value: 256 }, 0.0), vec![Action::Step(8)]);
    }

    #[test]
    fn changing_wheel_mode_stops_shuttle_and_rezeroes() {
        let mut c = Controller::default();
        let before = c.device_state(Lights::default());
        assert_eq!(before.jog_leds, jog_led::SHTL);
        c.handle(&Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: 3000 }, 0.0);
        assert!(c.shuttling());
        assert_eq!(keys(&mut c, &[Key::Jog], 0.0), vec![Action::Shuttle(0.0)]);
        let after = c.device_state(Lights { snap: true });
        assert_eq!(after.jog_mode, JogMode::Relative);
        assert_eq!(after.jog_leds, jog_led::JOG);
        assert_eq!(after.leds, led::SNAP);
        assert_ne!(after.jog_epoch, before.jog_epoch);
    }

    #[test]
    fn stop_play_hands_playback_back_and_rezeroes_the_wheel() {
        let mut c = Controller::default();
        c.handle(&Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: 3000 }, 0.0);
        let epoch = c.device_state(Lights::default()).jog_epoch;
        assert_eq!(keys(&mut c, &[Key::StopPlay], 0.0), vec![Action::Invoke("playback.toggle")]);
        assert!(!c.shuttling());
        assert_ne!(c.device_state(Lights::default()).jog_epoch, epoch);
    }

    #[test]
    fn hostile_config_does_not_panic() {
        let config = Config { jog_units_per_frame: 0, scroll_units_per_frame: -5, shuttle_speeds: [f64::NAN; 6], max_step: 0, ..Config::default() };
        let mut c = Controller::new(config);
        assert_eq!(c.handle(&Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: i32::MIN }, f64::NAN), vec![Action::Shuttle(-1.0)]);
        keys(&mut c, &[Key::Jog], f64::NAN);
        assert_eq!(c.handle(&Report::Jog { mode: Some(JogMode::Relative), value: i32::MIN }, 0.0), vec![Action::Step(-1)]);
        keys(&mut c, &[Key::Scroll], f64::INFINITY);
        let _ = c.handle(&Report::Jog { mode: Some(JogMode::Relative), value: i32::MAX }, 0.0);
        let _ = keys(&mut c, &[Key::In], f64::NAN);
        let _ = keys(&mut c, &[], f64::NAN);
        let _ = keys(&mut c, &[Key::In], f64::NAN);
    }
}
