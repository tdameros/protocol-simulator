pub mod connections;
pub mod frame_editor;
pub mod hex_inject;
pub mod live_monitor;

use egui::{Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::TabViewer;

use crate::engine_handle::EngineHandle;
use crate::state::{AppState, MonitorId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

    /// The tab itself, never its title.
    ///
    /// `egui_dock` defaults to hashing the title, and hangs the whole tab body
    /// off the result. A Traffic tab can be renamed from inside itself, so that
    /// default would give every keystroke a different tab, and the field being
    /// typed into would lose focus on each character.
    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new(*tab)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renaming_a_traffic_tab_leaves_its_identity_alone() {
        let mut state = AppState::default();
        let monitor = state.open_monitor();
        let engine = EngineHandle::new();
        let mut viewer = AppTabViewer {
            state: &mut state,
            engine: &engine,
        };

        let mut tab = Tab::LiveMonitor(monitor);
        let before = viewer.id(&mut tab);
        assert_eq!(viewer.title(&mut tab).text(), "Traffic");

        if let Some(monitor) = viewer.state.monitors.get_mut(&monitor) {
            monitor.title = "Heartbeats".to_owned();
        }

        assert_eq!(viewer.title(&mut tab).text(), "Heartbeats");
        // Every widget in the tab hangs off this. If it moved with the title,
        // typing a name would knock the focus out of the field on each letter.
        assert_eq!(viewer.id(&mut tab), before);
    }
}
