//! What the terminal front end is showing, and what a key does to it.
//!
//! Kept apart from the drawing so that a key press can be tested without a
//! terminal, the same way the panels are tested without a window.

use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use sim_core::frame::codec;
use sim_core::{ConnectionStatus, RetryPolicy};

use crate::connection_form::ConnectionForm;
use sim_session::engine_handle::EngineHandle;
use sim_session::hex;
use sim_session::project::Project;
use sim_session::reading::{self, Reading};
use sim_session::scenarios;
use sim_session::state::{ConnectionEntry, LogEntry, MonitorState, Session};

/// The same five views the window has, in the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Connections,
    Traffic,
    HexInject,
    Frames,
    Scenarios,
}

impl Tab {
    pub const ALL: [Self; 5] = [
        Self::Connections,
        Self::Traffic,
        Self::HexInject,
        Self::Frames,
        Self::Scenarios,
    ];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::Traffic => "Traffic",
            Self::HexInject => "Hex",
            Self::Frames => "Frames",
            Self::Scenarios => "Scenarios",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn at(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }
}

/// Which pane of the Frames view a key acts on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FramesFocus {
    Library,
    Fields,
}

/// One line of a frame's detail: a field on its own, or one flag inside a
/// bitfield.
///
/// Flattened so navigation is one dimension: a bitfield's flags are things to
/// move onto and edit exactly as a plain field is.
#[derive(Clone, Copy)]
pub enum FieldRow {
    Field(usize),
    Bit { field: usize, bit: usize },
}

/// Every row a frame's detail draws, in the order it draws them.
pub fn field_rows(frame: &sim_core::frame::FrameDef) -> Vec<FieldRow> {
    let mut rows = Vec::new();
    for (index, field) in frame.fields.iter().enumerate() {
        rows.push(FieldRow::Field(index));
        if let sim_core::frame::FieldKind::Bits { bits, .. } = &field.kind {
            rows.extend((0..bits.len()).map(|bit| FieldRow::Bit { field: index, bit }));
        }
    }
    rows
}

/// What is laid over the view, taking the keys the view would otherwise get.
/// What choosing an answer in [`Overlay::Pick`] does with it.
pub enum PickPurpose {
    /// Settles which definition a captured row is read through.
    DecodeAs,
    /// Settles the value of one enum field, by variant name.
    EnumField { field: usize },
    /// Settles which connection a frame is sent on.
    FrameTarget,
}

/// One value typed as text, and what it belongs to.
pub struct EditBox {
    pub title: String,
    pub text: String,
    target: EditTarget,
}

enum EditTarget {
    Field(usize),
    Bit { field: usize, bit: usize },
}

pub enum Overlay {
    /// The key map.
    Keys,
    /// One answer to be chosen from a list.
    Pick(Picker, PickPurpose),
    /// One value typed as text: a number, some bytes, a run of characters.
    EditText(EditBox),
    /// A file to be found on this machine.
    Browse(Browser),
    /// A connection being described before it exists.
    NewConnection(ConnectionForm),
}

/// Walking the disk to reach a file.
///
/// The window opens a desktop file dialog. A board reached over ssh has no
/// desktop to put one on, so the walk is here: a folder at a time, with the
/// same keys as every other list.
pub struct Browser {
    at: PathBuf,
    picker: Picker,
    /// What went wrong reading a folder, in place of its contents.
    trouble: Option<String>,
}

/// The entry that goes back up, shown first so it is always in the same place.
const UPWARDS: &str = "..";

impl Browser {
    fn opening(at: PathBuf) -> Self {
        let mut browser = Self {
            at,
            picker: Picker::new(String::new(), Vec::new()),
            trouble: None,
        };
        browser.listing();
        browser
    }

    /// Where the walk currently is, which is what the popup titles itself with.
    #[must_use]
    pub fn at(&self) -> &Path {
        &self.at
    }

    #[must_use]
    pub fn trouble(&self) -> Option<&str> {
        self.trouble.as_deref()
    }

    #[must_use]
    pub fn picker(&self) -> &Picker {
        &self.picker
    }

    /// Folders first, then files, each in name order.
    ///
    /// A folder that cannot be read says so in place of its contents rather
    /// than showing an empty one, which reads as a folder with nothing in it.
    fn listing(&mut self) {
        let mut folders = Vec::new();
        let mut files = Vec::new();

        match std::fs::read_dir(&self.at) {
            Ok(entries) => {
                self.trouble = None;
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    // Dot files are noise on the way to a project.
                    if name.starts_with('.') {
                        continue;
                    }
                    if entry.path().is_dir() {
                        folders.push(format!("{name}/"));
                    } else {
                        files.push(name);
                    }
                }
            }
            Err(error) => self.trouble = Some(format!("{}: {error}", self.at.display())),
        }

        folders.sort();
        files.sort();

