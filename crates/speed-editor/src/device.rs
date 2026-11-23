//! The device thread: find a Speed Editor over USB HID, authenticate, forward its reports,
//! write LED and jog-mode changes, re-authenticate before the device's timeout and reconnect
//! after it is unplugged. Every thread runs under `catch_unwind`; a failure becomes an
//! [`Event::Disconnected`] with the reason, never a crash of the editor.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use async_hid::{AsyncHidFeatureHandle, AsyncHidRead, AsyncHidWrite, Device, DeviceFeatureHandle, DeviceReader, DeviceWriter, HidBackend};
use futures_lite::StreamExt;
use futures_lite::future::block_on;

use crate::controller::DeviceState;
use crate::protocol::{self, AUTH_REPORT_LEN, MAX_INPUT_REPORT, PRODUCT_ID, VENDOR_ID, auth};
use crate::{DeviceSide, Event, Link};

/// How often to look for a device while none is connected.
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Wait after a failed connection attempt (busy device, permissions) before trying again.
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(5);
/// How often the control loop wakes to check the reader and the re-authentication deadline.
const TICK: Duration = Duration::from_millis(250);
/// Re-authentication period when the device does not say (it reports about 600 s).
const DEFAULT_AUTH_SECS: u64 = 300;

/// Start the device thread. `wake` is called after each event so the editor can repaint (for
/// egui: `ctx.request_repaint()`). The thread ends when the returned [`Link`] is dropped.
pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<Link> {
    let (link, side) = Link::pair();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    thread::Builder::new().name("speed-editor".into()).spawn(move || {
        let events = side.events.clone();
        let w = wake.clone();
        if let Err(p) = catch_unwind(AssertUnwindSafe(move || supervise(side, w))) {
            let reason = panic_message(&p);
            log::error!("speed editor thread failed: {reason}");
            let _ = events.send(Event::Disconnected { reason: Some(format!("Speed Editor support stopped: {reason}")) });
            wake();
        }
    })?;
    Ok(link)
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown error".into())
}

/// Why a connection ended.
enum End {
    /// The editor dropped its [`Link`]: stop for good.
    EditorGone,
    /// The device went away or failed; try again after the delay.
    Device(Option<String>, Duration),
}

fn supervise(side: DeviceSide, wake: Arc<dyn Fn() + Send + Sync>) {
    let DeviceSide { events, state } = side;
    let backend = HidBackend::default();
    let generation = Arc::new(AtomicU64::new(0));
    let mut desired: Option<DeviceState> = None;
    loop {
        let found = match block_on(find(&backend)) {
            Ok(d) => d,
            Err(e) => {
                log::debug!("speed editor: enumeration failed: {e}");
                None
            }
        };
        let end = match found {
            None => {
                // nothing connected: wait, keeping the latest requested state
                match drain_for(&state, SCAN_INTERVAL, &mut desired) {
                    Err(()) => End::EditorGone,
                    Ok(()) => continue,
                }
            }
            Some(device) => {
                let gen_now = generation.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
                run_connection(&device, gen_now, &generation, &events, &state, &mut desired, &wake)
            }
        };
        match end {
            End::EditorGone => return,
            End::Device(reason, delay) => {
                // the old reader (if still blocked in a read) drops its reports from now on
                generation.fetch_add(1, Ordering::SeqCst);
                if events.send(Event::Disconnected { reason }).is_err() {
                    return;
                }
                wake();
                if drain_for(&state, delay, &mut desired).is_err() {
                    return;
                }
            }
        }
    }
}

/// Wait up to `d`, keeping the latest state the editor asks for. `Err` once the editor is gone.
fn drain_for(state: &Receiver<DeviceState>, d: Duration, desired: &mut Option<DeviceState>) -> Result<(), ()> {
    let until = Instant::now() + d;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        match state.recv_timeout(left) {
            Ok(s) => *desired = Some(s),
            Err(RecvTimeoutError::Timeout) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => return Err(()),
        }
    }
}

