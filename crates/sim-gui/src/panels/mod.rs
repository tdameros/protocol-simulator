pub mod connections;
pub mod frame_editor;
pub mod hex_inject;
pub mod live_monitor;

use egui::{Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::TabViewer;

use crate::engine_handle::EngineHandle;
use crate::state::{AppState, MonitorId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Connections,
    /// Several may be open at once, each filtering the shared buffer its own way.
    LiveMonitor(MonitorId),
    HexInject,
    FrameEditor,
}

pub struct AppTabViewer<'a> {
    pub state: &'a mut AppState,
    pub engine: &'a EngineHandle,
}

impl TabViewer for AppTabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        match tab {
            Tab::Connections => "Connections".into(),
            // Renamed from inside the panel, so the tab reads back what you typed.
            Tab::LiveMonitor(id) => self
                .state
                .monitors
                .get(id)
                .map_or("Traffic", |monitor| monitor.title.as_str())
                .to_owned()
                .into(),
            Tab::HexInject => "Hex Inject".into(),
            Tab::FrameEditor => "Frames".into(),
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        match tab {
            Tab::Connections => connections::show(ui, self.state, self.engine),
            Tab::LiveMonitor(id) => live_monitor::show(ui, self.state, *id),
            Tab::HexInject => hex_inject::show(ui, self.state, self.engine),
            Tab::FrameEditor => frame_editor::show(ui, self.state, self.engine),
        }
    }

    /// Closing the last view of the buffer is allowed; the buffer itself is not
    /// the tab's to keep.
    fn on_close(&mut self, tab: &mut Self::Tab) -> OnCloseResponse {
        if let Tab::LiveMonitor(id) = tab {
            self.state.close_monitor(*id);
        }
        OnCloseResponse::Close
    }
}