        let mut options = vec![UPWARDS.to_owned()];
        options.append(&mut folders);
        options.append(&mut files);
        self.picker = Picker::new(String::new(), options);
    }

    /// Takes the line under the cursor. A folder is walked into, a file is the
    /// answer.
    fn taken(&mut self) -> Option<PathBuf> {
        let name = self.picker.taken()?;
        if name == UPWARDS {
            if let Some(up) = self.at.parent() {
                self.at = up.to_path_buf();
                self.listing();
            }
            return None;
        }
        let path = self.at.join(name.trim_end_matches('/'));
        if path.is_dir() {
            self.at = path;
            self.listing();
            return None;
        }
        Some(path)
    }
}

/// A list to choose one line from, narrowed by what is typed.
///
/// The window has combo boxes. A terminal has this, and it is the same idea:
/// the choice is offered rather than spelled, so a name that does not exist
/// cannot be given.
pub struct Picker {
    pub title: String,
    options: Vec<String>,
    typed: String,
    at: usize,
}

impl Picker {
    fn new(title: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            options,
            typed: String::new(),
            at: 0,
        }
    }

    /// What is typed so far, shown so that a narrowed list explains itself.
    #[must_use]
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// The lines still on offer, and which of them is under the cursor.
    #[must_use]
    pub fn shown(&self) -> (Vec<&str>, usize) {
        let wanted = self.typed.to_lowercase();
        let shown: Vec<&str> = self
            .options
            .iter()
            .filter(|option| option.to_lowercase().contains(&wanted))
            .map(String::as_str)
            .collect();
        let at = self.at.min(shown.len().saturating_sub(1));
        (shown, at)
    }

    fn step(&mut self, delta: isize) {
        let (shown, at) = self.shown();
        let Some(last) = shown.len().checked_sub(1) else {
            return;
        };
        self.at = at.saturating_add_signed(delta).min(last);
    }

    fn taken(&self) -> Option<String> {
        let (shown, at) = self.shown();
        shown.get(at).map(|name| (*name).to_owned())
    }

    /// Typing narrows the list and puts the cursor back at the top, since the
    /// line it was on is usually not the line still wanted.
    fn typing(&mut self, letter: char) {
        self.typed.push(letter);
        self.at = 0;
    }

    fn rubbed_out(&mut self) {
        self.typed.pop();
        self.at = 0;
    }
}

pub struct App {
    tab: Tab,
    overlay: Option<Overlay>,
    /// Whether a box has the keyboard.
    ///
    /// A box being typed into takes every letter, digits included, so while one
    /// does the keys that move between views are not the view's to take. Said
    /// as a state rather than left implicit, or the hint line would promise a
    /// way out that types a `q` instead.
    editing: bool,
    running: bool,
    session: Session,
    engine: EngineHandle,
    /// The project this was opened with, for the header to name.
    path: Option<PathBuf>,
    /// The folder last looked in, whether or not it held what was wanted.
    ///
    /// Kept apart from `path`, which is a project that opened. Naming a file
    /// that is not there is usually a typo, and the folder around it is where
    /// the right name is.
    looked_in: Option<PathBuf>,
    /// The connection under the cursor in the Connections view.
    connection_at: Option<usize>,
    /// The last thing that went right, until the next key reads it.
    ///
    /// Apart from `session.last_error`, which is for what did not: the two
    /// never compete for the one line reserved above the hints.
    status: Option<String>,
    /// Which pane of the Frames view a key acts on.
    frame_focus: FramesFocus,
    /// The row under the cursor in the fields pane, and the frame it belongs
    /// to.
    ///
    /// The frame is kept alongside the index so that switching to a
    /// differently shaped frame resets the cursor rather than landing on
    /// whatever row happened to share its number.
    field_at: Option<(String, usize)>,
}

impl Default for App {
    fn default() -> Self {
        let mut session = Session::default();
        // A view to show traffic in, before any project says otherwise. Without
        // one there is no filter to pass, so nothing would ever be drawn.
        session.open_monitor();
        Self {
            tab: Tab::Connections,
            overlay: None,
            editing: false,
            running: true,
            session,
            engine: EngineHandle::new(),
            path: None,
            looked_in: None,
            connection_at: None,
            status: None,
            frame_focus: FramesFocus::Library,
            field_at: None,
        }
    }
}

impl App {
    /// Opens what the command line named: a project file, or a folder of frame
    /// definitions.
    ///
    /// A link the project marked to open is opened straight away. A bench is
    /// reached to watch something already happening, and a first keystroke
    /// spent switching links on is a keystroke the operator should not need.
    #[must_use]
    pub fn opening(opened_with: Option<PathBuf>) -> Self {
        let mut app = Self::default();
        match opened_with {
            Some(path) if path.is_dir() => app.session.frames.load_from(path),
            Some(path) => {
                app.looked_in = path.parent().map(Path::to_path_buf);
                app.open(&path);
            }
            None => {}
        }
        // A project without one, or no project at all, still has traffic to
        // show.
        if app.session.monitors.is_empty() {
            app.session.open_monitor();
        }
        app
    }

