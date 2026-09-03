//! What the terminal front end is showing, and what a key does to it.
//!
//! Kept apart from the drawing so that a key press can be tested without a
//! terminal, the same way the panels are tested without a window.

use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use sim_session::engine_handle::EngineHandle;
use sim_session::project::Project;
use sim_session::reading::{self, Reading};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// The key map.
    Keys,
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
    pub fn overlay(&self) -> Option<Overlay> {
        self.overlay
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.running
    }

    /// The keys every view answers to, in the order they are shown.
    pub const KEYS: [(&'static str, &'static str); 5] = [
        ("1-5", "go to a view"),
        ("Tab", "next view"),
        ("Shift+Tab", "previous view"),
        ("?", "keys"),
        ("q", "quit"),
    ];

    /// What the view on show adds to them.
    ///
    /// Offered without being asked for, since a key nobody can guess is a key
    /// nobody presses.
    #[must_use]
    pub fn view_keys(&self) -> &'static [(&'static str, &'static str)] {
        match self.tab {
            Tab::Traffic => &[
                ("up/down", "read a row"),
                ("Esc", "put it away"),
                ("f", "follow"),
            ],
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
        // reading the key map cannot change the view behind it.
        if self.overlay.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.overlay = None;
            }
            return;
        }

        // What the view does with a key comes first: a list has to have Up and
        // Down before anything else claims them.
        if self.tab == Tab::Traffic && self.watching(key.code) {
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
