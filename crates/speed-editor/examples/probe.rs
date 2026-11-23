//! Speed Editor probe: lists Blackmagic HID devices, starts the device thread and prints every
//! event for 60 seconds. For checking a setup without building the whole editor:
//!
//! ```text
//! cargo run -p filmcraft-speed-editor --features hid --example probe
//! ```

use std::time::{Duration, Instant};

use async_hid::HidBackend;
use filmcraft_speed_editor::protocol::{PRODUCT_ID, VENDOR_ID};
use filmcraft_speed_editor::{Controller, Event, Lights, device};
use futures_lite::StreamExt;
use futures_lite::future::block_on;

/// Prints `log` records to standard error.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        if r.target().starts_with("filmcraft") || r.level() <= log::Level::Warn {
            eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Debug));

    println!("HID devices from Blackmagic Design (vendor {VENDOR_ID:04x}):");
    match block_on(async {
        let backend = HidBackend::default();
        let mut list = Vec::new();
        let mut devices = backend.enumerate().await?;
        while let Some(d) = devices.next().await {
            if d.vendor_id == VENDOR_ID {
                list.push(format!(
                    "  {:?}  product {:04x}{}  usage page {:#06x} usage {:#06x}  {:?}",
                    d.name,
                    d.product_id,
                    if d.product_id == PRODUCT_ID { " (Speed Editor)" } else { "" },
                    d.usage_page,
                    d.usage_id,
                    d.id
                ));
            }
        }
        Ok::<_, async_hid::HidError>(list)
    }) {
        Ok(list) if list.is_empty() => println!("  none. Check the USB cable (use a data cable, directly into the Mac)."),
        Ok(list) => list.iter().for_each(|l| println!("{l}")),
        Err(e) => println!("  listing failed: {e}"),
    }

    let link = match device::spawn(|| {}) {
        Ok(l) => l,
        Err(e) => {
            println!("could not start the device thread: {e}");
            return;
        }
    };
    println!("\nWaiting for the device; press keys and turn the wheel. Stops after 60 s.\n");
    let mut controller = Controller::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        while let Some(event) = link.try_recv() {
            let t = start.elapsed().as_secs_f64();
            match &event {
                Event::Connected { name } => {
                    println!("{t:6.2}s CONNECTED: {name}");
                    controller.reset();
                    link.set_state(controller.device_state(Lights::default()));
                }
                Event::Disconnected { reason } => println!("{t:6.2}s DISCONNECTED: {}", reason.as_deref().unwrap_or("unplugged")),
                Event::Report(r) => {
                    let actions = controller.handle(r, t);
                    println!("{t:6.2}s {r:?} -> {actions:?}");
                    link.set_state(controller.device_state(Lights::default()));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("done");
}
