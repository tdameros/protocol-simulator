pub mod connections;
pub mod hex_inject;
pub mod live_monitor;

use egui::{Ui, WidgetText};
use egui_dock::TabViewer;

use crate::engine_handle::EngineHandle;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Connections,
    LiveMonitor,
    HexInject,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::LiveMonitor => "Live Monitor",
            Self::HexInject => "Hex Inject",
        }
    }
}

pub struct AppTabViewer<'a> {
    pub state: &'a mut AppState,
    pub engine: &'a EngineHandle,
}

impl TabViewer for AppTabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        match tab {
            Tab::Connections => connections::show(ui, self.state, self.engine),
            Tab::LiveMonitor => live_monitor::show(ui, self.state),
            Tab::HexInject => hex_inject::show(ui, self.state, self.engine),
        }
    }
}
