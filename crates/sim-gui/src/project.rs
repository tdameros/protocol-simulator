//! The dock arrangement, which is the one part of a project only a window can
//! read.
//!
//! Everything else about the file lives in [`sim_session::project`]. This is
//! the section that section carries without understanding: a layout goes in as
//! an opaque value and comes back out here, so a project saved from a terminal
//! keeps the panes a window arranged.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, Result};
use egui::Theme;
use egui_dock::{DockState, NodeIndex};

#[cfg(test)]
use sim_session::project::UiSpec;
use sim_session::project::{Project, ThemeSpec};
use sim_session::state::{MonitorId, MonitorState, Session};

use crate::panels::Tab;

/// Everything the window currently holds, as it would be written down.
///
/// `path` is the file this is destined for, which is what relative paths are
/// relative to.
///
/// # Errors
///
/// Returns an error if the dock cannot be described, which would mean a tab
/// type serde cannot represent.
pub fn capture(
    state: &Session,
    dock: &DockState<Tab>,
    theme: Theme,
    path: Option<&Path>,
) -> Result<Project> {
    let mut project = Project::capture_settings(state, theme_spec(theme), path);
    project.ui.layout = Some(
        toml::Value::try_from(without_geometry(dock.clone()))
            .context("cannot describe the pane arrangement")?,
    );
    Ok(project)
}

/// The arrangement a restored project opens with, and the monitors that go
/// with it.
///
/// A layout the file does not carry, or one this build cannot read, falls back
/// to the default rather than refusing the project: panes are worth less than
/// the settings beside them.
#[must_use]
pub fn layout_of(
    project: &Project,
    monitors: &mut BTreeMap<MonitorId, MonitorState>,
) -> DockState<Tab> {
    let stored = project
        .ui
        .layout
        .clone()
        .and_then(|value| value.try_into::<DockState<Tab>>().ok());
    match stored {
        Some(dock) => reconciled(dock, monitors),
        None => default_layout(monitors),
    }
}

/// The file's word for a theme.
///
/// Two free functions rather than `From`, since neither type belongs to this
/// crate and the orphan rule has the last word.
#[must_use]
pub fn theme_spec(theme: Theme) -> ThemeSpec {
    match theme {
        Theme::Light => ThemeSpec::Light,
        Theme::Dark => ThemeSpec::Dark,
    }
}

/// The theme a file asked for.
#[must_use]
pub fn theme_of(spec: ThemeSpec) -> Theme {
    match spec {
        ThemeSpec::Light => Theme::Light,
        ThemeSpec::Dark => Theme::Dark,
    }
}

/// Clears the rectangles the dock recomputes on its first frame.
///
/// They are pixels, not settings: left in, every save would rewrite the file
/// with the window size it happened to have, and a project kept in Git would
/// show a diff for having been opened.
fn without_geometry(mut dock: DockState<Tab>) -> DockState<Tab> {
    for (_, node) in dock.iter_all_nodes_mut() {
        node.set_rect(egui::Rect::NOTHING);
        if let Some(leaf) = node.get_leaf_mut() {
            leaf.viewport = egui::Rect::NOTHING;
        }
    }
    dock
}

/// Gives every Traffic tab in a restored layout something to show, and drops
/// the monitors no tab refers to, so the file and the window agree.
fn reconciled(
    dock: DockState<Tab>,
    monitors: &mut BTreeMap<MonitorId, MonitorState>,
) -> DockState<Tab> {
    let shown: BTreeSet<MonitorId> = dock
        .iter_all_tabs()
        .filter_map(|(_, tab)| match tab {
            Tab::LiveMonitor(id) => Some(*id),
            _ => None,
        })
        .collect();

    for id in &shown {
        monitors
            .entry(*id)
            .or_insert_with(|| MonitorState::named(format!("Traffic {}", id.0)));
    }
    monitors.retain(|id, _| shown.contains(id));
    dock
}

