//! The project file: everything the window was set up to do, in one file.
//!
//! Written to be read. Someone opening it in an editor should recognise their
//! own settings, and someone receiving it from a colleague should be able to
//! use it as is. Two things follow from that. Paths are stored relative to the
//! file and always with forward slashes, so a project survives the trip between
//! machines and between operating systems. And the mirror structs here spell
//! out the file's own vocabulary rather than deriving serde onto the live
//! state, whose field names answer to the UI and would drag its churn into
//! something people keep in Git.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{ensure, Context as _, Result};
use egui::Theme;
use egui_dock::{DockState, NodeIndex};
use serde::{Deserialize, Serialize};

use sim_core::config::ConnectionSpec;
use sim_core::frame::value::FieldValues;
use sim_core::{ConnectionId, ConnectionStatus, RetryPolicy, TransportConfig};

use crate::panels::Tab;
use crate::state::{
    AppState, ConnectionEntry, DirectionFilter, HexAnchor, MonitorId, MonitorState, TrafficFilter,
};

/// Bumped only when an older reader would get a project wrong. A reader refuses
/// what it does not know rather than silently dropping the parts it cannot see.
pub const FORMAT_VERSION: u32 = 1;

/// Name offered by the save dialog.
pub const DEFAULT_FILE_NAME: &str = "simulator.toml";

const HEADER: &str = "\
# Protocol Simulator project.
#
# Written by the app, meant to be read, and safe to edit by hand or to keep in
# Git. Paths are relative to this file. Serial port names are not: they differ
# from machine to machine, so expect to fix those after receiving a project
# from someone else.
";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,

    /// Where the frame definitions live, relative to this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_dir: Option<String>,

    /// Where the scenarios live, relative to this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenarios_dir: Option<String>,

    #[serde(default, rename = "connection", skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionSpec>,

    #[serde(default, rename = "monitor", skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<MonitorSpec>,

    #[serde(default, skip_serializing_if = "HexSpec::is_empty")]
    pub hex_inject: HexSpec,

    /// Field values per frame name, so the trames you were about to send are
    /// still loaded when the project is reopened.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, FieldValues>,

    #[serde(default)]
    pub ui: UiSpec,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            frames_dir: None,
            scenarios_dir: None,
            connections: Vec::new(),
            monitors: Vec::new(),
            hex_inject: HexSpec::default(),
            values: BTreeMap::new(),
            ui: UiSpec::default(),
        }
    }
}

impl Project {
    /// Everything the window currently holds, as it would be written down.
    ///
    /// `path` is the file this is destined for, which is what relative paths are
    /// relative to.
    #[must_use]
    pub fn capture(
        state: &AppState,
        dock: &DockState<Tab>,
        theme: Theme,
        path: Option<&Path>,
    ) -> Self {
        let mut project = Self::capture_settings(state, theme, path);
        project.ui.layout = Some(without_geometry(dock.clone()));
        project
    }

    /// Everything but the dock arrangement.
    ///
    /// What the title bar compares against, every frame, to know whether there
    /// is anything left to save. Cloning a layout that the comparison ignores
    /// anyway would be work done sixty times a second for nothing.
    #[must_use]
    pub fn capture_settings(state: &AppState, theme: Theme, path: Option<&Path>) -> Self {
        let base = path.and_then(Path::parent);
        Self {
            version: FORMAT_VERSION,
            frames_dir: state
                .frames
                .directory
                .as_deref()
                .map(|directory| write_path(directory, base)),
            scenarios_dir: state
                .scenarios
                .directory
                .as_deref()
                .map(|directory| write_path(directory, base)),
            hex_inject: HexSpec {
                target: state.hex_target.as_ref().map(|id| id.0.clone()),
                text: state.hex_input.clone(),
            },
            connections: state
                .connections
                .iter()
                .map(|(id, entry)| {
                    ConnectionSpec::describe(id, &entry.config, entry.retry, entry.autoconnect)
                })
                .collect(),
            monitors: state.monitors.values().map(MonitorSpec::capture).collect(),
            values: state.frames.saved_values().clone(),
            ui: UiSpec {
                theme: ThemeSpec::from(theme),
                hex_values: state.hex_values,
                layout: None,
            },
        }
    }

