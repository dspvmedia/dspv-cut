//! The DaVinci Resolve Speed Editor in the editor: each frame, the device thread's events go
//! through [`filmcraft_speed_editor::Controller`] and its actions run as ordinary commands
//! (the same ids as menus, shortcuts and the control channel), and the device's LEDs follow
//! the editor's state. The device thread itself (USB) is started by the desktop app and handed
//! over with [`FilmcraftApp::with_speed_editor`]; this module has no OS code, so the web build
//! keeps working.

use serde_json::json;

use filmcraft_speed_editor::{Action, Controller, DeviceState, Event, Lights, Link};

use crate::FilmcraftApp;
use crate::dock::PanelKind;

/// Most events handled in one frame; the rest wait for the next (a flood cannot stall the UI).
const MAX_EVENTS_PER_FRAME: usize = 256;

/// A Speed Editor attached to the app.
pub struct SpeedEditor {
    link: Link,
    pub controller: Controller,
    /// Device name while connected.
    pub connected: Option<String>,
    sent: Option<DeviceState>,
}

impl SpeedEditor {
    pub fn new(link: Link) -> Self {
        SpeedEditor { link, controller: Controller::default(), connected: None, sent: None }
    }
}

impl FilmcraftApp {
    /// Attach a Speed Editor device thread (`filmcraft_speed_editor::device::spawn`).
    pub fn with_speed_editor(mut self, link: Link) -> Self {
        self.speed_editor = Some(SpeedEditor::new(link));
        self
    }

    /// Run what the Speed Editor asked for since the last frame and update its LEDs.
    pub(crate) fn poll_speed_editor(&mut self, ctx: &egui::Context) {
        let Some(mut se) = self.speed_editor.take() else { return };
        let now = ctx.input(|i| i.time);
        for n in 0.. {
            if n >= MAX_EVENTS_PER_FRAME {
                ctx.request_repaint();
                break;
            }
            let Some(event) = se.link.try_recv() else { break };
            match event {
                Event::Connected { name } => {
                    log::info!("speed editor: editor attached to {name}");
                    se.controller.reset();
                    se.sent = None;
                    self.ui.status = format!("{name} connected");
                    se.connected = Some(name);
                }
                Event::Disconnected { reason } => {
                    if se.controller.shuttling() {
                        self.stop_any_playback();
                    }
                    se.controller.reset();
                    se.sent = None;
                    match reason {
                        Some(r) => self.ui.status = r,
                        None if se.connected.is_some() => self.ui.status = "Speed Editor disconnected".into(),
                        None => {}
                    }
                    se.connected = None;
                }
                Event::Report(report) => {
                    for action in se.controller.handle(&report, now) {
                        let result = self.speed_editor_action(ctx, &action);
                        log::info!("speed editor: {action:?} -> {result:?} (focus: {:?}, playing: {})", self.ui.focused, self.playback.playing);
                        if let Err(e) = result {
                            self.ui.status = e;
                        }
                    }
                }
            }
        }
        // Playback stopped some other way (Space, a click, the end of the sequence) while the
        // wheel was shuttling: shuttle again from wherever the wheel is now.
        if se.controller.shuttling() && !self.playback.playing && !self.source_playback.clock.playing {
            se.controller.playback_taken_over();
        }
        if se.connected.is_some() {
            let state = se.controller.device_state(Lights { snap: self.session.state.snapping });
            if se.sent != Some(state) {
                se.link.set_state(state);
                se.sent = Some(state);
            }
        }
        self.speed_editor = Some(se);
    }

    fn source_focused(&self) -> bool {
        self.ui.focused == PanelKind::Source
    }

    fn stop_any_playback(&mut self) {
        if self.source_playback.clock.playing {
            self.stop_source();
        }
        if self.playback.playing {
            self.stop();
        }
    }

    fn speed_editor_action(&mut self, ctx: &egui::Context, action: &Action) -> Result<(), String> {
        match action {
            Action::Invoke(id) => crate::menus::invoke(self, ctx, id, json!({})).map(|_| ()),
            Action::Shuttle(speed) => {
                if *speed == 0.0 || !speed.is_finite() {
                    self.stop_any_playback();
                    Ok(())
                } else if self.source_focused() && self.session.state.source_item.is_some() {
                    self.shuttle_source(*speed)
                } else {
                    self.play(*speed);
                    Ok(())
                }
            }
            Action::Step(frames) => {
                // With edit points selected (trim mode) the wheel trims, as the arrow keys do.
                if self.session.is_enabled("trim.nudge") {
                    return self.session.execute("trim.nudge", json!({"frames": frames})).map(|_| ()).map_err(|e| e.to_string());
                }
                self.stop_any_playback();
                if self.source_focused() && self.session.state.source_item.is_some() {
                    self.step_source(*frames)
                } else {
                    self.session.execute("playhead.step", json!({"frames": frames})).map(|_| ()).map_err(|e| e.to_string())
                }
            }
        }
    }

