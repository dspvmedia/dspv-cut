//! FilmCraft desktop app: see the crate documentation (`lib.rs`) for the options.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    filmcraft::run()
}