    /// Loads the project into `state`, and reports what the caller still has to
    /// do with it.
    ///
    /// Every connection is resolved before anything is touched, so a file with a
    /// setting no port could have leaves the current session alone instead of
    /// half replacing it.
    ///
    /// # Errors
    ///
    /// Returns an error if a connection entry cannot be turned into a usable
    /// configuration.
    pub fn apply(&self, state: &mut AppState, path: Option<&Path>) -> Result<Restored> {
        let resolved = self
            .connections
            .iter()
            .map(|spec| Ok((spec.resolve()?, spec.autoconnect)))
            .collect::<Result<Vec<_>>>()?;

        let base = path.and_then(Path::parent);
        state.connections.clear();
        let mut connect = Vec::new();
        for ((id, config, retry), autoconnect) in resolved {
            state.connections.push((
                id.clone(),
                ConnectionEntry {
                    config: config.clone(),
                    status: ConnectionStatus::Disconnected,
                    retry,
                    autoconnect,
                },
            ));
            if autoconnect {
                connect.push((id, config, retry));
            }
        }

        state.frames.forget();
        if let Some(directory) = self.frames_dir.as_deref().map(|text| read_path(text, base)) {
            state.frames.load_from(directory);
        }
        // After the definitions, never before: a value only knows what shape it
        // should be in once the field declaring it is loaded.
        state.frames.restore_values(self.values.clone());

        state.scenarios.forget();
        if let Some(directory) = self
            .scenarios_dir
            .as_deref()
            .map(|text| read_path(text, base))
        {
            state.scenarios.load_from(directory);
        }

        state.hex_values = self.ui.hex_values;
        state.hex_input.clone_from(&self.hex_inject.text);
        state.hex_target = self
            .hex_inject
            .target
            .as_ref()
            .map(|name| ConnectionId(name.clone()));

        let mut monitors = BTreeMap::new();
        for (index, spec) in self.monitors.iter().enumerate() {
            monitors.insert(MonitorId(index + 1), spec.restore());
        }
        let layout = self.ui.layout.clone();
        let layout = match layout {
            Some(dock) => reconciled(dock, &mut monitors),
            None => default_layout(&mut monitors),
        };
        state.restore_monitors(monitors);

        Ok(Restored {
            layout,
            theme: self.ui.theme.into(),
            connect,
        })
    }

