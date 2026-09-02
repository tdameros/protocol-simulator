//! What the terminal front end is showing, and what a key does to it.
//!
//! Kept apart from the drawing so that a key press can be tested without a
//! terminal, the same way the panels are tested without a window.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

#[derive(Debug)]
pub struct App {
    tab: Tab,
    help: bool,
    running: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            tab: Tab::Connections,
            help: false,
            running: true,
        }
    }
}

impl App {
    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    #[must_use]
    pub fn help_is_open(&self) -> bool {
        self.help
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.running
    }

    /// The key map, in the order it is shown.
    pub const KEYS: [(&'static str, &'static str); 5] = [
        ("1-5", "go to a view"),
        ("Tab", "next view"),
        ("Shift+Tab", "previous view"),
        ("?", "keys"),
        ("q", "quit"),
    ];

    pub fn handle(&mut self, key: KeyEvent) {
        // Ctrl+C is the one key a terminal program may not redefine, whatever
        // else is on screen.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.running = false;
            return;
        }

        // An overlay takes the keys it knows and swallows the rest, so that
        // reading the key map cannot change the view behind it.
        if self.help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.help = false;
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.tab = Tab::at(self.tab.index() + 1),
            KeyCode::BackTab => self.tab = Tab::at(self.tab.index() + Tab::ALL.len() - 1),
            KeyCode::Char(digit @ '1'..='5') => {
                let wanted = digit as usize - '1' as usize;
                self.tab = Tab::at(wanted);
            }
            _ => {}
        }
    }
}