    fn open(&mut self, path: &Path) {
        let loaded = Project::read(path).and_then(|read| read.apply(&mut self.session, Some(path)));
        match loaded {
            Ok(restored) => {
                self.looked_in = path.parent().map(Path::to_path_buf);
                for (id, config, retry) in restored.connect {
                    self.engine.connect(id, config, retry);
                }
                self.session.restore_monitors(restored.monitors);
                self.path = Some(path.to_path_buf());
            }
            Err(error) => self.session.last_error = Some(format!("{error:#}")),
        }
    }

    /// Everything the engine has said since the last pass.
    pub fn take_engine_events(&mut self) {
        self.engine.drain_into(&mut self.session);
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The connection under the cursor in the Connections view.
    #[must_use]
    pub fn connection_at(&self) -> Option<usize> {
        self.connection_at
    }

    /// Whether a key in the Frames view acts on the frame list or the fields
    /// of the one chosen.
    #[must_use]
    pub fn frame_focus_is_fields(&self) -> bool {
        self.frame_focus == FramesFocus::Fields
    }

    /// The row under the cursor in the fields pane, for the frame currently
    /// on show.
    #[must_use]
    pub fn field_at(&self, frame: &str) -> Option<usize> {
        match &self.field_at {
            Some((name, at)) if name == frame => Some(*at),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// The traffic view this front end shows.
    ///
    /// The window has a tab per view. A terminal has one screen, so it shows
    /// the first the project described, which is where its filter and its
    /// follow setting come from.
    #[must_use]
    pub fn monitor(&self) -> Option<&MonitorState> {
        self.session.monitors.values().next()
    }

    /// The row being read, and what it reads as.
    ///
    /// Settling which definition to use is part of the answer, hence the
    /// mutable borrow: a row with one candidate takes it without asking.
    pub fn selected_reading(&mut self) -> Option<(&LogEntry, Reading<'_>)> {
        let seq = self.session.monitors.values().next()?.selected?;
        let entry = self.session.log.iter().find(|entry| entry.seq == seq)?;
        let decode_as = &mut self.session.monitors.values_mut().next()?.decode_as;
        let reading = reading::read(&self.session.frames, entry, decode_as);
        Some((entry, reading))
    }

    /// The rows that pass the view's filter, oldest first.
    #[must_use]
    pub fn rows(&self) -> Vec<&LogEntry> {
        let Some(monitor) = self.monitor() else {
            return Vec::new();
        };
        let compiled = monitor.filter.compile();
        self.session
            .log
            .iter()
            .filter(|entry| monitor.in_window(entry) && compiled.keeps(entry))
            .collect()
    }

    /// The project on show, by file name.
    #[must_use]
    pub fn opened(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    #[must_use]
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.running
    }

    /// What every view answers to, whichever one is on show.
    pub const KEYS: [(&'static str, &'static str); 4] = [
        ("o", "open"),
        ("1-5", "go to a view"),
        ("Tab", "next view"),
        ("Shift+Tab", "previous view"),
    ];

    /// The way out, and the way to the rest.
    ///
    /// Kept apart because these are never dropped for want of room: not knowing
    /// how to leave a terminal program is how a session gets killed from
    /// another window.
    pub const ESCAPES: [(&'static str, &'static str); 2] = [("?", "keys"), ("q", "quit")];

    /// The way out of wherever the keyboard currently is.
    ///
    /// A box being typed into swallows `q`, so promising it there would be a
    /// lie. What it does answer to is `Esc`.
    #[must_use]
    pub fn escapes(&self) -> &'static [(&'static str, &'static str)] {
        if self.editing {
            &[("Esc", "done")]
        } else {
            &Self::ESCAPES
        }
    }

    /// What the view on show adds to them.
    ///
    /// Offered without being asked for, since a key nobody can guess is a key
    /// nobody presses.
    #[must_use]
    pub fn view_keys(&self) -> &'static [(&'static str, &'static str)] {
        // An overlay has the keyboard, so it is its keys that are worth the
        // room: the view behind it answers to nothing while it is up.
        if self.editing {
            return &[("Enter", "send")];
        }

        // A list has the keyboard, so its keys are the ones worth the room. The
        // key map does not: it is what you open to read the view's own keys, so
        // it falls through to them.
        if let Some(Overlay::Pick(_, _)) = self.overlay {
            return &[
                ("up/down", "choose"),
                ("type", "narrow"),
                ("Enter", "take it"),
                ("Esc", "back"),
            ];
        }
        if let Some(Overlay::Browse(_)) = self.overlay {
            return &[
                ("up/down", "choose"),
                ("type", "narrow"),
                ("Enter", "open"),
                ("Esc", "back"),
            ];
        }
        if let Some(Overlay::NewConnection(_)) = self.overlay {
            return &[
                ("Tab", "next field"),
                ("left/right", "change"),
                ("space", "toggle"),
                ("Enter", "create"),
                ("Esc", "cancel"),
            ];
        }
        if let Some(Overlay::EditText(_)) = self.overlay {
            return &[("Enter", "apply"), ("Esc", "cancel")];
        }

        match self.tab {
            Tab::Traffic => &[
                ("up/down", "read a row"),
                ("Enter", "read as"),
                ("Esc", "put it away"),
                ("f", "follow"),
            ],
            Tab::Scenarios => &[("up/down", "choose"), ("Enter", "run"), ("x", "stop")],
            Tab::HexInject => &[("Enter", "type bytes"), ("x", "clear")],
            Tab::Frames if self.frame_focus == FramesFocus::Fields => &[
                ("up/down", "choose"),
                ("Enter", "edit"),
                ("s", "send"),
                ("Left", "list"),
            ],
            Tab::Frames => &[
                ("up/down", "choose"),
                ("Right", "fields"),
                ("t", "target"),
                ("s", "send"),
            ],
            Tab::Connections => &[
                ("up/down", "choose"),
                ("Enter", "toggle"),
                ("n", "new"),
                ("x", "remove"),
            ],
        }
    }

    /// What went wrong, until the next key says it has been read.
    #[must_use]
    pub fn trouble(&self) -> Option<&str> {
        self.session.last_error.as_deref()
    }

    /// The last thing that went right, until the next key reads it.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn handle(&mut self, key: KeyEvent) {
        // Cleared before the key is acted on, not after: an action that fails
        // again puts its message straight back, and one that succeeds leaves
        // the line to whatever comes next. Anything else would have a stale
        // complaint outlive the thing complained about.
        self.session.last_error = None;
        self.status = None;

        // Ctrl+C is the one key a terminal program may not redefine, whatever
        // else is on screen.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.running = false;
            return;
        }

