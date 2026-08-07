use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sim_core::Event;

use egui::{Color32, Context, CornerRadius, Modal, Theme, ViewportCommand};
use egui_dock::{DockArea, DockState, Style};
use egui_phosphor::regular as icons;

use crate::engine_handle::EngineHandle;
use crate::panels::{AppTabViewer, Tab};
use crate::prefs::Preferences;
use crate::project::{self, Project, DEFAULT_FILE_NAME};
use crate::state::{AppState, Direction, LogEntry};
use crate::theme;

const APP_NAME: &str = "Protocol Simulator";

/// Something that would replace the session, held back until the question of
/// unsaved changes has been answered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    New,
    Open(PathBuf),
    Browse,
    Quit,
}

pub struct SimApp {
    state: AppState,
    engine: EngineHandle,
    dock_state: DockState<Tab>,
    /// Where the current project lives, once it has anywhere to live.
    path: Option<PathBuf>,
    /// The project as the file has it. What the window holds is compared
    /// against this to know whether anything is left to save.
    saved: Project,
    prefs: Preferences,
    pending: Option<Pending>,
    /// Set once a close has been allowed through, so the guard does not catch
    /// the very close it just agreed to.
    leaving: bool,
    /// Last title handed to the window manager, to avoid saying it again on
    /// every frame.
    title: String,
}

impl SimApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opened_with: Option<PathBuf>) -> Self {
        // Whatever the window last showed, which egui remembers on its own. A
        // project overrides it on load, having its own opinion; a session
        // without one has nowhere else to keep the choice.
        let theme = cc.egui_ctx.theme();
        theme::apply(&cc.egui_ctx, theme);

        let mut state = AppState::default();
        let mut monitors = BTreeMap::new();
        let dock_state = project::default_layout(&mut monitors);
        state.restore_monitors(monitors);

        let mut app = Self {
            state,
            engine: EngineHandle::new(),
            dock_state,
            path: None,
            saved: Project::default(),
            prefs: Preferences::load(cc.storage),
            pending: None,
            leaving: false,
            title: String::new(),
        };
        app.saved = app.snapshot(theme);

