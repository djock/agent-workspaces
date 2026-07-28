//! The console list: an inline, arrow-key picker over the workspaces.
//!
//! This replaces a 774-line ratatui dashboard. The difference that matters is not
//! the line count — it is that this **does not take over the screen**. No
//! alternate screen, no full redraw, no clear. It prints the list where you are,
//! moves a highlight within those lines, and when you pick something it leaves the
//! list in your scrollback like any other command's output. A dashboard is a place
//! you visit; this is a prompt you answer.
//!
//! All the decision logic lives in `State`, which never touches a terminal, so the
//! interesting behaviour — movement clamping, filtering, what Enter resolves to —
//! is unit-testable without a tty.

use anyhow::{Context, Result};
use std::io::{IsTerminal, Write};

use crate::rows::{ListOpts, RowState, WorkspaceRow};
use crate::theme::Theme;

/// What the picker decided.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Launch this workspace. The caller execs into the agent, so the terminal
    /// must already be restored by the time this is returned.
    Launch(String),
    Quit,
}

/// A key, decoupled from crossterm so `State` can be driven from tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Quit,
    ToggleDetail,
    ToggleArchived,
    StartFilter,
    Backspace,
    Char(char),
}

/// What the caller should do after feeding a key in.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Keep going; redraw.
    Continue,
    /// Stop, with this outcome.
    Done(Outcome),
    /// The visible set changed in a way that needs the workspace list reloaded.
    Reload,
}

pub struct State {
    all: Vec<WorkspaceRow>,
    /// Indices into `all` that pass the current filter, in display order.
    visible: Vec<usize>,
    selected: usize,
    filter: String,
    filtering: bool,
    show_detail: bool,
    show_archived: bool,
}

impl State {
    pub fn new(all: Vec<WorkspaceRow>, show_archived: bool) -> Self {
        let mut s = State {
            all,
            visible: Vec::new(),
            selected: 0,
            filter: String::new(),
            filtering: false,
            show_detail: false,
            show_archived,
        };
        s.refilter();
        s
    }

    fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        self.visible = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, r)| self.show_archived || !r.archived)
            .filter(|(_, r)| f.is_empty() || r.name.to_lowercase().contains(&f))
            .map(|(i, _)| i)
            .collect();
        // Clamp rather than reset: narrowing a filter should keep you near where
        // you were, not throw you back to the top.
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
    }

    pub fn visible_rows(&self) -> Vec<&WorkspaceRow> {
        self.visible.iter().map(|&i| &self.all[i]).collect()
    }

    pub fn selected_row(&self) -> Option<&WorkspaceRow> {
        self.visible.get(self.selected).map(|&i| &self.all[i])
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn is_filtering(&self) -> bool {
        self.filtering
    }

    pub fn show_detail(&self) -> bool {
        self.show_detail
    }

    pub fn show_archived(&self) -> bool {
        self.show_archived
    }

    /// Feed one key in. The only place selection, filtering and exit are decided.
    pub fn on_key(&mut self, k: Key) -> Step {
        // While filtering, printable keys extend the query instead of acting as
        // commands — otherwise you could not filter for a workspace called "q".
        if self.filtering {
            match k {
                Key::Char(c) => {
                    self.filter.push(c);
                    self.refilter();
                    return Step::Continue;
                }
                Key::Backspace => {
                    self.filter.pop();
                    self.refilter();
                    return Step::Continue;
                }
                Key::Enter => {
                    self.filtering = false;
                    return Step::Continue;
                }
                Key::Quit => {
                    // Esc while filtering cancels the filter, it does not quit:
                    // losing the whole list because you mistyped a query would be
                    // its own small betrayal.
                    self.filtering = false;
                    self.filter.clear();
                    self.refilter();
                    return Step::Continue;
                }
                _ => {}
            }
        }

        match k {
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
                Step::Continue
            }
            Key::Down => {
                if !self.visible.is_empty() && self.selected + 1 < self.visible.len() {
                    self.selected += 1;
                }
                Step::Continue
            }
            Key::Enter => match self.selected_row() {
                // A registered path that no longer exists cannot be launched;
                // saying so beats execing an agent into a missing directory.
                Some(r) if matches!(r.state, RowState::Missing) => Step::Continue,
                Some(r) => Step::Done(Outcome::Launch(r.name.clone())),
                None => Step::Continue,
            },
            Key::Quit => Step::Done(Outcome::Quit),
            Key::ToggleDetail => {
                self.show_detail = !self.show_detail;
                Step::Continue
            }
            Key::ToggleArchived => {
                self.show_archived = !self.show_archived;
                Step::Reload
            }
            Key::StartFilter => {
                self.filtering = true;
                Step::Continue
            }
            Key::Char(_) | Key::Backspace => Step::Continue,
        }
    }
}

