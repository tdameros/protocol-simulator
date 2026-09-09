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
use ratatui::crossterm::terminal::EnterAlternateScreen;
use ratatui::layout::Rect;
use ratatui::{DefaultTerminal, TerminalOptions, Viewport};

use app::App;

/// How long a pass waits for a key before looking at the engine again.
///
/// Frames arrive on their own, so the loop cannot sit on the keyboard. Short
/// enough that a rate looks live, long enough that an idle bench costs a board
/// nothing.
const TICK: Duration = Duration::from_millis(100);

/// What a terminal that cannot say how big it is gets instead.
///
/// `$COLUMNS`/`$LINES` first, since a shell that tracks them at all is
/// usually tracking the real size; a plain 80x24 otherwise, which is what a
/// serial console tends to actually be.
const FALLBACK_SIZE: (u16, u16) = (80, 24);

fn main() -> std::io::Result<()> {
    // One positional argument, as the window takes: a project file, or a folder
    // of frame definitions. No file picker, since the machine this runs on is
    // usually reached over ssh and has no desktop to put one on.
    let opened_with = std::env::args().nth(1).map(PathBuf::from);

    // Raw mode, the alternate screen, and a panic hook that puts the terminal
    // back. Restoring is the whole point: a front end that leaves a broken
    // shell behind on a board reached over ssh is worse than no front end.
    let mut terminal = init_terminal()?;
    let outcome = run(&mut terminal, opened_with);
    ratatui::restore();
    outcome
}

/// Ratatui's own init, sidestepping the one call in it that a serial console
/// commonly cannot answer.
///
/// `ratatui::init` sizes the alternate screen by asking the terminal how big
/// it is, `TIOCGWINSZ` under the hood, and panics if that answer never comes.
/// A serial line with nothing behind that ioctl fails it outright rather than
/// answering wrong, which used to mean no display at all: raw mode and the
/// alternate screen still took hold, keys still reached the app, and nothing
/// was ever drawn to a viewport that was never sized. Asking first and
/// falling back to a fixed size, which needs no answer at all, means a board
/// like that gets a usable if not perfectly sized screen instead of a panic.
fn init_terminal() -> std::io::Result<DefaultTerminal> {
    // A zero answer is as useless as no answer: a viewport with no area would
    // still draw nothing, just without the courtesy of failing to say so.
    if matches!(ratatui::crossterm::terminal::size(), Ok((width, height)) if width > 0 && height > 0)
    {
        return ratatui::try_init();
    }

    let (width, height) = fallback_size();
    let terminal = ratatui::try_init_with_options(TerminalOptions {
        viewport: Viewport::Fixed(Rect::new(0, 0, width, height)),
    })?;
    // try_init_with_options leaves the alternate screen alone, on purpose: not
    // every caller of it wants one, but this one does, for the same reason
    // main's own comment gives. `run` clears it before drawing either way.
    ratatui::crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    Ok(terminal)
}

fn fallback_size() -> (u16, u16) {
    let dimension = |name: &str| -> Option<u16> {
        std::env::var(name)
            .ok()?
            .trim()
            .parse()
            .ok()
            .filter(|&n| n > 0)
    };
    (
        dimension("COLUMNS").unwrap_or(FALLBACK_SIZE.0),
        dimension("LINES").unwrap_or(FALLBACK_SIZE.1),
    )
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
