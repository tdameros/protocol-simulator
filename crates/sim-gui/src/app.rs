use sim_core::Event;

use egui::{Color32, CornerRadius, Theme};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::panels::{AppTabViewer, Tab};
use crate::state::{AppState, Direction, LogEntry};
use crate::theme;

pub struct SimApp {
    state: AppState,
    engine: EngineHandle,
    dock_state: DockState<Tab>,
}

impl SimApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);

        let mut dock_state = DockState::new(vec![Tab::LiveMonitor]);
        let surface = dock_state.main_surface_mut();
        let [live, _connections] =
            surface.split_left(NodeIndex::root(), 0.22, vec![Tab::Connections]);
        surface.split_below(live, 0.75, vec![Tab::HexInject]);

        Self {
            state: AppState::default(),
            engine: EngineHandle::new(),
            dock_state,
        }
    }

    fn apply_events(&mut self) {
        for event in self.engine.drain_events() {
            match event {
                Event::ConnectionStatus { id, status } => {
                    if let Some(entry) = self.state.connection_mut(&id) {
                        entry.status = status;
                    }
                }
                Event::FrameSent {
                    id,
                    bytes,
                    timestamp,
                } => {
                    self.state.push_log(LogEntry {
                        id,
                        direction: Direction::Sent,
                        bytes,
                        source: None,
                        timestamp,
                    });
                }
                Event::FrameReceived {
                    id,
                    bytes,
                    source,
                    timestamp,
                } => {
                    self.state.push_log(LogEntry {
                        id,
                        direction: Direction::Received,
                        bytes,
                        source,
                        timestamp,
                    });
                }
                Event::Error { id, error } => {
                    self.state.record_error(id, &error);
                }
            }
        }
    }

    fn toolbar(ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Protocol Simulator");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let is_dark = ui.ctx().theme() == Theme::Dark;
                let icon = if is_dark { icons::SUN } else { icons::MOON };
                if ui
                    .button(icon)
                    .on_hover_text("Toggle light / dark theme")
                    .clicked()
                {
                    ui.ctx().set_visuals(if is_dark {
                        egui::Visuals::light()
                    } else {
                        egui::Visuals::dark()
                    });
                }
            });
        });
    }
}

impl eframe::App for SimApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_events();
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));

        egui::Panel::top("toolbar").show(ui, Self::toolbar);

        if let Some(error) = self.state.last_error.clone() {
            egui::Panel::bottom("status_bar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(200, 60, 60), &error);
                    if ui.small_button("x").clicked() {
                        self.state.last_error = None;
                    }
                });
            });
        }

        egui::CentralPanel::no_frame().show(ui, |ui| {
            let mut viewer = AppTabViewer {
                state: &mut self.state,
                engine: &self.engine,
            };
            DockArea::new(&mut self.dock_state)
                .style(dock_style(ui.style()))
                .show_inside(ui, &mut viewer);
        });
    }

    /// Anything the panels do not paint shows this colour. eframe defaults it to
    /// near-black, which would show through as dark notches wherever panels meet.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

/// `egui_dock` derives panel corner radius from the interactive widget styles, so the
/// rounding meant for buttons and text fields ends up on surfaces that must tile
/// edge to edge — leaving uncovered notches at every junction. Square those off
/// while leaving the tabs themselves rounded.
fn dock_style(ui_style: &egui::Style) -> Style {
    let mut style = Style::from_egui(ui_style);
    style.tab_bar.corner_radius = CornerRadius::ZERO;
    style.tab.tab_body.corner_radius = CornerRadius::ZERO;
    style
}
