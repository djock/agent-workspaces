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
use std::path::{Path, PathBuf};

use crate::detail::Detail;
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
///
/// `Escape` and `Quit` are separate because they diverge: escape backs out of
/// whatever you are in (a filter, the info page), while `q` and Ctrl-C leave the
/// picker outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Escape,
    Quit,
    /// Show the info page for the selection (and close it again).
    Info,
    /// Delete the selection, after a confirmation.
    Delete,
    /// Archive or unarchive the selection.
    Archive,
    /// Show or hide archived workspaces in the list.
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
    /// Delete this workspace, then reload. `State` decides, `run` performs: the
    /// decision logic stays free of I/O and therefore testable without a tty.
    Delete { name: String, path: PathBuf },
    /// Archive or unarchive this workspace, then reload.
    SetArchived { name: String, path: PathBuf, archived: bool },
}

/// Which frame the picker is showing, and therefore what a key means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    List,
    /// The info page for the selected workspace.
    Info,
    /// "Delete X? [y/N]" over the list.
    ConfirmDelete,
}

pub struct State {
    all: Vec<WorkspaceRow>,
    /// Indices into `all` that pass the current filter, in display order.
    visible: Vec<usize>,
    selected: usize,
    filter: String,
    filtering: bool,
    mode: Mode,
    show_archived: bool,
    /// One line of feedback from the last action ("removed milo", an error),
    /// shown until the next key. Held rather than printed because printing from
    /// under raw mode would scribble over the frame.
    notice: Option<String>,
}

impl State {
    pub fn new(all: Vec<WorkspaceRow>, show_archived: bool) -> Self {
        let mut s = State {
            all,
            visible: Vec::new(),
            selected: 0,
            filter: String::new(),
            filtering: false,
            mode: Mode::List,
            show_archived,
            notice: None,
        };
        s.refilter();
        s
    }