async fn find(backend: &HidBackend) -> Result<Option<Device>, async_hid::HidError> {
    let mut devices = backend.enumerate().await?;
    let mut best: Option<Device> = None;
    while let Some(d) = devices.next().await {
        if d.vendor_id == VENDOR_ID && d.product_id == PRODUCT_ID {
            // Prefer the vendor-defined interface if the OS lists several collections.
            let vendor_page = d.usage_page >= 0xff00;
            if best.is_none() || vendor_page {
                best = Some(d);
            }
        }
    }
    Ok(best)
}

/// Read the device's feature report 6 at handshake step `step` and return its payload.
async fn read_auth(feature: &mut DeviceFeatureHandle, step: u8, what: &str) -> Result<u64, String> {
    // the first byte names the report to read
    let mut buf: [u8; AUTH_REPORT_LEN] = [6, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let n = feature.read_feature_report(&mut buf).await.map_err(|e| format!("Speed Editor authentication failed ({what}): {e}"))?;
    auth::payload(buf.get(..n.min(AUTH_REPORT_LEN)).unwrap_or(&buf), step)
}

async fn write_auth(feature: &mut DeviceFeatureHandle, report: &[u8], what: &str) -> Result<(), String> {
    feature.write_feature_report(report).await.map_err(|e| format!("Speed Editor authentication failed ({what}): {e}"))
}

/// Run the handshake. Returns seconds until the device wants it again.
async fn authenticate(feature: &mut DeviceFeatureHandle) -> Result<u64, String> {
    write_auth(feature, &auth::RESET, "reset").await?;
    let challenge = read_auth(feature, 0, "challenge").await?;
    write_auth(feature, &auth::HOST_CHALLENGE, "host challenge").await?;
    read_auth(feature, 2, "device response").await?;
    write_auth(feature, &auth::reply(protocol::auth_response(challenge)), "reply").await?;
    let status = read_auth(feature, 4, "status").await?;
    let secs = u64::from(auth::timeout_secs(status));
    Ok(if secs == 0 { DEFAULT_AUTH_SECS } else { secs })
}

/// Re-authenticate at 80% of the device's timeout, but not more often than every 30 s.
fn reauth_after(secs: u64) -> Duration {
    Duration::from_secs((secs.saturating_mul(4) / 5).max(30))
}

fn run_connection(
    device: &Device,
    generation: u64,
    current: &Arc<AtomicU64>,
    events: &Sender<Event>,
    state: &Receiver<DeviceState>,
    desired: &mut Option<DeviceState>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> End {
    let busy = |what: &str, err: async_hid::HidError| {
        End::Device(
            Some(format!(
                "Could not open the Speed Editor ({what}: {err}). Close DaVinci Resolve if it is running; on Linux, the device needs a udev rule (see the README)."
            )),
            RETRY_AFTER_ERROR,
        )
    };
    let mut feature = match block_on(device.open_feature_handle()) {
        Ok(f) => f,
        Err(e) => return busy("feature reports", e),
    };
    let (reader, mut writer) = match block_on(device.open()) {
        Ok(rw) => rw,
        Err(e) => return busy("reports", e),
    };
    let secs = match block_on(authenticate(&mut feature)) {
        Ok(s) => s,
        Err(reason) => return End::Device(Some(format!("{reason}. Close DaVinci Resolve if it is running.")), RETRY_AFTER_ERROR),
    };
    let mut reauth_at = Instant::now() + reauth_after(secs);

    let (dead_tx, dead_rx) = std::sync::mpsc::channel::<String>();
    if let Err(e) = spawn_reader(reader, generation, current.clone(), events.clone(), wake.clone(), dead_tx) {
        return End::Device(Some(format!("Could not start the Speed Editor reader: {e}")), RETRY_AFTER_ERROR);
    }
    let name = if device.name.trim().is_empty() { "DaVinci Resolve Speed Editor".to_string() } else { device.name.clone() };
    if events.send(Event::Connected { name }).is_err() {
        return End::EditorGone;
    }
    wake();

    let mut written: Option<DeviceState> = None;
    loop {
        if let Err(e) = block_on(write_changes(&mut writer, written, *desired)) {
            return End::Device(Some(format!("Speed Editor stopped responding: {e}")), SCAN_INTERVAL);
        }
        if desired.is_some() {
            written = *desired;
        }
        match dead_rx.try_recv() {
            Ok(reason) => return End::Device(Some(reason).filter(|r| !r.is_empty()), SCAN_INTERVAL),
            Err(TryRecvError::Disconnected) => return End::Device(None, SCAN_INTERVAL),
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= reauth_at {
            match block_on(authenticate(&mut feature)) {
                Ok(s) => reauth_at = Instant::now() + reauth_after(s),
                Err(reason) => return End::Device(Some(reason), RETRY_AFTER_ERROR),
            }
        }
        match state.recv_timeout(TICK) {
            Ok(s) => {
                *desired = Some(s);
                // take every queued update; only the latest matters
                while let Ok(s) = state.try_recv() {
                    *desired = Some(s);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return End::EditorGone,
        }
    }
}

/// Write the parts of `want` that differ from `have` (everything when nothing was written yet).
async fn write_changes(writer: &mut DeviceWriter, have: Option<DeviceState>, want: Option<DeviceState>) -> Result<(), async_hid::HidError> {
    let Some(want) = want else { return Ok(()) };
    if have.is_none_or(|h| h.jog_mode != want.jog_mode || h.jog_epoch != want.jog_epoch) {
        writer.write_output_report(&protocol::jog_mode_report(want.jog_mode)).await?;
    }
    if have.is_none_or(|h| h.jog_leds != want.jog_leds) {
        writer.write_output_report(&protocol::jog_led_report(want.jog_leds)).await?;
    }
    if have.is_none_or(|h| h.leds != want.leds) {
        writer.write_output_report(&protocol::led_report(want.leds)).await?;
    }
    Ok(())
}

fn spawn_reader(
    mut reader: DeviceReader,
    generation: u64,
    current: Arc<AtomicU64>,
    events: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    dead: Sender<String>,
) -> std::io::Result<()> {
    thread::Builder::new().name("speed-editor-read".into()).spawn(move || {
        let dead2 = dead.clone();
        let run = catch_unwind(AssertUnwindSafe(move || {
            let mut buf = [0u8; MAX_INPUT_REPORT];
            loop {
                let n = match block_on(reader.read_input_report(&mut buf)) {
                    Ok(n) => n,
                    Err(async_hid::HidError::Disconnected) => {
                        let _ = dead.send(String::new());
                        return;
                    }
                    Err(e) => {
                        let _ = dead.send(format!("Speed Editor read failed: {e}"));
                        return;
                    }
                };
                if current.load(Ordering::SeqCst) != generation {
                    return; // a newer connection owns the device
                }
                let Some(report) = buf.get(..n.min(buf.len())).and_then(protocol::parse_report) else { continue };
                if events.send(Event::Report(report)).is_err() {
                    return;
                }
                wake();
            }
        }));
        if let Err(p) = run {
            let _ = dead2.send(format!("Speed Editor reader failed: {}", panic_message(&p)));
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reauth_period_is_bounded() {
        assert_eq!(reauth_after(600), Duration::from_secs(480));
        assert_eq!(reauth_after(0), Duration::from_secs(30));
        assert_eq!(reauth_after(u64::MAX), Duration::from_secs(u64::MAX / 5));
    }

    #[test]
    fn drain_keeps_latest_and_sees_editor_leave() {
        let (link, side) = Link::pair();
        let s = crate::Controller::default().device_state(crate::Lights::default());
        link.set_state(s);
        let mut desired = None;
        assert_eq!(drain_for(&side.state, Duration::from_millis(20), &mut desired), Ok(()));
        assert_eq!(desired, Some(s));
        drop(link);
        assert_eq!(drain_for(&side.state, Duration::from_millis(20), &mut desired), Err(()));
    }
}
