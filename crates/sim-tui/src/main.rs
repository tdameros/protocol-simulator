#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod app;
mod connection_form;
mod ui;

#[cfg(test)]
mod tui_tests;

use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use app::App;

/// How long a pass waits for a key before looking at the engine again.
///
/// Frames arrive on their own, so the loop cannot sit on the keyboard. Short
/// enough that a rate looks live, long enough that an idle bench costs a board
/// nothing.
const TICK: Duration = Duration::from_millis(100);

fn main() -> std::io::Result<()> {
    // One positional argument, as the window takes: a project file, or a folder
    // of frame definitions. No file picker, since the machine this runs on is
    // usually reached over ssh and has no desktop to put one on.
    let opened_with = std::env::args().nth(1).map(PathBuf::from);

    // Raw mode, the alternate screen, and a panic hook that puts the terminal
    // back. Restoring is the whole point: a front end that leaves a broken
    // shell behind on a board reached over ssh is worse than no front end.
    let mut terminal = ratatui::init();
    let outcome = run(&mut terminal, opened_with);
    ratatui::restore();
    outcome
}

fn run(terminal: &mut DefaultTerminal, opened_with: Option<PathBuf>) -> std::io::Result<()> {
    let mut app = App::opening(opened_with);

    // Entering the alternate screen is not enough on every serial console: some
    // pass the escape code through without acting on it, leaving whatever was
    // scrolled by underneath showing through the parts this never repaints. An
    // explicit clear forces every cell to be drawn once, over that history.
    terminal.clear()?;

    while app.running() {
        app.take_engine_events();
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                // Windows reports the release as well, and acting on both would
                // move two tabs for one press.
                if key.kind == KeyEventKind::Press {
                    app.handle(key);
                }
            }
        }
    }

    Ok(())
}
