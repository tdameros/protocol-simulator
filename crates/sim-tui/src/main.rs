#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod app;
mod ui;

#[cfg(test)]
mod tui_tests;

use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use app::App;

fn main() -> std::io::Result<()> {
    // Raw mode, the alternate screen, and a panic hook that puts the terminal
    // back. Restoring is the whole point: a front end that leaves a broken
    // shell behind on a board reached over ssh is worse than no front end.
    let mut terminal = ratatui::init();
    let outcome = run(&mut terminal);
    ratatui::restore();
    outcome
}

fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    let mut app = App::default();

    while app.running() {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        // Blocking, which is right while nothing arrives on its own. The engine
        // will want a poll with a timeout so that a frame landing redraws
        // without a key being pressed.
        if let Event::Key(key) = event::read()? {
            // Windows reports the release as well, and acting on both would
            // move two tabs for one press.
            if key.kind == KeyEventKind::Press {
                app.handle(key);
            }
        }
    }

    Ok(())
}