    /// A fresh state over `rows`, keeping everything the user set up: the
    /// filter, the archived toggle, roughly where the cursor was. Reloading is
    /// how every action's result becomes visible, so it must not feel like the
    /// picker restarted.
    pub fn reloaded(&self, rows: Vec<WorkspaceRow>) -> State {
        let mut s = State {
            all: rows,
            visible: Vec::new(),
            selected: self.selected,
            filter: self.filter.clone(),
            filtering: self.filtering,
            // Never land back on a page or a confirmation for a row that may no
            // longer be there.
            mode: Mode::List,
            show_archived: self.show_archived,
            notice: self.notice.clone(),
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

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn show_archived(&self) -> bool {
        self.show_archived
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn set_notice(&mut self, msg: impl Into<String>) {
        self.notice = Some(msg.into());
    }

    /// What a printable key means as a command. Resolved here rather than in
    /// `read_key` because only `State` knows whether you are typing a filter:
    /// mapping `d` to "delete" at the terminal made the letters `j k q d a`
    /// untypeable, so `/al` toggled archived instead of finding "alpha".
    fn command_for(c: char) -> Option<Key> {
        Some(match c {
            'k' => Key::Up,
            'j' => Key::Down,
            'q' => Key::Quit,
            'i' => Key::Info,
            'd' => Key::Delete,
            // `a` acts on the selection like enter/i/d; the filter it used to
            // own moves to the shifted form.
            'a' => Key::Archive,
            'A' => Key::ToggleArchived,
            '/' => Key::StartFilter,
            _ => return None,
        })
    }

    /// Feed one key in. The only place selection, filtering and exit are decided.
    pub fn on_key(&mut self, k: Key) -> Step {
        // Any key clears the last action's feedback: it describes what the
        // previous key did, not this one.
        self.notice = None;

        // While filtering, printable keys extend the query instead of acting as
        // commands — otherwise you could not filter for a workspace called "q",
        // and `d` would delete rather than type.
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
                Key::Escape => {
                    // Esc while filtering cancels the filter, it does not quit:
                    // losing the whole list because you mistyped a query would be
                    // its own small betrayal.
                    self.filtering = false;
                    self.filter.clear();
                    self.refilter();
                    return Step::Continue;
                }
                // Actions on the selection are unreachable while typing: the
                // terminal sends these as characters, and a `State` driven
                // directly must not be a way around that.
                Key::Info | Key::Delete | Key::Archive | Key::ToggleArchived => {
                    return Step::Continue
                }
                // Up/Down still move, so you can narrow and then pick without
                // leaving the filter.
                _ => {}
            }
        }

        // Outside the filter, a printable key is a command. Inside the
        // confirmation it is not: `y` there answers a question, and `d` must not
        // re-open the one you are already being asked.
        let k = match (self.mode, k) {
            (Mode::ConfirmDelete, _) => k,
            (_, Key::Char(c)) => Self::command_for(c).unwrap_or(k),
            _ => k,
        };

        match self.mode {
            Mode::ConfirmDelete => self.on_key_confirming(k),
            Mode::Info => self.on_key_info(k),
            Mode::List => self.on_key_list(k),
        }
    }

    /// Only `y` deletes. Every other key — including Enter — cancels: a
    /// confirmation whose default answer is destructive is not a confirmation.
    fn on_key_confirming(&mut self, k: Key) -> Step {
        self.mode = Mode::List;
        match (k, self.selected_row()) {
            (Key::Char('y') | Key::Char('Y'), Some(r)) => {
                Step::Delete { name: r.name.clone(), path: r.path.clone() }
            }
            _ => Step::Continue,
        }
    }

    /// The info page is never a trap: escape and `i` return to the list, Enter
    /// opens what you are looking at, `q` still leaves.
    fn on_key_info(&mut self, k: Key) -> Step {
        match k {
            Key::Escape | Key::Info => {
                self.mode = Mode::List;
                Step::Continue
            }
            Key::Quit => Step::Done(Outcome::Quit),
            Key::Enter => self.launch_selected(),
            _ => Step::Continue,
        }
    }

    fn on_key_list(&mut self, k: Key) -> Step {
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
            Key::Enter => self.launch_selected(),
            Key::Escape | Key::Quit => Step::Done(Outcome::Quit),
            Key::Info => {
                // Nothing selected → nothing to show a page about.
                if self.selected_row().is_some() {
                    self.mode = Mode::Info;
                }
                Step::Continue
            }
            Key::Delete => {
                if self.selected_row().is_some() {
                    self.mode = Mode::ConfirmDelete;
                }
                Step::Continue
            }
            Key::Archive => match self.selected_row() {
                Some(r) => Step::SetArchived {
                    name: r.name.clone(),
                    path: r.path.clone(),
                    archived: !r.archived,
                },
                None => Step::Continue,
            },
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

    fn launch_selected(&self) -> Step {
        match self.selected_row() {
            // A registered path that no longer exists cannot be launched;
            // saying so beats execing an agent into a missing directory.
            Some(r) if matches!(r.state, RowState::Missing) => Step::Continue,
            Some(r) => Step::Done(Outcome::Launch(r.name.clone())),
            None => Step::Continue,
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

/// How much of a notebook and a timeline the info page shows. Capped so the
/// page fits a short terminal — it is drawn in flow, so a page taller than the
/// window would scroll the frame out from under the erase arithmetic.
const INFO_NOTEBOOK_LINES: usize = 5;
const INFO_RECENT_LINES: usize = 4;

/// `~/x` rather than `/Users/someone/x`. Cosmetic, but the home prefix is the
/// least informative part of every path on the page.
fn home_relative(p: &Path) -> String {
    let s = p.display().to_string();
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(home) if !home.is_empty() => match s.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => s,
        },
        _ => s,
    }
}

/// `2026-08-05T14:02:33Z` → `14:02`. Anything that is not shaped like an ISO
/// timestamp is passed through: an unparseable stamp is still evidence.
fn short_ts(ts: &str) -> String {
    match ts.split_once('T') {
        Some((_, time)) if time.len() >= 5 => time[..5].to_string(),
        _ => ts.to_string(),
    }
}

/// Truncate to the terminal width. Nothing wraps: a wrapped line would print as
/// two and break the "erase exactly what I printed" line count.
fn clip(s: &str, width: u16) -> String {
    let w = width.max(20) as usize;
    if s.chars().count() <= w {
        return s.to_string();
    }
    s.chars().take(w.saturating_sub(1)).collect::<String>() + "…"
}

/// The info page for one workspace, as lines. Pure, so what it shows is
/// assertable without a terminal; `now` is passed in for the same reason
/// `render_row` takes it.
///
/// Every field is optional and an absent one is omitted rather than rendered
/// blank — an empty value column reads as a bug, and a heading with nothing
/// under it reads as missing data rather than as data that does not exist.
pub fn render_info(
    r: &WorkspaceRow,
    d: &Detail,
    now: i64,
    width: u16,
    theme: &Theme,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let w = width.max(20) as usize;

    // Header: name left, agent right.
    let pad = w.saturating_sub(r.name.chars().count() + r.agent.chars().count() + 2);
    out.push(clip(&format!("  {}{}{}", r.name, " ".repeat(pad), r.agent), width));
    out.push(theme.dim(&clip(&format!("  {}", home_relative(&r.path)), width)));
    out.push(theme.dim(&"─".repeat(w.saturating_sub(2))));

    let mut fact = |label: &str, value: String| {
        if !value.is_empty() {
            out.push(clip(&format!("  {label:<10} {value}"), width));
        }
    };

    let mut status = Vec::new();
    match &r.state {
        RowState::Missing => status.push("missing".to_string()),
        RowState::Corrupt(e) => status.push(format!("corrupt ({e})")),
        RowState::Ok => {
            if r.archived {
                status.push("archived".to_string());
            }
            if let Some(s) = &r.status {
                status.push(s.clone());
            }
        }
    }
    if let Some(pid) = r.live_pid {
        status.push(format!("running · pid {pid}"));
    }
    fact("status", status.join(" · "));
    fact("activity", r.last_activity.map(|t| crate::rows::ago(t, now)).unwrap_or_default());
    fact(
        "usage",
        r.limits
            .as_ref()
            .map(|l| {
                format!(
                    "5h {}%  ·  week {}%",
                    l.five_hour.used_pct.round() as i64,
                    l.seven_day.used_pct.round() as i64
                )
            })
            .unwrap_or_default(),
    );
    fact(
        "tasks",
        match d.queue {
            Some(n) if n > 0 => format!("{n} open"),
            _ => String::new(),
        },
    );
    fact("tags", if r.tags.is_empty() { String::new() } else { format!("#{}", r.tags.join(" #")) });

    if let Some(obj) = &d.objective {
        out.push(String::new());
        out.push(theme.dim("  OBJECTIVE"));
        out.push(clip(&format!("  {obj}"), width));
    }

    if !d.notebook.is_empty() {
        let tail: Vec<&String> = d.notebook.iter().rev().take(INFO_NOTEBOOK_LINES).rev().collect();
        out.push(String::new());
        out.push(theme.dim(&format!("  NOTEBOOK (last {})", tail.len())));
        for l in tail {
            out.push(clip(&format!("  · {}", l.trim_start_matches(['-', '*', '#', ' '])), width));
        }
    }

    if !d.chain.is_empty() {
        out.push(String::new());
        out.push(theme.dim("  RECENT"));
        for e in d.chain.iter().rev().take(INFO_RECENT_LINES) {
            out.push(clip(&format!("  {}  {:<9} {}", short_ts(&e.ts), e.kind, e.actor), width));
        }
    }

    out
}

fn load(show_archived: bool) -> Result<Vec<WorkspaceRow>> {
    let opts = ListOpts { tag: None, include_archived: show_archived };
    Ok(crate::rows::list_all(&opts)?.rows)
}

/// Draw the list, returning how many lines were printed so the next frame can
/// move back over exactly those.
fn draw(
    state: &State,
    theme: &Theme,
    now: i64,
    (width, height): (u16, u16),
    out: &mut impl Write,
) -> Result<u16> {
    let mut lines = 0u16;

    if state.mode() == Mode::Info {
        if let Some(r) = state.selected_row() {
            let d = crate::detail::gather(r, INFO_NOTEBOOK_LINES);
            let mut page = render_info(r, &d, now, width, theme);
            // A frame taller than the window scrolls, and then `erase` walks up
            // over lines that are no longer where it printed them — leaving the
            // page smeared across the scrollback. Better to show less.
            let room = (height as usize).saturating_sub(3);
            if page.len() > room {
                page.truncate(room);
                page.push(theme.dim("  … (window too short to show the rest)"));
            }
            for line in page {
                writeln!(out, "{line}\r")?;
                lines += 1;
            }
            writeln!(out, "\r")?;
            lines += 1;
            writeln!(out, "{}\r", theme.dim("  esc back · enter open · q quit"))?;
            lines += 1;
            out.flush()?;
            return Ok(lines);
        }
    }

    let rows = state.visible_rows();
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

    if let Some(msg) = state.notice() {
        writeln!(out, "  {}\r", clip(msg, width))?;
        lines += 1;
    }

    let hint = if state.mode() == Mode::ConfirmDelete {
        match state.selected_row() {
            Some(r) => confirm_line(r),
            None => "  nothing to delete".to_string(),
        }
    } else if state.is_filtering() {
        format!("  /{}", state.filter())
    } else {
        let arch = if state.show_archived() { "hide archived" } else { "show archived" };
        format!(
            "  ↑↓ move · enter open · i info · d delete · a archive · A {arch} · / filter · q quit"
        )
    };
    // The confirmation is the one line that must not be dimmed: it is a
    // question about deleting something, not a hint you can skim past.
    let hint = if state.mode() == Mode::ConfirmDelete { hint } else { theme.dim(&hint) };
    writeln!(out, "{}\r", clip(&hint, width))?;
    lines += 1;

    out.flush()?;
    Ok(lines)
}

/// The confirmation question. It names what will actually happen, which differs
/// by workspace: a managed one is deleted whole, while an adopted project loses
/// only its `.ws/` and the source tree stays. `deletes_whole_directory` is the
/// exact predicate `remove_one` acts on, so the two cannot disagree.
fn confirm_line(r: &WorkspaceRow) -> String {
    if crate::commands::deletes_whole_directory(&r.path) {
        format!("  Delete {} and everything in {}? [y/N]", r.name, home_relative(&r.path))
    } else {
        format!("  Remove ws metadata from {} (the project itself is kept)? [y/N]", r.name)
    }
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

/// Terminal key → picker key. Pure, so the mapping is testable — which matters
/// now that one of these letters deletes a workspace.
fn map_key(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<Key> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let held = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match code {
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Escape),
        KeyCode::Backspace => Some(Key::Backspace),
        // Both of the conventional "get me out of here" chords.
        KeyCode::Char('c' | 'd') if modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Quit),
        // Any other modified key is dropped rather than read as the bare
        // letter. Ctrl-D otherwise arrived as `d` and opened the delete
        // confirmation, which is not what anyone means by it.
        KeyCode::Char(_) if held => None,
        // Every other printable key is passed through as itself. What it
        // *means* is `State::command_for`'s decision, because it depends on
        // whether a filter is being typed.
        KeyCode::Char(c) => Some(Key::Char(c)),
        _ => None,
    }
}

fn read_key() -> Result<Option<Key>> {
    use crossterm::event::{Event, KeyEvent};
    match crossterm::event::read()? {
        Event::Key(KeyEvent { code, modifiers, .. }) => Ok(map_key(code, modifiers)),
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

    let size = crossterm::terminal::size().unwrap_or((80, 24));
    let mut printed = draw(&state, &theme, now, size, &mut stdout)?;
    loop {
        let key = match read_key()? {
            Some(k) => k,
            None => continue,
        };
        let mut reload = false;
        match state.on_key(key) {
            Step::Continue => {}
            Step::Reload => reload = true,
            Step::Delete { name, path } => {
                match crate::commands::remove_one(&name, &path, false) {
                    Ok(()) => state.set_notice(format!("removed {name}")),
                    Err(e) => state.set_notice(describe_remove_error(&name, e)),
                }
                reload = true;
            }
            Step::SetArchived { name, path, archived } => {
                match set_archived(&name, &path, archived) {
                    Ok(()) => state.set_notice(format!(
                        "{name}: {}",
                        if archived { "archived" } else { "unarchived" }
                    )),
                    Err(e) => state.set_notice(format!("{name}: {e:#}")),
                }
                reload = true;
            }
            Step::Done(outcome) => {
                // Move back over the frame and clear it, so the picker leaves the
                // shell exactly as it found it before the agent takes over.
                erase(&mut stdout, printed)?;
                return Ok(outcome);
            }
        }
        if reload {
            state = state.reloaded(load(state.show_archived())?);
        }
        erase(&mut stdout, printed)?;
        printed = draw(&state, &theme, now, size, &mut stdout)?;
    }
}

/// Archive or unarchive, through the same gate `ws -archive` uses: a workspace
/// written by a newer contract version is not one to write to.
fn set_archived(name: &str, path: &Path, archived: bool) -> Result<()> {
    let wt = path.join(".ws/workspace.toml");
    crate::contract::check_gate(name, &wt)?;
    crate::meta::set_archived(&wt, archived)
}

/// One line, because that is all the frame has. The distinctions that change
/// what you would do next are kept: in use by whom, could not be determined,
/// could not be deleted.
fn describe_remove_error(name: &str, e: crate::commands::RemoveError) -> String {
    use crate::commands::RemoveError as E;
    match e {
        E::Live(pid) => format!("{name}: in use by pid {pid} — close it first"),
        E::LockUnreadable(e) => format!("{name}: could not check whether it is in use: {e}"),
        E::Delete(e) => format!("{name}: {e}"),
        // The workspace is gone but the registry still names it. Saying "removed"
        // here would be a lie the next listing contradicts.
        E::Unregister(e) => format!("{name}: deleted, but its registry entry remains: {e}"),
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
        crate::theme::resolve(
            "dark",
            &crate::theme::ThemeEnv { no_color: true, ..Default::default() },
        )
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
        assert_eq!(s.selected_index(), 1);

        let mut s2 = three();
        s2.on_key(Key::Char('j'));
        assert_eq!(s2.selected_index(), 1, "j is down outside the filter");
        s2.on_key(Key::Char('k'));
        assert_eq!(s2.selected_index(), 0, "k is up");
    }

    /// Every command letter must still be typeable into the filter. This was
    /// broken: the terminal layer resolved `j k q d a` to commands before the
    /// filter saw them, so a workspace whose name needed one of those letters
    /// could not be searched for — `/al` toggled archived instead.
    #[test]
    fn command_letters_are_typeable_into_the_filter() {
        let mut s = State::new(vec![row("alpha"), row("dad"), row("quiet")], false);
        s.on_key(Key::StartFilter);
        for c in "da".chars() {
            assert_eq!(s.on_key(Key::Char(c)), Step::Continue, "{c:?} must type, not act");
        }
        assert_eq!(s.filter(), "da");
        assert_eq!(s.visible_rows().len(), 1);
        assert_eq!(s.selected_row().unwrap().name, "dad");
        assert_eq!(s.mode(), Mode::List, "typing a filter must not have deleted anything");
        assert!(!s.show_archived(), "or toggled the archived filter");
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
        assert_eq!(s.on_key(Key::Escape), Step::Continue, "must not quit");
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
    fn the_info_page_opens_and_closes() {
        let mut s = three();
        assert_eq!(s.mode(), Mode::List);
        s.on_key(Key::Info);
        assert_eq!(s.mode(), Mode::Info);
        s.on_key(Key::Info);
        assert_eq!(s.mode(), Mode::List, "i closes what i opened");

        s.on_key(Key::Info);
        s.on_key(Key::Escape);
        assert_eq!(s.mode(), Mode::List, "esc backs out");
    }

    /// The page is never a trap: enter opens what you are reading about, and q
    /// still leaves the picker entirely.
    #[test]
    fn the_info_page_can_open_or_quit_directly() {
        let mut s = three();
        s.on_key(Key::Info);
        assert_eq!(s.on_key(Key::Enter), Step::Done(Outcome::Launch("alpha".into())));

        let mut s = three();
        s.on_key(Key::Info);
        assert_eq!(s.on_key(Key::Quit), Step::Done(Outcome::Quit));
    }

    #[test]
    fn info_with_nothing_selected_does_not_open_a_page_about_nothing() {
        let mut s = State::new(vec![], false);
        s.on_key(Key::Info);
        assert_eq!(s.mode(), Mode::List);
    }

    /// The whole point of the confirmation: only `y` goes through. Enter is
    /// listed among the cancels deliberately — a destructive default is not a
    /// confirmation, and enter is the key most likely to be leaned on.
    #[test]
    fn only_y_confirms_a_delete() {
        for cancel in
            [Key::Enter, Key::Escape, Key::Quit, Key::Char('n'), Key::Char('x'), Key::Down]
        {
            let mut s = three();
            s.on_key(Key::Delete);
            assert_eq!(s.mode(), Mode::ConfirmDelete);
            assert_eq!(s.on_key(cancel), Step::Continue, "{cancel:?} must cancel");
            assert_eq!(s.mode(), Mode::List, "{cancel:?} must leave the confirmation");
        }

        let mut s = three();
        s.on_key(Key::Delete);
        assert_eq!(
            s.on_key(Key::Char('y')),
            Step::Delete { name: "alpha".into(), path: PathBuf::from("/tmp/alpha") }
        );
        assert_eq!(s.mode(), Mode::List, "the confirmation is spent either way");
    }

    /// Movement behind a confirmation would let the answer land on a different
    /// workspace than the question named.
    #[test]
    fn keys_do_not_move_the_selection_behind_the_confirmation() {
        let mut s = three();
        s.on_key(Key::Down);
        s.on_key(Key::Delete);
        s.on_key(Key::Down);
        assert_eq!(s.selected_index(), 1, "the selection is frozen while confirming");
    }

    #[test]
    fn delete_with_nothing_selected_asks_nothing() {
        let mut s = State::new(vec![], false);
        s.on_key(Key::Delete);
        assert_eq!(s.mode(), Mode::List);
    }

    /// `d` must type a `d` while filtering, not delete the row under the
    /// cursor. Same reason `q` does not quit there.
    #[test]
    fn destructive_keys_are_inert_while_filtering() {
        let mut s = three();
        s.on_key(Key::StartFilter);
        for k in [Key::Delete, Key::Archive, Key::Info] {
            assert_eq!(s.on_key(k), Step::Continue, "{k:?}");
            assert_eq!(s.mode(), Mode::List, "{k:?} must not change mode while filtering");
        }
    }

    #[test]
    fn a_archives_the_selection_and_unarchives_an_archived_one() {
        let mut s = three();
        assert_eq!(
            s.on_key(Key::Archive),
            Step::SetArchived {
                name: "alpha".into(),
                path: PathBuf::from("/tmp/alpha"),
                archived: true,
            }
        );

        let mut s = State::new(vec![archived("old")], true);
        assert_eq!(
            s.on_key(Key::Archive),
            Step::SetArchived {
                name: "old".into(),
                path: PathBuf::from("/tmp/old"),
                archived: false,
            },
            "a second press must undo the first, not re-archive"
        );
    }

    /// Reloading is how every action's result becomes visible, so it must not
    /// feel like the picker restarted: the filter and the archived toggle are
    /// the user's, not the frame's.
    #[test]
    fn reloading_keeps_the_filter_and_the_archived_toggle() {
        let mut s = State::new(vec![row("alpha"), row("beta"), archived("old")], true);
        s.on_key(Key::StartFilter);
        s.on_key(Key::Char('b'));
        s.on_key(Key::Enter); // commit the filter
        s.set_notice("removed gamma");

        let r = s.reloaded(vec![row("alpha"), row("beta"), archived("old")]);
        assert_eq!(r.filter(), "b");
        assert_eq!(r.visible_rows().len(), 1);
        assert!(r.show_archived());
        assert_eq!(r.notice(), Some("removed gamma"), "the result of the action survives");
        assert_eq!(r.mode(), Mode::List, "never reload back into a page about a deleted row");
    }

    #[test]
    fn a_notice_lasts_until_the_next_key() {
        let mut s = three();
        s.set_notice("removed alpha");
        assert!(s.notice().is_some());
        s.on_key(Key::Down);
        assert_eq!(s.notice(), None, "it describes the previous key, not this one");
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
        let lines = draw(&s, &plain(), 0, (100, 40), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();

        assert_eq!(lines, 4, "three rows plus the hint line");
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains("alpha") && text.contains("gamma"));
        assert!(text.contains("↑↓ move"), "the hint tells you the keys: {text}");
        assert!(!text.contains("\x1b[2J"), "must never clear the screen");
    }

    /// The hint is the only place the keys are documented, and three of them
    /// are new or moved. A key nobody can discover is a key nobody presses.
    #[test]
    fn the_hint_names_every_action_key() {
        let s = three();
        let mut buf: Vec<u8> = Vec::new();
        draw(&s, &plain(), 0, (120, 40), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        for k in ["enter open", "i info", "d delete", "a archive", "A show archived", "q quit"] {
            assert!(text.contains(k), "the hint must name {k:?}: {text}");
        }
    }

    #[test]
    fn drawing_an_empty_list_says_so() {
        let s = State::new(vec![], false);
        let mut buf: Vec<u8> = Vec::new();
        let lines = draw(&s, &plain(), 0, (100, 40), &mut buf).unwrap();
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
        draw(&s, &plain(), 0, (100, 40), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("/b"), "the query must be visible while typing: {text}");
    }

    /// The confirmation replaces the hint, names the workspace, and shows a
    /// default that is not "delete it".
    #[test]
    fn the_confirmation_asks_before_it_deletes() {
        let mut s = three();
        s.on_key(Key::Delete);
        let mut buf: Vec<u8> = Vec::new();
        draw(&s, &plain(), 0, (100, 40), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("alpha"), "{text}");
        assert!(text.contains("[y/N]"), "the default must be No: {text}");
        assert!(!text.contains("↑↓ move"), "the hint gives way to the question: {text}");
    }

    #[test]
    fn a_notice_is_drawn_above_the_hint() {
        let mut s = three();
        s.set_notice("removed gamma");
        let mut buf: Vec<u8> = Vec::new();
        let lines = draw(&s, &plain(), 0, (100, 40), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(lines, 5, "three rows, the notice, the hint");
        assert!(text.contains("removed gamma"), "{text}");
    }

    // ---- the info page ----------------------------------------------------

    fn detail() -> Detail {
        Detail {
            objective: Some("Port the sync layer to gRPC".into()),
            notebook: vec!["retry budget lives in sync/mod.rs".into()],
            chain: vec![crate::detail::ChainEntry {
                ts: "2026-08-05T14:02:33Z".into(),
                kind: "opened".into(),
                actor: "ionut".into(),
            }],
            queue: Some(2),
        }
    }

    #[test]
    fn the_info_page_shows_what_the_workspace_is() {
        let r = WorkspaceRow {
            live_pid: Some(48211),
            tags: vec!["native".into()],
            last_activity: Some(0),
            ..row("milo")
        };
        let text = render_info(&r, &detail(), 300, 100, &plain()).join("\n");

        assert!(text.contains("milo") && text.contains("claude"), "{text}");
        assert!(text.contains("pid 48211"), "{text}");
        assert!(text.contains("2 open"), "open tasks: {text}");
        assert!(text.contains("#native"), "{text}");
        assert!(text.contains("OBJECTIVE") && text.contains("gRPC"), "{text}");
        assert!(text.contains("NOTEBOOK") && text.contains("retry budget"), "{text}");
        assert!(text.contains("RECENT") && text.contains("14:02"), "{text}");
        assert!(!text.contains("2026-08-05T"), "the date is noise next to the time: {text}");
    }

    /// A brand-new workspace has almost none of this. Blank values and headings
    /// with nothing under them read as broken; absent fields must simply not be
    /// there.
    #[test]
    fn the_info_page_omits_what_it_does_not_have() {
        let text = render_info(&row("fresh"), &Detail::default(), 0, 100, &plain()).join("\n");
        assert!(text.contains("fresh"), "{text}");
        for absent in ["OBJECTIVE", "NOTEBOOK", "RECENT", "tags", "usage", "activity", "tasks"] {
            assert!(!text.contains(absent), "must omit {absent:?}: {text}");
        }
        assert!(!text.contains("0 open"), "no tasks is not a fact worth a line: {text}");
    }

    #[test]
    fn the_info_page_caps_what_it_shows_and_never_wraps() {
        let d = Detail {
            notebook: (0..40).map(|i| format!("note {i} {}", "x".repeat(200))).collect(),
            chain: (0..40)
                .map(|i| crate::detail::ChainEntry {
                    ts: format!("2026-08-05T{:02}:00:00Z", i % 24),
                    kind: "opened".into(),
                    actor: "ionut".into(),
                })
                .collect(),
            ..detail()
        };
        let lines = render_info(&row("big"), &d, 0, 60, &plain());

        assert_eq!(lines.iter().filter(|l| l.starts_with("  · note")).count(), INFO_NOTEBOOK_LINES);
        assert!(lines.iter().any(|l| l.contains("note 39")), "the tail is the recent end");
        assert_eq!(lines.iter().filter(|l| l.contains("opened")).count(), INFO_RECENT_LINES);
        for l in &lines {
            assert!(l.chars().count() <= 60, "a wrapped line breaks the erase count: {l:?}");
        }
    }

    /// Ctrl-D used to arrive as a bare `d` and open the delete confirmation —
    /// found by driving the real binary through a pty, where the shell's EOF
    /// lands on the picker. No chord may reach a destructive key.
    #[test]
    fn modified_keys_are_not_commands() {
        use crossterm::event::{KeyCode, KeyModifiers as M};
        assert_eq!(map_key(KeyCode::Char('d'), M::CONTROL), Some(Key::Quit), "ctrl-d leaves");
        assert_eq!(map_key(KeyCode::Char('c'), M::CONTROL), Some(Key::Quit));
        for c in ['a', 'd', 'i', 'q', 'x'] {
            assert_eq!(map_key(KeyCode::Char(c), M::ALT), None, "alt-{c} is not {c}");
        }
        assert_eq!(map_key(KeyCode::Char('a'), M::CONTROL), None, "ctrl-a is not archive");
        assert_eq!(
            map_key(KeyCode::Char('d'), M::NONE),
            Some(Key::Char('d')),
            "plain d still acts"
        );
        assert_eq!(map_key(KeyCode::Char('A'), M::SHIFT), Some(Key::Char('A')), "shift is typing");
    }

    /// The page is drawn in flow like every other frame — no alternate screen,
    /// no clear — and reports its own line count so `erase` can undo exactly it.
    #[test]
    fn drawing_the_info_page_stays_in_flow() {
        let mut s = three();
        s.on_key(Key::Info);
        let mut buf: Vec<u8> = Vec::new();
        let lines = draw(&s, &plain(), 0, (100, 40), &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();

        assert_eq!(lines as usize, text.lines().count(), "erase undoes what draw printed");
        assert!(text.contains("esc back"), "{text}");
        assert!(!text.contains("\x1b[2J") && !text.contains("\x1b[?1049h"), "no takeover: {text}");
        assert!(!text.contains("↑↓ move"), "the list hint belongs to the list: {text}");
    }
}
