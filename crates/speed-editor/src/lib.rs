//! Blackmagic Design DaVinci Resolve Speed Editor support for FilmCraft / DSPV Cut.
//!
//! - [`protocol`]: the device's USB HID reports and authentication handshake (pure).
//! - [`controller`]: what the keys and the wheel do in the editor, as a pure state machine from
//!   reports to [`controller::Action`]s and the LED state to show.
//! - [`Link`]: the channel pair between the editor and a device thread. The editor side needs
//!   no OS or USB code, so the UI stays web-portable.
//! - `device` (feature `hid`): the device thread over the pure-Rust `async-hid` crate
//!   (hidraw on Linux, IOKit on macOS, the Win32 HID API on Windows): finds the device,
//!   authenticates, re-authenticates before the device's timeout, reconnects after unplugging,
//!   and writes LED / jog-mode changes.
//!
//! See the crate README for the key map and platform notes.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod controller;
#[cfg(feature = "hid")]
pub mod device;
pub mod protocol;

use std::sync::mpsc::{self, Receiver, Sender};

pub use controller::{Action, Controller, DeviceState, Lights, WheelMode};
pub use protocol::{Key, Report};

/// What the device thread tells the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A Speed Editor was found and authenticated.
    Connected { name: String },
    /// The device went away or could not be used; `reason` says why when it is an error.
    Disconnected { reason: Option<String> },
    /// An input report.
    Report(Report),
}

/// The editor's end of the connection to a device thread.
pub struct Link {
    events: Receiver<Event>,
    state: Sender<DeviceState>,
}

/// The device thread's end of a [`Link`].
pub struct DeviceSide {
    pub events: Sender<Event>,
    pub state: Receiver<DeviceState>,
}

impl Link {
    /// A connected pair of ends (the device thread takes the [`DeviceSide`]).
    pub fn pair() -> (Link, DeviceSide) {
        let (event_tx, event_rx) = mpsc::channel();
        let (state_tx, state_rx) = mpsc::channel();
        (Link { events: event_rx, state: state_tx }, DeviceSide { events: event_tx, state: state_rx })
    }

    /// The next pending event, if any. `None` also once the device thread has ended.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Ask the device to show this state. Cheap; the device thread writes only what changed.
    /// Ignored once the device thread has ended.
    pub fn set_state(&self, state: DeviceState) {
        let _ = self.state.send(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_pair_carries_events_and_state() {
        let (link, side) = Link::pair();
        assert_eq!(link.try_recv(), None);
        side.events.send(Event::Connected { name: "x".into() }).unwrap();
        assert_eq!(link.try_recv(), Some(Event::Connected { name: "x".into() }));
        let s = Controller::default().device_state(Lights::default());
        link.set_state(s);
        assert_eq!(side.state.try_recv().ok(), Some(s));
        drop(side);
        assert_eq!(link.try_recv(), None);
        link.set_state(s);
    }
}