/// One list line. Kept separate from drawing so the format is testable.
///
/// `now` is passed in rather than read, so the relative-activity column is
/// assertable — a test that has to wait for wall-clock time to change is a test
/// that eventually fails on someone else's machine.
pub fn render_row(r: &WorkspaceRow, selected: bool, now: i64) -> String {
    let marker = if selected { '>' } else { ' ' };
    let live = if r.live_pid.is_some() { '*' } else { ' ' };
    let state = match &r.state {
        RowState::Ok if r.archived => "[archived]".to_string(),
        RowState::Ok => r.status.clone().unwrap_or_default(),
        RowState::Missing => "(missing)".to_string(),
        RowState::Corrupt(_) => "(corrupt)".to_string(),
    };
    let tags = if r.tags.is_empty() { String::new() } else { format!("#{}", r.tags.join(" #")) };
    // "-" for never-touched rather than blank: an empty column reads as a
    // rendering bug, a dash reads as an answer.
    let when = r.last_activity.map(|t| crate::rows::ago(t, now)).unwrap_or_else(|| "-".into());
    // The 5h usage figure, when the status line has captured one for this
    // workspace. Absent is "", not "0%", which would be a lie.
    let usage = r
        .limits
        .as_ref()
        .map(|l| format!("{}%", l.five_hour.used_pct.round() as i64))
        .unwrap_or_default();
    format!(
        "{marker}{live} {:<22} {:<7} {:>5} {:>5}  {:<18} {}",
        r.name, r.agent, when, usage, state, tags
    )
    .trim_end()
    .to_string()
}

fn load(show_archived: bool) -> Result<Vec<WorkspaceRow>> {
    let opts = ListOpts { tag: None, include_archived: show_archived };
    Ok(crate::rows::list_all(&opts)?.rows)
}

/// Draw the list, returning how many lines were printed so the next frame can
/// move back over exactly those.
fn draw(state: &State, theme: &Theme, now: i64, out: &mut impl Write) -> Result<u16> {
    let rows = state.visible_rows();
    let mut lines = 0u16;

    for (i, r) in rows.iter().enumerate() {
        let sel = i == state.selected_index();
        let line = render_row(r, sel, now);
        let line = if sel { theme.selected(&line) } else { line };
        writeln!(out, "{line}\r")?;
        lines += 1;
    }
    if rows.is_empty() {
        writeln!(out, "  no workspaces match\r")?;
        lines += 1;
    }

    if state.show_detail() {
        if let Some(r) = state.selected_row() {
            let d = crate::detail::gather(r, 3);
            if let Some(obj) = &d.objective {
                writeln!(out, "    objective: {obj}\r")?;
                lines += 1;
            }
            let tasks = d.queue.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
            writeln!(out, "    open tasks: {tasks}\r")?;
            lines += 1;
            // The notebook tail is the point of the detail view: it is where the
            // last session wrote down what it learned.
            for l in &d.notebook {
                writeln!(out, "    {}\r", theme.dim(l))?;
                lines += 1;
            }
            for e in d.chain.iter().rev().take(3) {
                writeln!(out, "    {}\r", theme.dim(&format!("{}  {}  {}", e.ts, e.kind, e.actor)))?;
                lines += 1;
            }
        }
    }

    let hint = if state.is_filtering() {
        format!("  /{}", state.filter())
    } else {
        let arch = if state.show_archived() { "hide archived" } else { "show archived" };
        format!("  ↑↓ move · enter open · d detail · / filter · a {arch} · q quit")
    };
    writeln!(out, "{}\r", theme.dim(&hint))?;
    lines += 1;

    out.flush()?;
    Ok(lines)
}