    /// # Errors
    ///
    /// Returns an error if the file cannot be read, is not valid TOML, or was
    /// written by a build that knows a format this one does not.
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let project: Self = toml::from_str(&text)
            .with_context(|| format!("{} is not a valid project", path.display()))?;
        ensure!(
            project.version <= FORMAT_VERSION,
            "{} is a version {} project, and this build reads up to {FORMAT_VERSION}",
            path.display(),
            project.version,
        );
        Ok(project)
    }

    /// # Errors
    ///
    /// Returns an error if the project cannot be serialised or the file cannot
    /// be written.
    pub fn write(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("cannot describe this session")?;
        std::fs::write(path, format!("{HEADER}\n{body}"))
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

/// What loading a project leaves for the caller: the parts that are not
/// [`AppState`]'s to hold.
#[derive(Debug)]
pub struct Restored {
    pub layout: DockState<Tab>,
    pub theme: Theme,
    /// Connections the file asked to have opened.
    pub connect: Vec<(ConnectionId, TransportConfig, Option<RetryPolicy>)>,
}

/// The raw hex box, which is scratch space often enough that it is worth
/// keeping, and empty often enough that it is worth omitting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HexSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

impl HexSpec {
    fn is_empty(&self) -> bool {
        self.target.is_none() && self.text.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSpec {
    pub title: String,
    #[serde(default = "yes")]
    pub follow: bool,
    #[serde(default, skip_serializing_if = "not_set")]
    pub show_filter: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<FilterSpec>,
}

impl MonitorSpec {
    fn capture(monitor: &MonitorState) -> Self {
        Self {
            title: monitor.title.clone(),
            follow: monitor.follow,
            show_filter: monitor.show_filter,
            // An untouched filter is worth no lines in the file.
            filter: monitor
                .filter
                .is_active()
                .then(|| FilterSpec::capture(&monitor.filter)),
        }
    }

    fn restore(&self) -> MonitorState {
        MonitorState {
            title: self.title.clone(),
            filter: self
                .filter
                .as_ref()
                .map(FilterSpec::restore)
                .unwrap_or_default(),
            show_filter: self.show_filter,
            follow: self.follow,
            // Runtime position in a stream that starts empty, not a setting.
            paused_at: None,
            since: 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FilterSpec {
    /// Empty means every connection.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub connections: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "DirectionSpec::is_both")]
    pub direction: DirectionSpec,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<usize>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hex: String,
    /// Offset the hex pattern has to sit at. Absent means anywhere in the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<usize>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "not_set")]
    pub invert: bool,
}

impl FilterSpec {
    fn capture(filter: &TrafficFilter) -> Self {
        Self {
            connections: filter.connections.clone(),
            direction: DirectionSpec::from(filter.direction),
            source: filter.source.clone(),
            min_len: filter.min_len,
            max_len: filter.max_len,
            hex: filter.hex.clone(),
            at: match filter.anchor {
                HexAnchor::Anywhere => None,
                HexAnchor::At(offset) => Some(offset),
            },
            text: filter.text.clone(),
            invert: filter.invert,
        }
    }

    fn restore(&self) -> TrafficFilter {
        TrafficFilter {
            connections: self.connections.clone(),
            direction: self.direction.into(),
            source: self.source.clone(),
            min_len: self.min_len,
            max_len: self.max_len,
            hex: self.hex.clone(),
            anchor: self.at.map_or(HexAnchor::Anywhere, HexAnchor::At),
            text: self.text.clone(),
            invert: self.invert,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectionSpec {
    #[default]
    Both,
    Sent,
    Received,
}

impl DirectionSpec {
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "the signature is serde's, skip_serializing_if hands a reference"
    )]
    fn is_both(&self) -> bool {
        *self == Self::Both
    }
}

impl From<DirectionFilter> for DirectionSpec {
    fn from(direction: DirectionFilter) -> Self {
        match direction {
            DirectionFilter::Both => Self::Both,
            DirectionFilter::Sent => Self::Sent,
            DirectionFilter::Received => Self::Received,
        }
    }
}

impl From<DirectionSpec> for DirectionFilter {
    fn from(spec: DirectionSpec) -> Self {
        match spec {
            DirectionSpec::Both => Self::Both,
            DirectionSpec::Sent => Self::Sent,
            DirectionSpec::Received => Self::Received,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiSpec {
    #[serde(default)]
    pub theme: ThemeSpec,
    /// Whether whole-number fields are shown in hexadecimal.
    #[serde(default, skip_serializing_if = "not_set")]
    pub hex_values: bool,
    /// The dock arrangement, as `egui_dock` describes it. The one section here
    /// not meant to be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<DockState<Tab>>,
}

/// Compared on everything but the layout, and deliberately.
///
/// This is what tells the title bar whether there is anything to save, and
/// dragging a tab is not a change worth being nagged about. A layout that moved
/// is still written out by the next save; it just does not ask for one.
impl PartialEq for UiSpec {
    fn eq(&self, other: &Self) -> bool {
        self.theme == other.theme && self.hex_values == other.hex_values
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSpec {
    #[default]
    Light,
    Dark,
}

impl From<Theme> for ThemeSpec {
    fn from(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::Light,
            Theme::Dark => Self::Dark,
        }
    }
}

impl From<ThemeSpec> for Theme {
    fn from(spec: ThemeSpec) -> Self {
        match spec {
            ThemeSpec::Light => Self::Light,
            ThemeSpec::Dark => Self::Dark,
        }
    }
}

fn yes() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the signature is serde's, skip_serializing_if hands a reference"
)]
fn not_set(value: &bool) -> bool {
    !*value
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

/// A path as it goes into the file: relative to it where that is possible, and
/// always with forward slashes so the file crosses between operating systems.
fn write_path(path: &Path, base: Option<&Path>) -> String {
    let relative = base.and_then(|base| relative_to(path, base));
    let text = relative.unwrap_or_else(|| path.to_path_buf());
    text.to_string_lossy().replace('\\', "/")
}

/// The reverse. A relative path is read against the file's own folder, so where
/// the project was copied to is what counts, not where it was written.
fn read_path(text: &str, base: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(text);
    match base {
        Some(base) if path.is_relative() => base.join(path),
        _ => path,
    }
}

/// `path` expressed from `base`, or `None` when the two share no ground, which
/// on Windows means different drives.
fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    let mut from_path = path.components().peekable();
    let mut from_base = base.components().peekable();

    let mut shared = false;
    while from_path.peek().is_some() && from_path.peek() == from_base.peek() {
        from_path.next();
        from_base.next();
        shared = true;
    }
    if !shared {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in from_base {
        // Anything that is not a plain step up cannot be walked back out of.
        if component == Component::Normal(component.as_os_str()) {
            relative.push("..");
        } else {
            return None;
        }
    }
    relative.extend(from_path);
    Some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    use sim_core::{RetryPolicy, TcpMode};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sim-project-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn connection(name: &str, autoconnect: bool) -> (ConnectionId, ConnectionEntry) {
        (
            ConnectionId(name.to_owned()),
            ConnectionEntry {
                config: TransportConfig::Udp {
                    bind: "127.0.0.1:9000".parse().expect("address"),
                    remote: "127.0.0.1:9001".parse().expect("address"),
                },
                status: ConnectionStatus::Connected,
                retry: Some(RetryPolicy::standard()),
                autoconnect,
            },
        )
    }

    fn busy_state() -> AppState {
        let mut state = AppState::default();
        state.connections = vec![connection("bus", true), connection("probe", false)];
        state.hex_input = "AA 55".to_owned();
        state.hex_values = true;
        state.hex_target = Some(ConnectionId("bus".to_owned()));
        state.connections[1].1.config = TransportConfig::Tcp {
            mode: TcpMode::Server {
                listen: "0.0.0.0:502".parse().expect("address"),
            },
        };

        let id = state.open_monitor();
        let monitor = state.monitors.get_mut(&id).expect("the tab just opened");
        monitor.title = "Heartbeats".to_owned();
        monitor.filter = TrafficFilter {
            connections: BTreeSet::from(["bus".to_owned()]),
            direction: DirectionFilter::Received,
            hex: "AA 55".to_owned(),
            anchor: HexAnchor::At(0),
            invert: true,
            min_len: Some(4),
            ..TrafficFilter::default()
        };
        state
    }

    fn capture(state: &AppState, dock: &DockState<Tab>) -> Project {
        Project::capture(state, dock, Theme::Dark, None)
    }

    #[test]
    fn a_session_comes_back_the_way_it_was_written_down() {
        let state = busy_state();
        let dock = default_layout(&mut BTreeMap::new());
        let written = capture(&state, &dock);

        let text = toml::to_string_pretty(&written).expect("should serialise");
        let read: Project = toml::from_str(&text).expect("should parse back");
        assert_eq!(read, written, "through:\n{text}");

        let mut reopened = AppState::default();
        let restored = read.apply(&mut reopened, None).expect("should apply");

        assert_eq!(reopened.connections.len(), 2);
        assert_eq!(reopened.connections[0].0, ConnectionId("bus".to_owned()));
        // Only the one that asked for it, and never as already connected.
        assert_eq!(restored.connect.len(), 1);
        assert_eq!(restored.connect[0].0, ConnectionId("bus".to_owned()));
        assert!(reopened
            .connections
            .iter()
            .all(|(_, entry)| entry.status == ConnectionStatus::Disconnected));

        assert_eq!(reopened.hex_input, "AA 55");
        assert!(
            reopened.hex_values,
            "the base fields are read in comes back"
        );
        assert_eq!(reopened.hex_target, Some(ConnectionId("bus".to_owned())));
        assert_eq!(restored.theme, Theme::Dark);

        let monitor = reopened.monitors.values().next().expect("one tab");
        assert_eq!(monitor.title, "Heartbeats");
        assert_eq!(monitor.filter.anchor, HexAnchor::At(0));
        assert_eq!(monitor.filter.direction, DirectionFilter::Received);
        assert!(monitor.filter.invert);
        assert_eq!(monitor.filter.min_len, Some(4));

        // Compared as captured again, since that is what the title bar does to
        // decide whether anything is left to save.
        assert_eq!(capture(&reopened, &restored.layout), written);

        // And a tab opened afterwards cannot land on top of a restored one.
        assert_eq!(reopened.open_monitor(), MonitorId(2));
    }

    #[test]
    fn an_untouched_traffic_tab_costs_no_lines() {
        let mut state = AppState::default();
        state.open_monitor();
        let dock = default_layout(&mut BTreeMap::new());
        let text = toml::to_string_pretty(&capture(&state, &dock)).expect("should serialise");

        assert!(text.contains("[[monitor]]"), "{text}");
        assert!(!text.contains("[monitor.filter]"), "{text}");
    }

    #[test]
    fn a_frames_folder_travels_with_the_file() {
        let root = scratch("relative");
        let frames = root.join("frames");
        std::fs::create_dir_all(&frames).expect("frames dir");
        let file = root.join(DEFAULT_FILE_NAME);

        let mut state = AppState::default();
        state.frames.load_from(frames.clone());

        let dock = default_layout(&mut BTreeMap::new());
        let project = Project::capture(&state, &dock, Theme::Light, Some(&file));
        assert_eq!(project.frames_dir.as_deref(), Some("frames"));

        // The same file opened from somewhere else finds its frames there too,
        // which is the whole point of not writing an absolute path.
        let moved = scratch("relative-moved");
        std::fs::create_dir_all(moved.join("frames")).expect("frames dir");
        let mut elsewhere = AppState::default();
        project
            .apply(&mut elsewhere, Some(&moved.join(DEFAULT_FILE_NAME)))
            .expect("should apply");
        assert_eq!(
            elsewhere.frames.directory.as_deref(),
            Some(moved.join("frames").as_path())
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&moved).ok();
    }

    #[test]
    fn a_path_outside_the_project_folder_is_still_written_relative() {
        let base = Path::new("/home/dev/firmware/tools");
        let frames = Path::new("/home/dev/firmware/protocol/frames");
        assert_eq!(write_path(frames, Some(base)), "../protocol/frames");

        // Nothing in common leaves no choice but the absolute path.
        assert_eq!(relative_to(frames, Path::new("relative/base")), None);
    }

    #[test]
    fn dragging_a_pane_neither_rewrites_the_file_nor_asks_to_be_saved() {
        let state = AppState::default();
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
            toml::to_string_pretty(&capture(&state, &used)).expect("should serialise"),
            toml::to_string_pretty(&capture(&state, &fresh)).expect("should serialise"),
        );

        // And a genuinely different arrangement is still not an unsaved change.
        let mut rearranged = fresh.clone();
        rearranged.push_to_focused_leaf(Tab::Connections);
        assert_eq!(capture(&state, &rearranged), capture(&state, &fresh));
    }

    #[test]
    fn a_traffic_tab_in_the_layout_always_has_something_to_show() {
        // A layout mentioning a tab the monitor list has lost, which is what a
        // hand-edited file tends to produce.
        let mut project = Project {
            ui: UiSpec {
                theme: ThemeSpec::Light,
                hex_values: false,
                layout: Some(default_layout(&mut BTreeMap::new())),
            },
            ..Project::default()
        };
        project.monitors.clear();

        let mut state = AppState::default();
        let restored = project.apply(&mut state, None).expect("should apply");
        let shown: Vec<MonitorId> = restored
            .layout
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

    #[test]
    fn a_project_from_a_newer_build_is_refused_rather_than_half_read() {
        let dir = scratch("version");
        let file = dir.join(DEFAULT_FILE_NAME);
        std::fs::write(&file, format!("version = {}\n", FORMAT_VERSION + 1)).expect("write");

        let error = Project::read(&file).expect_err("should refuse");
        assert!(error.to_string().contains("version"), "{error}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_setting_no_port_could_have_leaves_the_session_alone() {
        let project: Project = toml::from_str(
            r#"
version = 1
[[connection]]
name = "uart"
transport = "serial"
port = "COM3"
baud = 9600
stop_bits = 3
"#,
        )
        .expect("valid toml");

        let mut state = AppState::default();
        state.connections = vec![connection("kept", false)];
        let error = project.apply(&mut state, None).expect_err("should refuse");
        assert!(error.to_string().contains("uart"), "{error}");
        assert_eq!(state.connections.len(), 1, "the session was left untouched");
    }

    #[test]
    fn a_file_says_what_it_is_before_it_says_anything_else() {
        let dir = scratch("header");
        let file = dir.join(DEFAULT_FILE_NAME);
        let state = AppState::default();
        let dock = default_layout(&mut BTreeMap::new());

        Project::capture(&state, &dock, Theme::Light, Some(&file))
            .write(&file)
            .expect("should write");
        let text = std::fs::read_to_string(&file).expect("should read back");
        assert!(text.starts_with("# Protocol Simulator project."), "{text}");
        Project::read(&file).expect("what it writes, it reads");

        std::fs::remove_dir_all(&dir).ok();
    }
}