        // An overlay takes the keys it knows and swallows the rest, so that
        // reading a list cannot change the view behind it.
        if self.overlay.is_some() {
            self.over(key.code);
            return;
        }

        if self.editing {
            self.typing(key.code);
            return;
        }

        // What the view does with a key comes first: a list has to have Up and
        // Down before anything else claims them.
        let taken = match self.tab {
            Tab::Traffic => self.watching(key.code),
            Tab::Scenarios => self.running_scenarios(key.code),
            Tab::HexInject => self.injecting(key.code),
            Tab::Frames => self.framing(key.code),
            Tab::Connections => self.connecting(key.code),
        };
        if taken {
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Keys),
            KeyCode::Char('o') => self.browse(),
            KeyCode::Char('n') if self.tab == Tab::Connections => {
                self.overlay = Some(Overlay::NewConnection(ConnectionForm::default()));
            }
            KeyCode::Tab => self.tab = Tab::at(self.tab.index() + 1),
            KeyCode::BackTab => self.tab = Tab::at(self.tab.index() + Tab::ALL.len() - 1),
            KeyCode::Char(digit @ '1'..='5') => {
                let wanted = digit as usize - '1' as usize;
                self.tab = Tab::at(wanted);
            }
            _ => {}
        }
    }

    /// What an overlay does with a key. Everything else it swallows.
    fn over(&mut self, code: KeyCode) {
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        match overlay {
            Overlay::Keys => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.overlay = None;
                }
            }
            Overlay::NewConnection(form) => match code {
                KeyCode::Tab | KeyCode::Down => form.next(),
                KeyCode::BackTab | KeyCode::Up => form.previous(),
                KeyCode::Left => form.cycle(-1),
                KeyCode::Right => form.cycle(1),
                KeyCode::Char(' ') => form.toggle(),
                KeyCode::Backspace => form.backspace(),
                KeyCode::Char(letter) => form.type_char(letter),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => self.submit_connection(),
                _ => {}
            },
            Overlay::Browse(browser) => match code {
                KeyCode::Down => browser.picker.step(1),
                KeyCode::Up => browser.picker.step(-1),
                KeyCode::Backspace => browser.picker.rubbed_out(),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    // A folder walks, and only a file ends the walk.
                    if let Some(path) = browser.taken() {
                        self.overlay = None;
                        self.open(&path);
                    }
                }
                KeyCode::Char(letter) => browser.picker.typing(letter),
                _ => {}
            },
            Overlay::Pick(picker, purpose) => match code {
                KeyCode::Down => picker.step(1),
                KeyCode::Up => picker.step(-1),
                KeyCode::Backspace => picker.rubbed_out(),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    let taken = picker.taken();
                    let purpose = std::mem::replace(purpose, PickPurpose::DecodeAs);
                    self.overlay = None;
                    if let Some(name) = taken {
                        self.take_picked(purpose, name);
                    }
                }
                KeyCode::Char(letter) => picker.typing(letter),
                _ => {}
            },
            Overlay::EditText(edit) => match code {
                KeyCode::Backspace => {
                    edit.text.pop();
                }
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => self.submit_edit(),
                KeyCode::Char(letter) => edit.text.push(letter),
                _ => {}
            },
        }
    }

    /// Offers the definitions the read row could be, when there is a choice.
    fn pick_frame(&mut self) {
        let Some(seq) = self.monitor().and_then(|monitor| monitor.selected) else {
            return;
        };
        let Some(entry) = self.session.log.iter().find(|entry| entry.seq == seq) else {
            return;
        };
        let candidates: Vec<String> = self
            .session
            .frames
            .frames()
            .filter(|frame| frame.size() == entry.bytes.len())
            .map(|frame| frame.name.clone())
            .collect();
        if !candidates.is_empty() {
            self.overlay = Some(Overlay::Pick(
                Picker::new("Read as", candidates),
                PickPurpose::DecodeAs,
            ));
        }
    }

    /// Offers the disk, starting where the project on show sits.
    fn browse(&mut self) {
        let at = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.looked_in.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        self.overlay = Some(Overlay::Browse(Browser::opening(at)));
    }

    /// Validates the connection form under focus and, on success, opens it.
    ///
    /// Left open on failure, with the reason in the form's own trouble line:
    /// the fields typed so far are worth keeping while it is fixed.
    fn submit_connection(&mut self) {
        let Some(Overlay::NewConnection(form)) = &mut self.overlay else {
            return;
        };
        let Some((id, config)) = form.submit(&self.session.connections) else {
            return;
        };
        let retry = form.auto_reconnect().then(RetryPolicy::standard);
        let autoconnect = form.autoconnect();
        self.engine.connect(id.clone(), config.clone(), retry);
        self.session.connections.push((
            id,
            ConnectionEntry {
                config,
                status: ConnectionStatus::Connecting,
                retry,
                autoconnect,
            },
        ));
        self.overlay = None;
    }

    /// The keys the connection list answers to, and whether it took this one.
    fn connecting(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.pick_connection(1),
            KeyCode::Up | KeyCode::Char('k') => self.pick_connection(-1),
            KeyCode::Enter => self.toggle_selected_connection(),
            KeyCode::Char('a') => self.toggle_selected_autoconnect(),
            KeyCode::Char('x') => self.remove_selected_connection(),
            _ => return false,
        }
        true
    }

    fn pick_connection(&mut self, delta: isize) {
        let Some(last) = self.session.connections.len().checked_sub(1) else {
            return;
        };
        self.connection_at = Some(
            self.connection_at
                .unwrap_or(0)
                .saturating_add_signed(delta)
                .min(last),
        );
    }

    /// The connection a key would act on: the one under the cursor, or the
    /// first one when nothing has been chosen yet.
    fn selected_connection(&self) -> Option<sim_core::ConnectionId> {
        let at = self.connection_at.unwrap_or(0);
        self.session.connections.get(at).map(|(id, _)| id.clone())
    }

    /// Connects a disconnected link with the settings it already has, or
    /// disconnects one that is up.
    fn toggle_selected_connection(&mut self) {
        let Some(id) = self.selected_connection() else {
            return;
        };
        match self.session.status_of(&id) {
            Some(ConnectionStatus::Disconnected) => {
                if let Some((config, retry)) = self.session.begin_reconnect(&id) {
                    self.engine.connect(id, config, retry);
                }
            }
            Some(_) => self.engine.disconnect(id),
            None => {}
        }
    }

    fn toggle_selected_autoconnect(&mut self) {
        let Some(id) = self.selected_connection() else {
            return;
        };
        if let Some(entry) = self.session.connection_mut(&id) {
            entry.autoconnect = !entry.autoconnect;
        }
    }

    /// Removal only reaches a link that is down, the same as the window: one
    /// still up has to be told to stop before it can be forgotten.
    fn remove_selected_connection(&mut self) {
        let Some(id) = self.selected_connection() else {
            return;
        };
        match self.session.status_of(&id) {
            Some(ConnectionStatus::Disconnected) => self.session.remove_connection(&id),
            Some(_) => {
                self.session.last_error = Some(format!(
                    "{}: disconnect it first (Enter), then remove it.",
                    id.0
                ));
            }
            None => {}
        }
    }

    /// The keys the Frames view answers to, and whether it took this one.
    fn framing(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Left => self.frame_focus = FramesFocus::Library,
            KeyCode::Right if self.session.frames.selected_frame().is_some() => {
                self.frame_focus = FramesFocus::Fields;
            }
            KeyCode::Down | KeyCode::Char('j') => self.frame_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.frame_move(-1),
            KeyCode::Char(' ') if self.frame_focus == FramesFocus::Fields => self.toggle_bit(),
            KeyCode::Enter => match self.frame_focus {
                FramesFocus::Library => {
                    if self.session.frames.selected_frame().is_some() {
                        self.frame_focus = FramesFocus::Fields;
                    }
                }
                FramesFocus::Fields => self.edit_selected_field(),
            },
            KeyCode::Char('s') => self.send_selected_frame(),
            KeyCode::Char('t') => self.pick_frame_target(),
            _ => return false,
        }
        true
    }

    /// Offers the connections currently up, to choose which one a frame goes
    /// out on.
    fn pick_frame_target(&mut self) {
        let connected: Vec<String> = self
            .session
            .connections
            .iter()
            .filter(|(_, entry)| entry.status == ConnectionStatus::Connected)
            .map(|(id, _)| id.0.clone())
            .collect();
        if connected.is_empty() {
            self.session.last_error = Some("No connected link to send to.".to_owned());
            return;
        }
        self.overlay = Some(Overlay::Pick(
            Picker::new("Target connection", connected),
            PickPurpose::FrameTarget,
        ));
    }

    fn frame_move(&mut self, delta: isize) {
        match self.frame_focus {
            FramesFocus::Library => self.pick_frame_in_library(delta),
            FramesFocus::Fields => self.pick_field(delta),
        }
    }

    fn pick_frame_in_library(&mut self, delta: isize) {
        let Some(last) = self.session.frames.entries.len().checked_sub(1) else {
            return;
        };
        let at = match self.session.frames.selected {
            Some(at) => at.saturating_add_signed(delta).min(last),
            None => 0,
        };
        self.session.frames.selected = Some(at);
    }

    /// The rows of the frame on show, resetting the cursor when it is not the
    /// frame the cursor was last on.
    fn field_frame_rows(&mut self) -> Option<(sim_core::frame::FrameDef, Vec<FieldRow>)> {
        let frame = self.session.frames.selected_frame()?.clone();
        let rows = field_rows(&frame);
        let fresh = self
            .field_at
            .as_ref()
            .is_none_or(|(name, _)| *name != frame.name);
        if fresh {
            self.field_at = Some((frame.name.clone(), 0));
        }
        Some((frame, rows))
    }

    fn pick_field(&mut self, delta: isize) {
        let Some((frame, rows)) = self.field_frame_rows() else {
            return;
        };
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let at = self.field_at.as_ref().map_or(0, |(_, at)| *at);
        self.field_at = Some((frame.name, at.saturating_add_signed(delta).min(last)));
    }

    /// Flips a single-bit flag under the cursor without going through an
    /// editor: there is nothing to type for a value that is only ever 0 or 1.
    fn toggle_bit(&mut self) {
        let Some((frame, rows)) = self.field_frame_rows() else {
            return;
        };
        let Some(&FieldRow::Bit { field, bit }) =
            self.field_at.as_ref().and_then(|(_, at)| rows.get(*at))
        else {
            return;
        };
        let sim_core::frame::FieldKind::Bits { bits, .. } = &frame.fields[field].kind else {
            return;
        };
        if bits[bit].width != 1 {
            return;
        }
        let name = bits[bit].name.clone();
        let values = self.session.frames.values_mut(&frame);
        let held = values
            .entry(frame.fields[field].name.clone())
            .or_insert_with(|| {
                sim_core::frame::value::Value::Bits(std::collections::BTreeMap::new())
            });
        if let sim_core::frame::value::Value::Bits(set) = held {
            let slot = set.entry(name).or_insert(0);
            *slot = u64::from(*slot == 0);
        }
    }

    /// Opens the right editor for the row under the cursor: a list for an
    /// enum, a toggle already done for a one-bit flag, a box to type into for
    /// everything else. A checksum is computed, not edited.
    fn edit_selected_field(&mut self) {
        let Some((frame, rows)) = self.field_frame_rows() else {
            return;
        };
        let Some(&row) = self.field_at.as_ref().and_then(|(_, at)| rows.get(*at)) else {
            return;
        };

        match row {
            FieldRow::Field(index) => {
                let field = &frame.fields[index];
                match &field.kind {
                    sim_core::frame::FieldKind::Checksum { .. } => {
                        self.session.last_error =
                            Some("Computed automatically, on send.".to_owned());
                    }
                    sim_core::frame::FieldKind::Enum { variants, .. } => {
                        let options: Vec<String> = variants
                            .iter()
                            .map(|variant| format!("{} = {}", variant.name, variant.value))
                            .collect();
                        self.overlay = Some(Overlay::Pick(
                            Picker::new(field.name.clone(), options),
                            PickPurpose::EnumField { field: index },
                        ));
                    }
                    _ => {
                        let values = self.session.frames.values_mut(&frame);
                        let current = values.get(&field.name).cloned();
                        let text = current.map_or_else(String::new, |value| match &field.kind {
                            sim_core::frame::FieldKind::Bytes { .. } => {
                                value.as_bytes().map_or_else(String::new, hex::packed)
                            }
                            sim_core::frame::FieldKind::Text { .. } => {
                                value.as_text().unwrap_or_default().to_owned()
                            }
                            _ => reading::describe(field, &value, false),
                        });
                        self.overlay = Some(Overlay::EditText(EditBox {
                            title: field.name.clone(),
                            text,
                            target: EditTarget::Field(index),
                        }));
                    }
                }
            }
            FieldRow::Bit { field, bit } => {
                let sim_core::frame::FieldKind::Bits { bits, .. } = &frame.fields[field].kind
                else {
                    return;
                };
                if bits[bit].width == 1 {
                    self.toggle_bit();
                    return;
                }
                let values = self.session.frames.values_mut(&frame);
                let held = values
                    .get(&frame.fields[field].name)
                    .and_then(sim_core::frame::value::Value::as_bits)
                    .and_then(|set| set.get(&bits[bit].name))
                    .copied()
                    .unwrap_or(0);
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: bits[bit].name.clone(),
                    text: held.to_string(),
                    target: EditTarget::Bit { field, bit },
                }));
            }
        }
    }

    /// Applies what a picker settled: which frame a row reads as, or which
    /// enum variant a field now holds.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "consumed by one arm and not the other, moving it in either way"
    )]
    fn take_picked(&mut self, purpose: PickPurpose, taken: String) {
        match purpose {
            PickPurpose::DecodeAs => {
                if let Some(monitor) = self.session.monitors.values_mut().next() {
                    monitor.decode_as = Some(taken);
                }
            }
            PickPurpose::EnumField { field } => {
                let Some(frame) = self.session.frames.selected_frame().cloned() else {
                    return;
                };
                // The label carries the value after " = ", which is the part
                // that means something to the encoder.
                let Some(value) = taken.rsplit(" = ").next().and_then(|v| v.parse().ok()) else {
                    return;
                };
                self.session.frames.values_mut(&frame).insert(
                    frame.fields[field].name.clone(),
                    sim_core::frame::value::Value::Uint(value),
                );
            }
            PickPurpose::FrameTarget => {
                self.session.frame_target = Some(sim_core::ConnectionId(taken));
            }
        }
    }

    /// Reads what was typed into the box on show, and writes it into the
    /// field or flag it belongs to.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a typed number is clamped to the field's own width by the encoder, \
                  which is a truer check than one done here on the way in"
    )]
    fn submit_edit(&mut self) {
        let Some(Overlay::EditText(edit)) = &self.overlay else {
            return;
        };
        let text = edit.text.clone();
        let target = match &edit.target {
            EditTarget::Field(index) => EditTarget::Field(*index),
            EditTarget::Bit { field, bit } => EditTarget::Bit {
                field: *field,
                bit: *bit,
            },
        };
        let Some(frame) = self.session.frames.selected_frame().cloned() else {
            self.overlay = None;
            return;
        };

        match target {
            EditTarget::Field(index) => {
                let field = frame.fields[index].clone();
                let written = match &field.kind {
                    sim_core::frame::FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
                        hex::read_number(&text)
                            .map(|value| sim_core::frame::value::Value::Uint(value.max(0.0) as u64))
                    }
                    sim_core::frame::FieldKind::Scalar(
                        sim_core::frame::ScalarType::F32 | sim_core::frame::ScalarType::F64,
                    ) => hex::read_number(&text).map(sim_core::frame::value::Value::Float),
                    sim_core::frame::FieldKind::Scalar(_) => hex::read_number(&text)
                        .map(|value| sim_core::frame::value::Value::Int(value as i64)),
                    sim_core::frame::FieldKind::Bytes { len } => {
                        hex::parse(&text).ok().map(|mut bytes| {
                            bytes.resize(*len, 0);
                            sim_core::frame::value::Value::Bytes(bytes)
                        })
                    }
                    sim_core::frame::FieldKind::Text { len } => {
                        let mut text = text.clone();
                        text.truncate(*len);
                        Some(sim_core::frame::value::Value::Text(text))
                    }
                    sim_core::frame::FieldKind::Enum { .. }
                    | sim_core::frame::FieldKind::Bits { .. }
                    | sim_core::frame::FieldKind::Checksum { .. } => None,
                };
                if let Some(value) = written {
                    self.session
                        .frames
                        .values_mut(&frame)
                        .insert(field.name, value);
                }
            }
            EditTarget::Bit { field, bit } => {
                let sim_core::frame::FieldKind::Bits { bits, .. } = &frame.fields[field].kind
                else {
                    self.overlay = None;
                    return;
                };
                if let Some(value) = hex::read_number(&text) {
                    let name = bits[bit].name.clone();
                    let values = self.session.frames.values_mut(&frame);
                    let held = values
                        .entry(frame.fields[field].name.clone())
                        .or_insert_with(|| {
                            sim_core::frame::value::Value::Bits(std::collections::BTreeMap::new())
                        });
                    if let sim_core::frame::value::Value::Bits(set) = held {
                        set.insert(name, value.max(0.0) as u64);
                    }
                }
            }
        }
        self.overlay = None;
    }

    /// Encodes the chosen frame from the values on show and sends it.
    ///
    /// Failing to encode is reported rather than sent as whatever fell out: a
    /// frame the definition refuses is not a frame the receiver asked for.
    fn send_selected_frame(&mut self) {
        let Some(frame) = self.session.frames.selected_frame().cloned() else {
            return;
        };
        let Some(id) = self.session.frame_target.clone() else {
            self.session.last_error = Some("No target chosen. Press t to pick one.".to_owned());
            return;
        };
        if self.session.status_of(&id) != Some(ConnectionStatus::Connected) {
            self.session.last_error = Some(format!("{} is not connected.", id.0));
            return;
        }

        let values = self.session.frames.values_mut(&frame).clone();
        match codec::encode(&frame, &values) {
            Ok(bytes) => {
                self.status = Some(format!("Sent {} byte(s) to {}.", bytes.len(), id.0));
                self.engine.send_raw(id, bytes);
            }
            Err(error) => self.session.last_error = Some(error.to_string()),
        }
    }

    /// Whether a box has the keyboard.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// What the focused box does with a key. It takes all of them.
    fn typing(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(letter) => self.session.hex_input.push(letter),
            KeyCode::Backspace => {
                self.session.hex_input.pop();
            }
            KeyCode::Esc => self.editing = false,
            KeyCode::Enter => {
                self.inject();
                self.editing = false;
            }
            _ => {}
        }
    }

    /// The keys the injection view answers to while nothing is being typed.
    fn injecting(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Enter | KeyCode::Char('i') => self.editing = true,
            KeyCode::Char('x') => self.session.hex_input.clear(),
            _ => return false,
        }
        true
    }

    /// Sends what is typed, to the link the project named.
    fn inject(&mut self) {
        let Ok(bytes) = hex::parse(&self.session.hex_input) else {
            return;
        };
        let Some(id) = self
            .session
            .hex_target
            .clone()
            .or_else(|| self.session.connections.first().map(|(id, _)| id.clone()))
        else {
            self.session.last_error = Some("No link to send on.".to_owned());
            return;
        };
        self.engine.send_raw(id, bytes);
    }

    /// The keys the scenario list answers to, and whether it took this one.
    fn running_scenarios(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.pick_scenario(1),
            KeyCode::Up | KeyCode::Char('k') => self.pick_scenario(-1),
            KeyCode::Enter => self.start_selected(),
            KeyCode::Char('x') => self.stop_selected(),
            _ => return false,
        }
        true
    }

    fn pick_scenario(&mut self, delta: isize) {
        let held = self.session.scenarios.entries.len();
        let Some(last) = held.checked_sub(1) else {
            return;
        };
        let at = match self.session.scenarios.selected {
            Some(at) => at.saturating_add_signed(delta).min(last),
            None => 0,
        };
        self.session.scenarios.selected = Some(at);
    }

    fn start_selected(&mut self) {
        let Some(scenario) = self.session.scenarios.selected_scenario().cloned() else {
            return;
        };
        scenarios::start(&mut self.session, &self.engine, &scenario);
    }

    /// Stopping is by name, which is what the engine answers to.
    fn stop_selected(&mut self) {
        let Some(name) = self
            .session
            .scenarios
            .selected_scenario()
            .map(|scenario| scenario.name.clone())
        else {
            return;
        };
        if self.session.running.contains_key(&name) {
            self.engine.stop_scenario(name);
        }
    }

    /// The keys the traffic list answers to, and whether it took this one.
    fn watching(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Esc => {
                if let Some(monitor) = self.session.monitors.values_mut().next() {
                    monitor.selected = None;
                }
            }
            KeyCode::Enter | KeyCode::Char('d') => self.pick_frame(),
            KeyCode::Char('f') => {
                if let Some(monitor) = self.session.monitors.values_mut().next() {
                    monitor.follow = !monitor.follow;
                }
            }
            _ => return false,
        }
        true
    }

    /// Moves the read row by `delta`, stopping at either end.
    ///
    /// Reading a row and following the newest frame are opposite things, so the
    /// first stops the second: a list that keeps scrolling moves the row being
    /// read out from under you.
    fn step(&mut self, delta: isize) {
        let seqs: Vec<u64> = self.rows().iter().map(|entry| entry.seq).collect();
        let Some(last) = seqs.len().checked_sub(1) else {
            return;
        };
        let Some(monitor) = self.session.monitors.values_mut().next() else {
            return;
        };

        let at = match monitor
            .selected
            .and_then(|seq| seqs.iter().position(|s| *s == seq))
        {
            Some(at) => at.saturating_add_signed(delta).min(last),
            // Nothing read yet: start at the newest, which is what a bench is
            // looking at when it reaches for the keyboard.
            None => last,
        };
        monitor.selected = Some(seqs[at]);
        monitor.follow = false;
    }
}