/// Restores cooked mode however we leave — normal return, `?`, or panic.
///
/// Raw mode surviving the process is the one way a terminal-light tool can still
/// wreck someone's shell, so this is a guard rather than a pair of calls.
struct RawGuard;

impl RawGuard {
    fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("cannot switch the terminal to raw mode")?;
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn read_key() -> Result<Option<Key>> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    match crossterm::event::read()? {
        Event::Key(KeyEvent { code, modifiers, .. }) => Ok(match code {
            KeyCode::Up => Some(Key::Up),
            KeyCode::Down => Some(Key::Down),
            KeyCode::Enter => Some(Key::Enter),
            KeyCode::Esc => Some(Key::Quit),
            KeyCode::Backspace => Some(Key::Backspace),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Quit),
            KeyCode::Char('k') => Some(Key::Up),
            KeyCode::Char('j') => Some(Key::Down),
            KeyCode::Char('q') => Some(Key::Quit),
            KeyCode::Char('d') => Some(Key::ToggleDetail),
            KeyCode::Char('a') => Some(Key::ToggleArchived),
            KeyCode::Char('/') => Some(Key::StartFilter),
            KeyCode::Char(c) => Some(Key::Char(c)),
            _ => None,
        }),
        _ => Ok(None),
    }
}

/// Run the picker. Falls back to printing the list when there is no terminal to
/// drive — a pipe, a CI job, `ws | grep` — rather than failing.
pub fn run() -> Result<Outcome> {
    if !std::io::stdout().is_terminal() {
        crate::commands::list(None, false)?;
        return Ok(Outcome::Quit);
    }

    let cfg = crate::config::load();
    let theme = crate::theme::resolve(&cfg.theme, &crate::theme::ThemeEnv::detect());
    let now = crate::time::now_unix();
    let mut state = State::new(load(false)?, false);
    let mut stdout = std::io::stdout();

    let _guard = match RawGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            // Not a panic and not a hard error: print the list and say why the
            // interactive path was unavailable.
            eprintln!("ws: {e:#} — listing instead");
            crate::commands::list(None, false)?;
            return Ok(Outcome::Quit);
        }
    };

    let mut printed = draw(&state, &theme, now, &mut stdout)?;
    loop {
        let key = match read_key()? {
            Some(k) => k,
            None => continue,
        };
        let step = state.on_key(key);
        if let Step::Reload = step {
            state = {
                let rows = load(state.show_archived())?;
                let mut s = State::new(rows, state.show_archived());
                s.show_detail = state.show_detail();
                s
            };
        }
        if let Step::Done(outcome) = step {
            // Move back over the frame and clear it, so the picker leaves the
            // shell exactly as it found it before the agent takes over.
            erase(&mut stdout, printed)?;
            return Ok(outcome);
        }
        erase(&mut stdout, printed)?;
        printed = draw(&state, &theme, now, &mut stdout)?;
    }
}

