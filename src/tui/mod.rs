//! The ratatui dashboard. `run()` owns the terminal; everything it needs to
//! decide is computed by `app` and drawn by `render`, both terminal-free.
pub mod app;
pub mod detail;
pub mod render;
pub mod theme;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use self::app::{Action, App};

/// What the TUI wants done after it gives the terminal back.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Quit,
    /// The user picked a workspace. The caller launches it *after* `run()` has
    /// restored the terminal — the agent then takes over this same terminal,
    /// which is what spec §13 means by "opens in the current terminal".
    Launch(String),
}

/// Draw/handle-key loop. Restores the terminal on every exit path, including
/// an error, so a panic-free failure never leaves the user in raw mode.
pub fn run() -> Result<Outcome> {
    // I4: load archived rows too and let `App::show_archived` (toggled with
    // `A`) decide what is on screen. Filtering them out here is what made `a`
    // a one-way door with no in-TUI way back.
    let rows = crate::rows::list_workspaces(&crate::rows::ListOpts {
        tag: None,
        include_archived: true,
    })?;
    let cfg = crate::config::load();
    let theme = theme::resolve(&cfg.theme, &theme::ThemeEnv::detect());
    let mut app = App::with_theme(rows, crate::limits::now_epoch(), theme);

    // I1: `ratatui::init()` is `try_init().expect(..)`, so `ws -tui` in a
    // script, a pipe, or a TTY-less SSH session panicked with exit 101 and a
    // stray escape sequence on stdout. `-tui` is a documented flag; asking for
    // it without a terminal deserves a sentence, not a backtrace.
    let mut term = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            // try_init enables raw mode, enters the alternate screen, then
            // builds the terminal — and can fail at any of the three, so undo
            // it. Raw mode is undone unconditionally (crossterm sets it via
            // the controlling tty, which can succeed even when stdout is a
            // pipe: `ws -tui | cat` would otherwise leave the shell in raw
            // mode). The alternate-screen sequence is only written back to a
            // real terminal — into a pipe it is exactly the stray escape
            // sequence on stdout that this fix exists to remove.
            use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
            let _ = disable_raw_mode();
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                let _ = ratatui::crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
            }
            return Err(anyhow::Error::new(e).context("-tui requires a terminal"));
        }
    };
    let result = event_loop(&mut term, &mut app);
    ratatui::restore();
    result
}

fn event_loop(term: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<Outcome> {
    loop {
        // Gather the detail pane here, not inside `render` — `render` takes
        // `&App` so it can stay pure and terminal-I/O-free; this is the one
        // `&mut` touch per loop that keeps the cache fresh for whatever
        // `render_detail` reads through `app.cached_detail()`.
        app.detail();
        term.draw(|f| render::render(f, app))?;
        let Event::Key(KeyEvent { code, kind, modifiers, .. }) = event::read()? else {
            continue;
        };
        // Key *releases* arrive as separate events on some terminals; acting on
        // both would double every keystroke.
        if kind != KeyEventKind::Press {
            continue;
        }
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return Ok(Outcome::Quit);
        }
        match app.on_key(code) {
            Action::Quit => return Ok(Outcome::Quit),
            Action::Launch(name) => return Ok(Outcome::Launch(name)),
            Action::None => {}
        }
    }
}