        // A project file named on the command line, a frames folder as before,
        // or failing both, whatever was open last time.
        match opened_with {
            Some(path) if path.is_dir() => app.state.frames.load_from(path),
            Some(path) => app.open(&cc.egui_ctx, &path),
            None => {
                if let Some(path) = app.prefs.last().map(Path::to_path_buf) {
                    app.open(&cc.egui_ctx, &path);
                }
            }
        }
        app
    }

    fn snapshot(&self, theme: Theme) -> Project {
        Project::capture_settings(&self.state, theme, self.path.as_deref())
    }

    fn is_dirty(&self, theme: Theme) -> bool {
        self.snapshot(theme) != self.saved
    }

    fn open(&mut self, ctx: &Context, path: &Path) {
        let loaded = Project::read(path).and_then(|project| {
            let restored = project.apply(&mut self.state, Some(path))?;
            Ok(restored)
        });

        match loaded {
            Ok(restored) => {
                for (id, config, retry) in restored.connect {
                    self.engine.connect(id, config, retry);
                }
                self.dock_state = restored.layout;
                ctx.set_theme(restored.theme);
                self.path = Some(path.to_path_buf());
                self.saved = self.snapshot(restored.theme);
                self.prefs.remember(path);
                self.state.last_error = None;
            }
            Err(error) => {
                // A project that will not open is one that should stop being
                // offered, whether it moved, was deleted, or was hand-edited
                // into something unreadable.
                self.prefs.forget(path);
                self.state.last_error = Some(format!("{error:#}"));
            }
        }
    }

    fn start_new(&mut self, ctx: &Context) {
        for (id, _) in std::mem::take(&mut self.state.connections) {
            self.engine.disconnect(id);
        }
        self.state = AppState::default();
        let mut monitors = BTreeMap::new();
        self.dock_state = project::default_layout(&mut monitors);
        self.state.restore_monitors(monitors);
        self.path = None;
        self.saved = self.snapshot(ctx.theme());
    }

    /// Writes to the file the project came from, or asks for one.
    ///
    /// Returns whether anything reached the disk, which is what tells a pending
    /// close whether it may go ahead.
    fn save(&mut self, ctx: &Context) -> bool {
        match self.path.clone() {
            Some(path) => self.save_to(ctx, &path),
            None => self.save_as(ctx),
        }
    }

    fn save_as(&mut self, ctx: &Context) -> bool {
        let chosen = rfd::FileDialog::new()
            .set_file_name(DEFAULT_FILE_NAME)
            .add_filter("Project", &["toml"])
            .save_file();
        chosen.is_some_and(|path| self.save_to(ctx, &path))
    }

    fn save_to(&mut self, ctx: &Context, path: &Path) -> bool {
        // Set first: what the file holds depends on where it sits, paths inside
        // it being relative to it.
        self.path = Some(path.to_path_buf());
        let project = Project::capture(&self.state, &self.dock_state, ctx.theme(), Some(path));

        match project.write(path) {
            Ok(()) => {
                self.saved = self.snapshot(ctx.theme());
                self.prefs.remember(path);
                self.state.last_error = None;
                true
            }
            Err(error) => {
                self.state.last_error = Some(format!("{error:#}"));
                false
            }
        }
    }

    /// Runs a pending action, now that unsaved work is no longer in the way.
    fn go_ahead(&mut self, ctx: &Context, action: Pending) {
        match action {
            Pending::New => self.start_new(ctx),
            Pending::Open(path) => self.open(ctx, &path),
            Pending::Browse => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Project", &["toml"])
                    .pick_file()
                {
                    self.open(ctx, &path);
                }
            }
            Pending::Quit => {
                self.leaving = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// Asks for an action, or performs it straight away when there is nothing
    /// to lose by doing so.
    fn request(&mut self, ctx: &Context, action: Pending) {
        if self.is_dirty(ctx.theme()) {
            self.pending = Some(action);
        } else {
            self.go_ahead(ctx, action);
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
                        seq: 0,
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
                        seq: 0,
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
                Event::ScenarioStep { name, step, pass } => {
                    self.state
                        .running
                        .insert(name, crate::state::ScenarioRun { step, pass });
                }
                Event::ScenarioFinished { name, outcome } => {
                    self.state.running.remove(&name);
                    // A scenario that gave up says why, where a scenario that
                    // simply ran out has nothing to report.
                    if let sim_core::Outcome::Failed(reason) = outcome {
                        self.state.last_error = Some(format!("[{name}] {reason}"));
                    }
                }
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, dirty: bool) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| self.file_menu(ui));
            ui.menu_button("View", |ui| self.view_menu(ui));

            ui.separator();
            ui.label(
                egui::RichText::new(match &self.path {
                    Some(path) => file_label(path),
                    None => "Untitled project".to_owned(),
                })
                .strong(),
            );
            if dirty {
                ui.label(egui::RichText::new("edited").weak());
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let theme = ui.ctx().theme();
                let icon = if theme == Theme::Dark {
                    icons::SUN
                } else {
                    icons::MOON
                };
                if ui
                    .button(icon)
                    .on_hover_text("Toggle light / dark theme")
                    .clicked()
                {
                    ui.ctx().set_theme(match theme {
                        Theme::Dark => Theme::Light,
                        Theme::Light => Theme::Dark,
                    });
                }
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        if ui
            .button(format!("{} New project", icons::FILE_PLUS))
            .clicked()
        {
            self.request(&ctx, Pending::New);
            ui.close();
        }
        if ui
            .button(format!("{} Open project...", icons::FOLDER_OPEN))
            .clicked()
        {
            self.request(&ctx, Pending::Browse);
            ui.close();
        }

        ui.separator();

        if ui
            .button(format!("{} Save", icons::FLOPPY_DISK))
            .on_hover_text("Ctrl+S")
            .clicked()
        {
            self.save(&ctx);
            ui.close();
        }
        if ui
            .button(format!("{} Save as...", icons::FLOPPY_DISK_BACK))
            .clicked()
        {
            self.save_as(&ctx);
            ui.close();
        }

        ui.separator();

        ui.menu_button(format!("{} Recent", icons::LIST), |ui| {
            if self.prefs.recent.is_empty() {
                ui.weak("Nothing opened yet.");
                return;
            }
            for path in self.prefs.recent.clone() {
                if ui
                    .button(file_label(&path))
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    self.request(&ctx, Pending::Open(path));
                    ui.close();
                }
            }
        });
    }

    /// Where a closed panel is found again.
    ///
    /// Closing the Connections tab used to be one way, and with the layout
    /// saved alongside the project it stayed that way across restarts: no
    /// Connections panel means no way to create a connection, so the project
    /// became unusable rather than merely untidy.
    ///
    /// Entries never close anything. A menu that shuts a panel you meant to
    /// jump to would be a worse surprise than the one it fixes; the tab keeps
    /// its own close button for that.
    fn view_menu(&mut self, ui: &mut egui::Ui) {
        for (tab, label) in [
            (Tab::Connections, "Connections"),
            (Tab::FrameEditor, "Frames"),
            (Tab::Scenarios, "Scenarios"),
            (Tab::HexInject, "Hex Inject"),
        ] {
            self.reveal_entry(ui, tab, label);
        }

        let traffic: Vec<(Tab, String)> = self
            .state
            .monitors
            .iter()
            .map(|(id, monitor)| (Tab::LiveMonitor(*id), monitor.title.clone()))
            .collect();
        for (tab, label) in traffic {
            self.reveal_entry(ui, tab, &label);
        }

        ui.separator();

        if ui
            .button(format!("{} New Traffic tab", icons::PLUS))
            .clicked()
        {
            self.state.monitor_requested = true;
            ui.close();
        }
        if ui
            .button(format!("{} Reset layout", icons::ARROW_COUNTER_CLOCKWISE))
            .on_hover_text("Put the panels back where they started, keeping every Traffic tab")
            .clicked()
        {
            self.reset_layout();
            ui.close();
        }
    }

    /// One panel in the View menu, ticked when it is already somewhere on
    /// screen. Clicking brings it into view either way.
    fn reveal_entry(&mut self, ui: &mut egui::Ui, tab: Tab, label: &str) {
        let found = self.dock_state.find_tab(&tab);
        let mark = if found.is_some() { icons::CHECK } else { " " };

        if ui.button(format!("{mark}  {label}")).clicked() {
            match found {
                Some(path) => {
                    let _ = self.dock_state.set_active_tab(path);
                    self.dock_state
                        .set_focused_node_and_surface(path.node_path());
                }
                None => self.dock_state.push_to_focused_leaf(tab),
            }
            ui.close();
        }
    }

    /// Rebuilds the starting arrangement around the tabs that exist now, so a
    /// reset costs you the layout and nothing else.
    fn reset_layout(&mut self) {
        let mut monitors = std::mem::take(&mut self.state.monitors);
        self.dock_state = project::default_layout(&mut monitors);
        self.state.restore_monitors(monitors);
    }

    /// The question asked before a session is thrown away.
    fn unsaved_changes_modal(&mut self, ctx: &Context) {
        let Some(action) = self.pending.clone() else {
            return;
        };

        let response = Modal::new(egui::Id::new("unsaved_changes")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("Unsaved changes");
            ui.label(match &self.path {
                Some(path) => format!("{} has changes that are not on disk.", file_label(path)),
                None => "This project has never been saved.".to_owned(),
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    // Only proceed once it is really written: a cancelled save
                    // dialog must not take the session with it.
                    if self.save(ctx) {
                        self.pending = None;
                        self.go_ahead(ctx, action.clone());
                    }
                }
                if ui.button("Discard").clicked() {
                    self.pending = None;
                    self.go_ahead(ctx, action.clone());
                }
                if ui.button("Cancel").clicked() {
                    self.pending = None;
                }
            });
        });

        // Clicking outside is a cancel, the safe reading of an ambiguous click.
        if response.should_close() {
            self.pending = None;
        }
    }

    fn update_title(&mut self, ctx: &Context, dirty: bool) {
        let name = self
            .path
            .as_deref()
            .map_or_else(|| "Untitled".to_owned(), file_label);
        let mark = if dirty { " *" } else { "" };
        let title = format!("{name}{mark} - {APP_NAME}");
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for SimApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync_row_height(ui);
        self.apply_events();
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        let dirty = self.is_dirty(ctx.theme());
        self.update_title(&ctx, dirty);

        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save(&ctx);
        }

        // A close is held back rather than refused: the modal turns it into a
        // question, and answering it sends the close again.
        if ctx.input(|input| input.viewport().close_requested()) && dirty && !self.leaving {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, dirty));

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

        self.unsaved_changes_modal(&ctx);

        // The dock cannot be rearranged while it is drawing itself, so a panel
        // asking for a new tab leaves the request here.
        if std::mem::take(&mut self.state.monitor_requested) {
            let id = self.state.open_monitor();
            self.dock_state.push_to_focused_leaf(Tab::LiveMonitor(id));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Only what belongs to this machine. The project itself is written when
        // asked for, never behind your back.
        self.prefs.store(storage);
    }

    /// Anything the panels do not paint shows this colour. eframe defaults it to
    /// near-black, which would show through as dark notches wherever panels meet.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `egui_dock` derives panel corner radius from the interactive widget styles, so the
/// rounding meant for buttons and text fields ends up on surfaces that must tile
/// edge to edge, leaving uncovered notches at every junction. Square those off
/// while leaving the tabs themselves rounded.
fn dock_style(ui_style: &egui::Style) -> Style {
    let mut style = Style::from_egui(ui_style);
    style.tab_bar.corner_radius = CornerRadius::ZERO;
    style.tab.tab_body.corner_radius = CornerRadius::ZERO;
    style
}