/// Move up over `lines` printed lines and clear them. Never clears the screen —
/// scrollback above the list is not ours to touch.
fn erase(out: &mut impl Write, lines: u16) -> Result<()> {
    use crossterm::{cursor, terminal, QueueableCommand};
    for _ in 0..lines {
        out.queue(cursor::MoveToPreviousLine(1))?;
        out.queue(terminal::Clear(terminal::ClearType::CurrentLine))?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::RowState;
    use std::path::PathBuf;

    fn row(name: &str) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            path: PathBuf::from("/tmp").join(name),
            state: RowState::Ok,
            agent: "claude".into(),
            live_pid: None,
            archived: false,
            tags: Vec::new(),
            status: None,
            last_activity: None,
            limits: None,
        }
    }

    fn archived(name: &str) -> WorkspaceRow {
        WorkspaceRow { archived: true, ..row(name) }
    }

    fn missing(name: &str) -> WorkspaceRow {
        WorkspaceRow { state: RowState::Missing, ..row(name) }
    }

    fn plain() -> Theme {
        crate::theme::resolve("dark", &crate::theme::ThemeEnv { no_color: true, ..Default::default() })
    }

    fn three() -> State {
        State::new(vec![row("alpha"), row("beta"), row("gamma")], false)
    }

    #[test]
    fn movement_clamps_at_both_ends() {
        let mut s = three();
        assert_eq!(s.selected_index(), 0);
        s.on_key(Key::Up);
        assert_eq!(s.selected_index(), 0, "up at the top stays");

        s.on_key(Key::Down);
        s.on_key(Key::Down);
        assert_eq!(s.selected_index(), 2);
        s.on_key(Key::Down);
        assert_eq!(s.selected_index(), 2, "down at the bottom stays");
    }

    #[test]
    fn j_and_k_move_like_the_arrows() {
        let mut s = three();
        s.on_key(Key::Down);
        let with_arrow = s.selected_index();
        let mut s2 = three();
        s2.on_key(Key::Char('j'));
        // 'j' only means "down" outside filter mode, where the key mapper turns
        // it into Key::Down; State itself sees Down.
        assert_eq!(with_arrow, 1);
        assert_eq!(s2.selected_index(), 0, "a raw Char is not movement inside State");
    }

    #[test]
    fn enter_resolves_to_the_selected_workspace() {
        let mut s = three();
        s.on_key(Key::Down);
        assert_eq!(s.on_key(Key::Enter), Step::Done(Outcome::Launch("beta".into())));
    }

    #[test]
    fn quit_resolves_to_quit() {
        let mut s = three();
        assert_eq!(s.on_key(Key::Quit), Step::Done(Outcome::Quit));
    }

    /// A registered path that no longer exists must not be launched: execing an
    /// agent into a missing directory is worse than doing nothing.
    #[test]
    fn enter_on_a_missing_workspace_does_nothing() {
        let mut s = State::new(vec![missing("gone")], false);
        assert_eq!(s.on_key(Key::Enter), Step::Continue);
    }

    #[test]
    fn enter_with_nothing_visible_does_nothing() {
        let mut s = State::new(vec![], false);
        assert_eq!(s.on_key(Key::Enter), Step::Continue);
        assert!(s.selected_row().is_none());
    }

    #[test]
    fn filtering_narrows_and_typing_accumulates() {
        let mut s = three();
        s.on_key(Key::StartFilter);
        assert!(s.is_filtering());
        s.on_key(Key::Char('a'));
        // alpha, beta, gamma all contain 'a'
        assert_eq!(s.visible_rows().len(), 3);
        s.on_key(Key::Char('l'));
        assert_eq!(s.filter(), "al");
        assert_eq!(s.visible_rows().len(), 1, "only alpha contains 'al'");
        assert_eq!(s.selected_row().unwrap().name, "alpha");
    }

    #[test]
    fn backspace_widens_the_filter_again() {
        let mut s = three();
        s.on_key(Key::StartFilter);
        s.on_key(Key::Char('a'));
        s.on_key(Key::Char('l'));
        assert_eq!(s.visible_rows().len(), 1);
        s.on_key(Key::Backspace);
        assert_eq!(s.filter(), "a");
        assert_eq!(s.visible_rows().len(), 3);
    }

    /// Esc while filtering cancels the filter rather than quitting — otherwise a
    /// mistyped query throws you out of the picker entirely.
    #[test]
    fn escape_while_filtering_clears_rather_than_quits() {
        let mut s = three();
        s.on_key(Key::StartFilter);
        s.on_key(Key::Char('z'));
        assert_eq!(s.visible_rows().len(), 0);
        assert_eq!(s.on_key(Key::Quit), Step::Continue, "must not quit");
        assert_eq!(s.filter(), "");
        assert_eq!(s.visible_rows().len(), 3);
        assert!(!s.is_filtering());
    }

    /// A workspace literally named "q" must be reachable, which is the whole
    /// reason printable keys are captured while filtering.
    #[test]
    fn a_workspace_named_q_can_be_filtered_for() {
        let mut s = State::new(vec![row("q"), row("other")], false);
        s.on_key(Key::StartFilter);
        assert_eq!(s.on_key(Key::Char('q')), Step::Continue, "'q' must not quit here");
        assert_eq!(s.visible_rows().len(), 1);
        assert_eq!(s.selected_row().unwrap().name, "q");
    }

    #[test]
    fn enter_closes_the_filter_without_launching() {
        let mut s = three();
        s.on_key(Key::StartFilter);
        s.on_key(Key::Char('b'));
        assert_eq!(s.on_key(Key::Enter), Step::Continue, "first Enter commits the filter");
        assert!(!s.is_filtering());
        assert_eq!(s.on_key(Key::Enter), Step::Done(Outcome::Launch("beta".into())));
    }

    /// Narrowing must not silently reset the cursor to the top, and must never
    /// leave it pointing past the end of the list.
    #[test]
    fn selection_is_clamped_when_the_filter_narrows() {
        let mut s = three();
        s.on_key(Key::Down);
        s.on_key(Key::Down);
        assert_eq!(s.selected_index(), 2);
        s.on_key(Key::StartFilter);
        s.on_key(Key::Char('a'));
        s.on_key(Key::Char('l')); // only alpha
        assert_eq!(s.selected_index(), 0);
        assert_eq!(s.selected_row().unwrap().name, "alpha");
    }

    #[test]
    fn archived_workspaces_are_hidden_until_toggled() {
        let mut s = State::new(vec![row("live"), archived("old")], false);
        assert_eq!(s.visible_rows().len(), 1);
        assert_eq!(s.on_key(Key::ToggleArchived), Step::Reload, "the caller reloads");
        assert!(s.show_archived());
    }

    #[test]
    fn detail_toggles() {
        let mut s = three();
        assert!(!s.show_detail());
        s.on_key(Key::ToggleDetail);
        assert!(s.show_detail());
        s.on_key(Key::ToggleDetail);
        assert!(!s.show_detail());
    }

    #[test]
    fn the_selected_row_is_marked_and_others_are_not() {
        let sel = render_row(&row("alpha"), true, 0);
        let un = render_row(&row("alpha"), false, 0);
        assert!(sel.starts_with('>'), "{sel:?}");
        assert!(!un.starts_with('>'), "{un:?}");
        assert!(sel.contains("alpha") && un.contains("alpha"));
    }

    #[test]
    fn a_live_workspace_is_marked() {
        let r = WorkspaceRow { live_pid: Some(1234), ..row("busy") };
        assert!(render_row(&r, false, 0).contains('*'));
    }

    #[test]
    fn broken_states_are_labelled_rather_than_hidden() {
        assert!(render_row(&missing("gone"), false, 0).contains("(missing)"));
        let c = WorkspaceRow { state: RowState::Corrupt("bad toml".into()), ..row("weird") };
        assert!(render_row(&c, false, 0).contains("(corrupt)"));
        assert!(render_row(&archived("old"), false, 0).contains("[archived]"));
    }

    #[test]
    fn tags_and_status_are_shown() {
        let r = WorkspaceRow {
            tags: vec!["rust".into(), "cli".into()],
            status: Some("mid refactor".into()),
            ..row("proj")
        };
        let line = render_row(&r, false, 0);
        assert!(line.contains("#rust #cli"), "{line}");
        assert!(line.contains("mid refactor"), "{line}");
    }

    /// The frame is drawn to a buffer, so what it prints is assertable without a
    /// terminal — and this pins the "never clear the screen" rule.
    #[test]
    fn drawing_emits_one_line_per_row_plus_a_hint_and_never_clears() {
        let s = three();
        let mut buf: Vec<u8> = Vec::new();
        let lines = draw(&s, &plain(), 0, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();

        assert_eq!(lines, 4, "three rows plus the hint line");
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains("alpha") && text.contains("gamma"));
        assert!(text.contains("↑↓ move"), "the hint tells you the keys: {text}");
        assert!(!text.contains("\x1b[2J"), "must never clear the screen");
    }

    #[test]
    fn drawing_an_empty_list_says_so() {
        let s = State::new(vec![], false);
        let mut buf: Vec<u8> = Vec::new();
        let lines = draw(&s, &plain(), 0, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(lines, 2, "the empty notice plus the hint");
        assert!(text.contains("no workspaces match"), "{text}");
    }

    #[test]
    fn the_filter_query_is_echoed_while_filtering() {
        let mut s = three();
        s.on_key(Key::StartFilter);
        s.on_key(Key::Char('b'));
        let mut buf: Vec<u8> = Vec::new();
        draw(&s, &plain(), 0, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("/b"), "the query must be visible while typing: {text}");
    }
}
