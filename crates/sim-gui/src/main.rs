#![deny(clippy::all)]
#![warn(clippy::pedantic)]
// Without this, Windows gives a GUI binary a console as well, which sits behind
// the window doing nothing. Only in release: a debug build keeps its console so
// panics and logs stay visible while working on it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod engine_handle;
mod frames;
mod panels;
mod project;
mod state;
mod theme;

use std::path::PathBuf;

use app::SimApp;

fn main() -> eframe::Result {
    // Optional positional argument: the folder holding frame .toml files, so a
    // project can be opened straight from the shell instead of clicking through
    // the picker every launch.
    let frames_dir = std::env::args().nth(1).map(PathBuf::from);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Protocol Simulator",
        native_options,
        Box::new(move |cc| Ok(Box::new(SimApp::new(cc, frames_dir)))),
    )
}