/// The arrangement a project with no layout of its own opens with, which is
/// also what a first run gets.
#[must_use]
pub fn default_layout(monitors: &mut BTreeMap<MonitorId, MonitorState>) -> DockState<Tab> {
    if monitors.is_empty() {
        monitors.insert(MonitorId(1), MonitorState::named("Traffic".to_owned()));
    }
    let traffic: Vec<Tab> = monitors.keys().copied().map(Tab::LiveMonitor).collect();

    let mut dock = DockState::new(traffic);
    let surface = dock.main_surface_mut();
    let [live, _connections] = surface.split_left(NodeIndex::root(), 0.22, vec![Tab::Connections]);
    surface.split_below(
        live,
        0.6,
        vec![Tab::FrameEditor, Tab::Scenarios, Tab::HexInject],
    );
    dock
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(state: &Session, dock: &DockState<Tab>) -> Project {
        capture(state, dock, Theme::Dark, None).expect("a dock should describe")
    }

    #[test]
    fn dragging_a_pane_neither_rewrites_the_file_nor_asks_to_be_saved() {
        let state = Session::default();
        let fresh = default_layout(&mut BTreeMap::new());

        let mut used = fresh.clone();
        for (_, node) in used.iter_all_nodes_mut() {
            node.set_rect(egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(1280.0, 800.0),
            ));
        }

        // The pixels the dock measured on its first frame are not settings.
        assert_eq!(
            toml::to_string_pretty(&written(&state, &used)).expect("should serialise"),
            toml::to_string_pretty(&written(&state, &fresh)).expect("should serialise"),
        );

        // And a genuinely different arrangement is still not an unsaved change.
        let mut rearranged = fresh.clone();
        rearranged.push_to_focused_leaf(Tab::Connections);
        assert_eq!(written(&state, &rearranged), written(&state, &fresh));
    }

    #[test]
    fn a_traffic_tab_in_the_layout_always_has_something_to_show() {
        // A layout mentioning a tab the monitor list has lost, which is what a
        // hand-edited file tends to produce.
        let mut project = Project {
            ui: UiSpec {
                theme: ThemeSpec::Light,
                hex_values: false,
                layout: Some(
                    toml::Value::try_from(default_layout(&mut BTreeMap::new()))
                        .expect("a dock should describe"),
                ),
            },
            ..Project::default()
        };
        project.monitors.clear();

        let mut state = Session::default();
        let restored = project.apply(&mut state, None).expect("should apply");
        let mut monitors = restored.monitors;
        let dock = layout_of(&project, &mut monitors);
        state.restore_monitors(monitors);

        let shown: Vec<MonitorId> = dock
            .iter_all_tabs()
            .filter_map(|(_, tab)| match tab {
                Tab::LiveMonitor(id) => Some(*id),
                _ => None,
            })
            .collect();
        assert!(!shown.is_empty());
        for id in shown {
            assert!(state.monitors.contains_key(&id), "{id:?} has no monitor");
        }
    }

    #[test]
    fn the_starting_layout_holds_every_panel_there_is() {
        let mut monitors = BTreeMap::from([
            (MonitorId(1), MonitorState::named("Traffic".to_owned())),
            (MonitorId(4), MonitorState::named("Heartbeats".to_owned())),
        ]);
        let dock = default_layout(&mut monitors);
        let tabs: Vec<Tab> = dock.iter_all_tabs().map(|(_, tab)| *tab).collect();

        // What Reset layout has to hand back. Connections above all: a project
        // without it cannot be given a connection at all.
        for expected in [
            Tab::Connections,
            Tab::FrameEditor,
            Tab::Scenarios,
            Tab::HexInject,
            Tab::LiveMonitor(MonitorId(1)),
            Tab::LiveMonitor(MonitorId(4)),
        ] {
            assert!(tabs.contains(&expected), "{expected:?} is missing");
        }
        assert_eq!(tabs.len(), 6, "no tab was invented either");
        assert_eq!(monitors.len(), 2, "and none was lost on the way");
    }

    /// The section this crate is the only reader of, taken through the file it
    /// is carried in.
    #[test]
    fn a_pane_arrangement_survives_being_written_out_and_read_back() {
        let mut state = Session::default();
        state.open_monitor();
        let mut monitors = std::mem::take(&mut state.monitors);
        let mut dock = default_layout(&mut monitors);
        dock.push_to_focused_leaf(Tab::Connections);
        state.restore_monitors(monitors);

        let text = toml::to_string_pretty(&written(&state, &dock)).expect("should serialise");
        let read: Project = toml::from_str(&text).expect("should parse back");

        let mut restored = read
            .apply(&mut Session::default(), None)
            .expect("should apply")
            .monitors;
        let back = layout_of(&read, &mut restored);

        let was: Vec<Tab> = dock.iter_all_tabs().map(|(_, tab)| *tab).collect();
        let now: Vec<Tab> = back.iter_all_tabs().map(|(_, tab)| *tab).collect();
        assert_eq!(now, was, "through:\n{text}");
    }
}