    fn step_source(&mut self, frames: i64) -> Result<(), String> {
        let item = self.session.state.source_item.ok_or("Open a Source clip first")?;
        let view = filmcraft_engine::clip_ops::source_view(&self.session, item).ok_or("Source clip is unavailable")?;
        if view.rate.frame_duration().0 == 0 {
            return Ok(());
        }
        let frame = view.rate.frame_at(self.session.state.source_playhead).saturating_add(frames).max(0);
        let time = view.rate.tick_of(frame);
        self.session.execute("source.setPlayhead", json!({"time": time.0})).map(|_| ()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use filmcraft_engine::Session;
    use filmcraft_speed_editor::controller::command_ids;
    use filmcraft_speed_editor::protocol::{JogMode, led};
    use filmcraft_speed_editor::{Event, Key, Link, Report};
    use serde_json::json;

    use crate::FilmcraftApp;

    fn poll(app: &mut FilmcraftApp, ctx: &egui::Context, time: f64) {
        let mut output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn keys_and_wheel_drive_the_editor_and_leds_follow() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let (link, device) = Link::pair();
        let mut app = FilmcraftApp::new(session).with_speed_editor(link);
        app.audio = None;
        app.ui.focused = crate::dock::PanelKind::Timeline;
        let send = |e: Event| device.events.send(e).unwrap();

        send(Event::Connected { name: "DaVinci Resolve Speed Editor".into() });
        poll(&mut app, &ctx, 0.0);
        assert!(app.ui.status.contains("connected"));
        let state = device.state.try_recv().expect("LED state sent on connect");
        assert_eq!(state.jog_mode, JogMode::AbsoluteDeadZero);

        // SNAP toggles snapping, and its LED follows
        let snapping = app.session.state.snapping;
        send(Event::Report(Report::Keys(vec![Key::Snap])));
        send(Event::Report(Report::Keys(vec![])));
        poll(&mut app, &ctx, 0.1);
        assert_eq!(app.session.state.snapping, !snapping);
        let leds = std::iter::from_fn(|| device.state.try_recv().ok()).last().map(|s| s.leds);
        assert_eq!(leds.map(|l| l & led::SNAP != 0), Some(!snapping));

        // JOG mode: 512 units forward is two frames
        let start = app.session.playhead();
        send(Event::Report(Report::Keys(vec![Key::Jog])));
        send(Event::Report(Report::Keys(vec![])));
        send(Event::Report(Report::Jog { mode: Some(JogMode::Relative), value: 512 }));
        poll(&mut app, &ctx, 0.2);
        let rate = app.session.sequence_rate();
        assert_eq!(rate.frame_at(app.session.playhead()), rate.frame_at(start) + 2);

        // STOP/PLAY plays; the shuttle wheel then sets the speed; back to the middle stops
        send(Event::Report(Report::Keys(vec![Key::StopPlay])));
        send(Event::Report(Report::Keys(vec![])));
        poll(&mut app, &ctx, 0.3);
        assert!(app.playback.playing);
        send(Event::Report(Report::Keys(vec![Key::Shuttle])));
        send(Event::Report(Report::Keys(vec![])));
        poll(&mut app, &ctx, 0.4);
        send(Event::Report(Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: -2200 }));
        poll(&mut app, &ctx, 0.5);
        assert!(app.playback.playing);
        assert_eq!(app.playback.speed, -2.0);
        send(Event::Report(Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: 0 }));
        poll(&mut app, &ctx, 0.6);
        assert!(!app.playback.playing);

        // unplugging while shuttling stops playback
        send(Event::Report(Report::Jog { mode: Some(JogMode::AbsoluteDeadZero), value: 3000 }));
        poll(&mut app, &ctx, 0.7);
        assert!(app.playback.playing);
        send(Event::Disconnected { reason: None });
        poll(&mut app, &ctx, 0.8);
        assert!(!app.playback.playing);
        assert!(app.ui.status.contains("disconnected"));
        assert!(app.speed_editor.as_ref().is_some_and(|s| s.connected.is_none()));

        // the device thread going away leaves the app working
        drop(device);
        poll(&mut app, &ctx, 0.9);
    }

    /// Every command the Speed Editor can run exists, in the engine or in the UI.
    #[test]
    fn every_mapped_command_exists() {
        let ui: Vec<String> = crate::menus::external_commands().into_iter().map(|c| c.id.to_string()).collect();
        for id in command_ids() {
            let known = filmcraft_engine::commands::find(id).is_some() || ui.iter().any(|u| u == id);
            assert!(known, "Speed Editor maps a key to unknown command `{id}`");
        }
    }
}
