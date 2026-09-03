//! What the terminal front end is showing, and what a key does to it.
//!
//! Kept apart from the drawing so that a key press can be tested without a
//! terminal, the same way the panels are tested without a window.

use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use sim_session::engine_handle::EngineHandle;
use sim_session::project::Project;
use sim_session::reading::{self, Reading};
use sim_session::scenarios;
use sim_session::state::{LogEntry, MonitorState, Session};

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

    /// What the view will hold, said plainly until it holds it.
    #[must_use]
    pub fn pending(self) -> &'static str {
        match self {
            Self::Connections => "The links, their state, and what it takes to open one.",
            Self::Traffic => "Every frame sent and received, and the fields behind a row.",
            Self::HexInject => "Bytes typed by hand, sent as they are.",
            Self::Frames => "The frame definitions, their fields, and the shared types.",
            Self::Scenarios => "The scenarios, what each step does, and which are running.",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn at(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }
}

/// What is laid over the view, taking the keys the view would otherwise get.
pub enum Overlay {
    /// The key map.
    Keys,
    /// One answer to be chosen from a list.
    Pick(Picker),
}

/// A list to choose one line from, narrowed by what is typed.
///
/// The window has combo boxes. A terminal has this, and it is the same idea:
/// the choice is offered rather than spelled, so a name that does not exist
/// cannot be given.
pub struct Picker {
    pub title: &'static str,
    options: Vec<String>,
    typed: String,
    at: usize,
}

impl Picker {
    fn new(title: &'static str, options: Vec<String>) -> Self {
        Self {
            title,
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
    running: bool,
    session: Session,
    engine: EngineHandle,
    /// The project this was opened with, for the header to name.
    path: Option<PathBuf>,
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
            running: true,
            session,
            engine: EngineHandle::new(),
            path: None,
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
            Some(path) => app.open(&path),
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

    /// Moving between views, which every view answers to.
    pub const KEYS: [(&'static str, &'static str); 3] = [
        ("1-5", "go to a view"),
        ("Tab", "next view"),
        ("Shift+Tab", "previous view"),
    ];

    /// The way out, and the way to the rest.
    ///
    /// Kept apart because these two are never dropped for want of room: not
    /// knowing how to leave a terminal program is how a session gets killed
    /// from another window.
    pub const ESCAPES: [(&'static str, &'static str); 2] = [("?", "keys"), ("q", "quit")];

    /// What the view on show adds to them.
    ///
    /// Offered without being asked for, since a key nobody can guess is a key
    /// nobody presses.
    #[must_use]
    pub fn view_keys(&self) -> &'static [(&'static str, &'static str)] {
        // An overlay has the keyboard, so it is its keys that are worth the
        // room: the view behind it answers to nothing while it is up.
        match self.overlay {
            Some(Overlay::Pick(_)) => {
                return &[
                    ("up/down", "choose"),
                    ("type", "narrow"),
                    ("Enter", "take it"),
                    ("Esc", "back"),
                ]
            }
            Some(Overlay::Keys) => return &[],
            None => {}
        }

        match self.tab {
            Tab::Traffic => &[
                ("up/down", "read a row"),
                ("Enter", "read as"),
                ("Esc", "put it away"),
                ("f", "follow"),
            ],
            Tab::Scenarios => &[("up/down", "choose"), ("Enter", "run"), ("x", "stop")],
            _ => &[],
        }
    }

    pub fn handle(&mut self, key: KeyEvent) {
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

        // What the view does with a key comes first: a list has to have Up and
        // Down before anything else claims them.
        let taken = match self.tab {
            Tab::Traffic => self.watching(key.code),
            Tab::Scenarios => self.running_scenarios(key.code),
            _ => false,
        };
        if taken {
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Keys),
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
            Overlay::Pick(picker) => match code {
                KeyCode::Down => picker.step(1),
                KeyCode::Up => picker.step(-1),
                KeyCode::Backspace => picker.rubbed_out(),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    let taken = picker.taken();
                    self.overlay = None;
                    if let (Some(name), Some(monitor)) =
                        (taken, self.session.monitors.values_mut().next())
                    {
                        monitor.decode_as = Some(name);
                    }
                }
                KeyCode::Char(letter) => picker.typing(letter),
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
            self.overlay = Some(Overlay::Pick(Picker::new("Read as", candidates)));
        }
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
