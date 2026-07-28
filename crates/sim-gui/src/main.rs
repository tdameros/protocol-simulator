#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod app;
mod engine_handle;
mod panels;
mod state;
mod theme;

use app::SimApp;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Protocol Simulator",
        native_options,
        Box::new(|cc| Ok(Box::new(SimApp::new(cc)))),
    )
}
