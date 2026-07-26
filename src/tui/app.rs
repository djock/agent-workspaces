//! TUI state and key handling — deliberately free of any terminal I/O so the
//! whole interaction model is unit-testable.
use crate::rows::WorkspaceRow;
use ratatui::crossterm::event::KeyCode;

#[derive(Debug, Clone, PartialEq)]
pub enum InputField {
    Tag,
    Status,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
    Filter,
    Input(InputField),
    Confirm,
}

/// What the event loop should do after a key. `None` = redraw and keep going.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    Launch(String),
}

pub struct App {
    pub rows: Vec<WorkspaceRow>,
    /// Index into `rows`, not into the filtered view.
    pub selected: usize,
    /// Epoch seconds used for every relative time on screen; injected so
    /// snapshots are deterministic.
    pub now: i64,
    pub mode: Mode,
    pub message: Option<String>,
    /// Whether archived rows are shown. `rows` always *contains* them (they
    /// are loaded with `include_archived`); this decides what `visible()`
    /// lets through, so `A` is a pure view toggle with no reload. Without it
    /// `a` was a one-way door: archive, quit, and the workspace was gone with
    /// no in-TUI way back.
    pub show_archived: bool,
    /// Index of a `RowState::Missing` row whose Enter has been armed. Opening
    /// one *creates* a workspace at a path ws did not make — see
    /// `contract::init` — so the first Enter says what it is about to do and
    /// the second does it.
    armed_missing: Option<usize>,
    /// Case-insensitive substring typed in `Mode::Filter`, matched against
    /// row names.
    pub filter: String,
    /// Free-text being typed in `Mode::Input`.
    pub buffer: String,
    pub theme: crate::tui::theme::Theme,
    /// Cached detail for `selected`, so a redraw (every keystroke, including
    /// each filter character) does not re-read the README, the newest notebook
    /// and the whole timeline. Invalidated when the selection changes; a
    /// workspace edited by another process mid-view keeps the stale pane until
    /// the cursor moves, which is the honest trade for not doing file I/O in
    /// the draw path.
    detail_cache: Option<(usize, crate::tui::detail::Detail)>,
    /// What `r` is about to delete for the row that opened `Mode::Confirm`:
    /// `true` = the whole workspace directory, `false` = only its `.ws/`.
    ///
    /// Computed once, here, when the dialog opens — not in `render_confirm`.
    /// The predicate is `commands::deletes_whole_directory`, which reads
    /// `config.toml` and calls `canonicalize()`; calling it from the draw path
    /// put file I/O back into `render.rs` on every keystroke, which is the
    /// invariant this module's doc comment claims. Storing the answer keeps
    /// both properties that matter: the draw stays I/O-free, and the dialog
    /// still states exactly what `remove_one` will do rather than re-deriving
    /// it (the C3 failure mode).
    pub confirm_deletes_whole_directory: bool,
}

impl App {
    pub fn new(rows: Vec<WorkspaceRow>, now: i64) -> Self {
        App {
            rows,
            selected: 0,
            now,
            mode: Mode::Browse,
            message: None,
            show_archived: false,
            armed_missing: None,
            filter: String::new(),
            buffer: String::new(),
            // Deterministic for unit tests: no env reads, matches config default "auto".
            theme: crate::tui::theme::resolve("auto", &crate::tui::theme::ThemeEnv::default()),
            detail_cache: None,
            confirm_deletes_whole_directory: false,
        }
    }

    /// Same as `new`, with the theme resolved from config + environment.
    pub fn with_theme(rows: Vec<WorkspaceRow>, now: i64, theme: crate::tui::theme::Theme) -> Self {
        let mut a = App::new(rows, now);
        a.theme = theme;
        a
    }

    /// Indices of rows on screen: archived rows unless `show_archived`, then
    /// the current filter (case-insensitive substring over the name), in
    /// registry order.
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&i| self.show_archived || !self.rows[i].archived)
            .filter(|&i| needle.is_empty() || self.rows[i].name.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn selected_row(&self) -> Option<&WorkspaceRow> {
        self.rows.get(self.selected)
    }

    /// Gather (and cache) the detail pane for `selected`. `&mut self` because
    /// gathering does file I/O — the event loop calls this once per iteration,
    /// just before `term.draw`, so the actual draw (`render`, which only gets
    /// `&App`) always finds a fresh-enough cache and never touches disk itself.
    pub fn detail(&mut self) -> &crate::tui::detail::Detail {
        let idx = self.selected;
        let stale = !matches!(&self.detail_cache, Some((i, _)) if *i == idx);
        if stale {
            let gathered = match self.visible_selection() {
                Some(r) => crate::tui::detail::gather(r, 5),
                None => crate::tui::detail::Detail::default(),
            };
            self.detail_cache = Some((idx, gathered));
        }
        &self.detail_cache.as_ref().unwrap().1
    }

    /// `&self` read of whatever `detail()` last gathered, for `render` (which
    /// must stay `&App`, not `&mut App`, to keep the draw path I/O-free).
    /// `None` before the first `detail()` call of a run, which `render`
    /// treats the same as "nothing selected".
    pub fn cached_detail(&self) -> Option<&crate::tui::detail::Detail> {
        self.detail_cache.as_ref().map(|(_, d)| d)
    }

    /// The row under the cursor, but only when it is actually on screen.
    /// `rows` is loaded with `include_archived: true` and `selected` is an
    /// index into `rows`, not `visible()` — so once `A` exists, a row can be
    /// selected yet hidden (every visible row got archived, or the filter
    /// changed). Everything that would act on "the selected workspace" —
    /// launching it, the mutation keys, the detail pane — must ask this, not
    /// `selected_row()`, or it ends up acting on a row the list is actively
    /// telling the user is not there.
    pub fn visible_selection(&self) -> Option<&WorkspaceRow> {
        self.visible().contains(&self.selected).then(|| &self.rows[self.selected])
    }

    /// Move the cursor `delta` positions through the *visible* rows, clamped at
    /// both ends. A no-op when nothing is visible.
    pub fn move_selection(&mut self, delta: i32) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let cur = visible.iter().position(|&i| i == self.selected).unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, visible.len() as i32 - 1) as usize;
        self.selected = visible[next];
    }

    /// Keep `selected` on a row that is actually on screen after the filter
    /// changes — otherwise Enter could open a workspace the user cannot see.
    /// When nothing is visible there is nothing to land on; `selected` is
    /// left pointing at whatever it pointed at, but that is harmless because
    /// every consumer asks `visible_selection()`, which treats "not in
    /// `visible()`" — including "`visible()` is empty" — as no selection.
    fn reconcile_selection(&mut self) {
        let visible = self.visible();
        if visible.is_empty() || visible.contains(&self.selected) {
            return;
        }
        self.selected = visible[0];
    }

    /// Handle one key. Pure: the caller decides what `Action` means.
    pub fn on_key(&mut self, key: KeyCode) -> Action {
        self.message = None;
        // Arming a `(missing)` row survives exactly one key: the next Enter.
        let armed = self.armed_missing.take();
        match self.mode {
            Mode::Filter => match key {
                KeyCode::Esc => {
                    self.mode = Mode::Browse;
                    self.filter.clear();
                    self.reconcile_selection();
                    Action::None
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.reconcile_selection();
                    Action::None
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.reconcile_selection();
                    Action::None
                }
                KeyCode::Up => { self.move_selection(-1); Action::None }
                KeyCode::Down => { self.move_selection(1); Action::None }
                KeyCode::Enter => self.launch_selected(armed),
                _ => Action::None,
            },
            Mode::Browse => match key {
                KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                KeyCode::Char('/') => { self.mode = Mode::Filter; Action::None }
                KeyCode::Char('j') | KeyCode::Down => { self.move_selection(1); Action::None }
                KeyCode::Char('k') | KeyCode::Up => { self.move_selection(-1); Action::None }
                KeyCode::Enter => self.launch_selected(armed),
                KeyCode::Char('t') if self.visible_selection().is_some() => {
                    self.mode = Mode::Input(InputField::Tag);
                    self.buffer.clear();
                    Action::None
                }
                KeyCode::Char('s') if self.visible_selection().is_some() => {
                    self.mode = Mode::Input(InputField::Status);
                    // Pre-fill with the current status so `s` edits rather than retypes.
                    self.buffer = self.visible_selection().and_then(|r| r.status.clone()).unwrap_or_default();
                    Action::None
                }
                KeyCode::Char('a') if self.visible_selection().is_some() => {
                    self.toggle_archived();
                    Action::None
                }
                // I4: `a` archives; without this `A` there was no way back
                // from inside the TUI — the row simply vanished and recovery
                // meant knowing about `ws -list --archived` and `ws
                // -unarchive`. Rows are loaded with `include_archived`, so
                // this is a view toggle, not a reload.
                KeyCode::Char('A') => {
                    self.show_archived = !self.show_archived;
                    self.reconcile_selection();
                    self.message = Some(
                        if self.show_archived { "showing archived workspaces" }
                        else { "hiding archived workspaces" }.to_string(),
                    );
                    Action::None
                }
                KeyCode::Char('r') if self.visible_selection().is_some() => {
                    // Resolve the deletion scope here, on the keystroke, so
                    // the dialog's draw does no file I/O. See the field's doc.
                    self.confirm_deletes_whole_directory = self
                        .visible_selection()
                        .is_some_and(|r| crate::commands::deletes_whole_directory(&r.path));
                    self.mode = Mode::Confirm;
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Input(_) => match key {
                KeyCode::Esc => { self.mode = Mode::Browse; self.buffer.clear(); Action::None }
                KeyCode::Backspace => { self.buffer.pop(); Action::None }
                KeyCode::Char(c) => { self.buffer.push(c); Action::None }
                KeyCode::Enter => {
                    if let Err(e) = self.commit_input() {
                        self.message = Some(format!("failed: {e:#}"));
                    }
                    self.mode = Mode::Browse;
                    self.buffer.clear();
                    Action::None
                }
                _ => Action::None,
            },
            Mode::Confirm => match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.remove_selected();
                    self.mode = Mode::Browse;
                    Action::None
                }
                _ => { self.mode = Mode::Browse; Action::None }
            },
        }
    }

    fn commit_input(&mut self) -> anyhow::Result<()> {
        let Some(r) = self.rows.get(self.selected) else { return Ok(()) };
        let toml_path = crate::rows::workspace_toml(&r.path);
        let field = match &self.mode {
            Mode::Input(f) => f.clone(),
            _ => return Ok(()),
        };
        let value = self.buffer.trim().to_string();
        match field {
            InputField::Tag => {
                if value.is_empty() {
                    return Ok(());
                }
                // Space-separated, so one prompt can add several tags.
                let tags: Vec<String> = value.split_whitespace().map(String::from).collect();
                let all = crate::meta::add_tags(&toml_path, &tags)?;
                self.rows[self.selected].tags = all;
            }
            InputField::Status => {
                let text = (!value.is_empty()).then_some(value.as_str());
                crate::meta::set_status(&toml_path, text)?;
                self.rows[self.selected].status = text.map(String::from);
            }
        }
        // Neither field is drawn by the detail pane today, but both are
        // properties of the selected row's `workspace.toml`, and the cache
        // exists to save I/O, not to promise staleness — invalidate on every
        // successful write rather than trusting that today's Detail layout
        // stays that way.
        self.detail_cache = None;
        Ok(())
    }

    fn toggle_archived(&mut self) {
        let Some(r) = self.rows.get(self.selected) else { return };
        let next = !r.archived;
        let toml_path = crate::rows::workspace_toml(&r.path);
        match crate::meta::set_archived(&toml_path, next) {
            Ok(()) => {
                self.rows[self.selected].archived = next;
                self.message = Some(format!(
                    "{} {} (A shows archived)",
                    self.rows[self.selected].name,
                    if next { "archived" } else { "unarchived" }
                ));
                // The row may have just left the view; keep the cursor on
                // something that is actually on screen.
                self.reconcile_selection();
                self.detail_cache = None;
            }
            Err(e) => self.message = Some(format!("failed: {e:#}")),
        }
    }

    fn remove_selected(&mut self) {
        let Some(r) = self.rows.get(self.selected) else { return };
        let (name, path, live_pid) = (r.name.clone(), r.path.clone(), r.live_pid);
        // The row already carries `live_pid` — the same field the list marks
        // with the live indicator — so check it before ever touching disk.
        // The TUI has no force affordance and should not invent one; a live
        // workspace can only be removed from the CLI with `--force`.
        if let Some(pid) = live_pid {
            self.message = Some(format!(
                "{name} is in use by pid {pid} (another terminal). Close it, or use --force from the CLI"
            ));
            return;
        }
        // The row is a startup snapshot. Another process may have re-registered
        // this name at a different path since; deleting the remembered one would
        // destroy a directory the registry no longer associates with this name.
        match crate::registry::lookup_checked(&name) {
            Ok(Some(current)) if current == path => {}
            Ok(Some(_)) => {
                self.message = Some(format!("{name} has moved since this list was loaded — reopen ws to refresh"));
                return;
            }
            Ok(None) => {
                self.message = Some(format!("{name} is no longer registered — reopen ws to refresh"));
                return;
            }
            Err(e) => {
                self.message = Some(format!("cannot verify {name}: {e:#}"));
                return;
            }
        }
        match crate::commands::remove_one(&name, &path, false) {
            Ok(()) => {
                self.drop_selected_row();
                self.message = Some(format!("removed {name}"));
            }
            Err(crate::commands::RemoveError::Delete(e)) => {
                // Nothing changed — the row is still accurate on screen.
                self.message = Some(format!("failed to remove {name}: {e:#}"));
            }
            Err(crate::commands::RemoveError::Live(pid)) => {
                // Nothing changed — the row is still accurate on screen.
                self.message = Some(format!(
                    "{name} is in use by pid {pid} (another terminal). Close it, or use --force from the CLI"
                ));
            }
            Err(crate::commands::RemoveError::LockUnreadable(e)) => {
                // Nothing changed — the row is still accurate on screen.
                self.message = Some(format!("cannot remove {name}: {e:#}"));
            }
            Err(crate::commands::RemoveError::Unregister(e)) => {
                // The workspace itself is gone; only the registry write
                // failed. The row must go too — leaving it on screen would
                // claim the workspace still exists, which is false.
                self.drop_selected_row();
                self.message = Some(format!(
                    "{name} removed, but its registry entry could not be cleared: {e:#}"
                ));
            }
        }
    }

    /// Drop the currently selected row and keep `selected` valid — shared by
    /// both of `remove_selected`'s "the workspace is actually gone" paths.
    fn drop_selected_row(&mut self) {
        self.rows.remove(self.selected);
        // The removed row's index now points at its successor (or past the end).
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        self.reconcile_selection();
        // `rows` shifted: the row now sitting at `selected`'s index (if any)
        // is not the one the cache was gathered for, even when the index
        // itself is unchanged — a cache keyed only on index would otherwise
        // serve the removed row's stale detail to whatever moved into its slot.
        self.detail_cache = None;
    }

    fn launch_selected(&mut self, armed: Option<usize>) -> Action {
        let Some((name, path, state)) = self
            .visible_selection()
            .map(|r| (r.name.clone(), r.path.clone(), r.state.clone()))
        else {
            return Action::None;
        };
        // C1: a `(missing)` row's registry path is usually the user's own
        // project directory with `.ws/` deleted. Launching it runs
        // `contract::init` there — it creates files and, in a git repository,
        // a commit. Say so before doing it rather than after.
        if state == crate::rows::RowState::Missing && armed != Some(self.selected) {
            self.armed_missing = Some(self.selected);
            self.message = Some(format!(
                "{name} has no .ws/ — Enter again to recreate it at {}",
                path.display()
            ));
            return Action::None;
        }
        Action::Launch(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{RowState, WorkspaceRow};
    use ratatui::crossterm::event::KeyCode;

    fn row(name: &str) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(), path: format!("/tmp/{name}").into(), state: RowState::Ok,
            agent: "claude".into(), live_pid: None, archived: false, tags: vec![],
            status: None, color: None, last_activity: None, limits: None,
        }
    }

    fn app3() -> App {
        App::new(vec![row("alpha"), row("beta"), row("gamma")], 0)
    }

    #[test]
    fn arrows_and_jk_move_the_selection_and_stop_at_the_ends() {
        let mut a = app3();
        assert_eq!(a.selected, 0);
        a.on_key(KeyCode::Down);
        assert_eq!(a.selected, 1);
        a.on_key(KeyCode::Char('j'));
        assert_eq!(a.selected, 2);
        a.on_key(KeyCode::Down);
        assert_eq!(a.selected, 2, "selection clamps at the last row");
        a.on_key(KeyCode::Char('k'));
        assert_eq!(a.selected, 1);
        a.on_key(KeyCode::Up);
        a.on_key(KeyCode::Up);
        assert_eq!(a.selected, 0, "and at the first");
    }

    #[test]
    fn slash_starts_filtering_and_typing_narrows_the_view() {
        let mut a = app3();
        a.on_key(KeyCode::Char('/'));
        assert_eq!(a.mode, Mode::Filter);
        a.on_key(KeyCode::Char('a'));
        a.on_key(KeyCode::Char('m'));
        assert_eq!(a.filter, "am");
        let names: Vec<&str> = a.visible().iter().map(|&i| a.rows[i].name.as_str()).collect();
        assert_eq!(names, vec!["gamma"], "substring match, case-insensitive");
        assert_eq!(a.selected, 2, "selection follows the surviving row");
    }

    #[test]
    fn backspace_widens_and_esc_clears_the_filter() {
        let mut a = app3();
        a.on_key(KeyCode::Char('/'));
        a.on_key(KeyCode::Char('b'));
        assert_eq!(a.visible().len(), 1);
        a.on_key(KeyCode::Backspace);
        assert_eq!(a.filter, "");
        assert_eq!(a.visible().len(), 3);
        a.on_key(KeyCode::Char('b'));
        assert_eq!(a.on_key(KeyCode::Esc), Action::None, "esc leaves filtering, it does not quit");
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(a.filter, "");
        assert_eq!(a.visible().len(), 3);
    }

    #[test]
    fn q_quits_in_browse_mode_but_types_in_filter_mode() {
        let mut a = app3();
        assert_eq!(a.on_key(KeyCode::Char('q')), Action::Quit);
        let mut b = app3();
        b.on_key(KeyCode::Char('/'));
        assert_eq!(b.on_key(KeyCode::Char('q')), Action::None);
        assert_eq!(b.filter, "q");
    }

    #[test]
    fn enter_launches_the_selected_workspace() {
        let mut a = app3();
        a.on_key(KeyCode::Down);
        assert_eq!(a.on_key(KeyCode::Enter), Action::Launch("beta".into()));
    }

    #[test]
    fn enter_with_no_visible_rows_does_nothing() {
        let mut a = app3();
        a.on_key(KeyCode::Char('/'));
        for c in "zzz".chars() {
            a.on_key(KeyCode::Char(c));
        }
        assert!(a.visible().is_empty());
        assert_eq!(a.on_key(KeyCode::Enter), Action::None, "must not launch a row that isn't there");
    }

    /// I4. `a` hides a row; `A` must bring it back, in the same session.
    #[test]
    fn capital_a_toggles_archived_rows_into_and_out_of_view() {
        let mut a = App::new(vec![row("alpha"), row("beta")], 0);
        a.rows[1].archived = true;

        let names = |a: &App| -> Vec<String> {
            a.visible().iter().map(|&i| a.rows[i].name.clone()).collect()
        };
        assert_eq!(names(&a), vec!["alpha".to_string()], "archived rows are hidden by default");

        assert_eq!(a.on_key(KeyCode::Char('A')), Action::None);
        assert!(a.show_archived);
        assert_eq!(names(&a), vec!["alpha".to_string(), "beta".to_string()], "A reveals them");
        assert!(a.message.as_deref().unwrap_or("").contains("archived"), "and says so");

        a.on_key(KeyCode::Char('A'));
        assert!(!a.show_archived);
        assert_eq!(names(&a), vec!["alpha".to_string()], "and hides them again");
    }

    /// Archiving the row under the cursor removes it from view; the cursor
    /// must land on a row that is actually on screen.
    #[test]
    fn archiving_the_selected_row_moves_the_cursor_somewhere_visible() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha"), real_row(d.path(), "beta")], 0);
        a.on_key(KeyCode::Char('a'));
        assert!(a.rows[0].archived);
        assert!(
            a.visible().contains(&a.selected),
            "the cursor must not sit on a row that is no longer on screen"
        );
        assert_eq!(a.rows[a.selected].name, "beta");
        // A reveals it again and the row is still there to unarchive.
        a.on_key(KeyCode::Char('A'));
        assert_eq!(a.visible().len(), 2);
    }

    /// C1 (TUI half). A `(missing)` row's path is usually the user's own
    /// project; Enter there runs `contract::init`, which writes files and can
    /// commit. The first Enter must explain, not act.
    #[test]
    fn enter_on_a_missing_row_warns_before_it_recreates() {
        let mut a = app3();
        a.rows[0].state = RowState::Missing;

        assert_eq!(a.on_key(KeyCode::Enter), Action::None, "the first Enter does not launch");
        let msg = a.message.clone().expect("it explains instead");
        assert!(msg.contains("alpha") && msg.contains("Enter again"), "{msg}");
        assert!(msg.contains("/tmp/alpha"), "and names the path it would write to: {msg}");

        assert_eq!(a.on_key(KeyCode::Enter), Action::Launch("alpha".into()), "the second acts");
    }

    #[test]
    fn arming_a_missing_row_does_not_survive_another_key() {
        let mut a = app3();
        a.rows[0].state = RowState::Missing;
        a.on_key(KeyCode::Enter);
        a.on_key(KeyCode::Down); // moved away and back
        a.on_key(KeyCode::Up);
        assert_eq!(a.on_key(KeyCode::Enter), Action::None, "the warning must be re-shown");
    }

    /// A healthy row is unaffected — Enter still opens on the first press.
    #[test]
    fn enter_on_a_healthy_row_is_never_armed() {
        let mut a = app3();
        assert_eq!(a.on_key(KeyCode::Enter), Action::Launch("alpha".into()));
    }

    use std::sync::Mutex;
    use tempfile::TempDir;

    // These tests mutate process-global env (XDG_CONFIG_HOME, WS_ROOT) and
    // touch real files — serialize explicitly rather than leaning on the
    // RUST_TEST_THREADS pin (see registry.rs, rows.rs).
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A row backed by a real `.ws/workspace.toml` so `apply()` has something to write.
    fn real_row(dir: &std::path::Path, name: &str) -> WorkspaceRow {
        std::fs::create_dir_all(dir.join(name).join(".ws")).unwrap();
        std::fs::write(dir.join(name).join(".ws/workspace.toml"), format!("name = \"{name}\"\n")).unwrap();
        let mut r = row(name);
        r.path = dir.join(name);
        r
    }

    #[test]
    fn t_opens_a_tag_prompt_and_enter_writes_the_tag() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha")], 0);
        a.on_key(KeyCode::Char('t'));
        assert_eq!(a.mode, Mode::Input(InputField::Tag));
        for c in "rust".chars() {
            a.on_key(KeyCode::Char(c));
        }
        assert_eq!(a.buffer, "rust");
        a.on_key(KeyCode::Enter);

        assert_eq!(a.mode, Mode::Browse, "prompt closes");
        let m = crate::meta::read(&d.path().join("alpha/.ws/workspace.toml"));
        assert_eq!(m.tags, vec!["rust".to_string()], "written to disk");
        assert_eq!(a.rows[0].tags, vec!["rust".to_string()], "and reflected in the row");
    }

    #[test]
    fn s_sets_the_status_text() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha")], 0);
        a.on_key(KeyCode::Char('s'));
        assert_eq!(a.mode, Mode::Input(InputField::Status));
        for c in "mid-refactor".chars() {
            a.on_key(KeyCode::Char(c));
        }
        a.on_key(KeyCode::Enter);
        let m = crate::meta::read(&d.path().join("alpha/.ws/workspace.toml"));
        assert_eq!(m.status.as_deref(), Some("mid-refactor"));
    }

    #[test]
    fn esc_abandons_a_prompt_without_writing() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha")], 0);
        a.on_key(KeyCode::Char('s'));
        a.on_key(KeyCode::Char('x'));
        a.on_key(KeyCode::Esc);
        assert_eq!(a.mode, Mode::Browse);
        assert!(a.buffer.is_empty());
        assert_eq!(crate::meta::read(&d.path().join("alpha/.ws/workspace.toml")).status, None);
    }

    #[test]
    fn a_toggles_archived_immediately() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha")], 0);
        a.on_key(KeyCode::Char('a'));
        assert!(crate::meta::read(&d.path().join("alpha/.ws/workspace.toml")).archived);
        assert!(a.rows[0].archived, "the row updates without a reload");

        // It was the only row, so archiving it just hid it (`show_archived`
        // is still false) — a bare second `a` must now be inert rather than
        // silently un-archiving a row the list is not showing; that is the
        // exact hidden-row-mutation defect this gate exists to close.
        assert!(a.visible().is_empty(), "the newly-archived row is no longer on screen");
        assert_eq!(a.on_key(KeyCode::Char('a')), Action::None, "a is inert on a hidden row");
        assert!(
            crate::meta::read(&d.path().join("alpha/.ws/workspace.toml")).archived,
            "and does not silently un-archive it"
        );

        // Reveal it and toggle back through the normal path — the round
        // trip still works, it just needs `A` first.
        a.on_key(KeyCode::Char('A'));
        a.on_key(KeyCode::Char('a'));
        assert!(!crate::meta::read(&d.path().join("alpha/.ws/workspace.toml")).archived, "toggles back");
    }

    #[test]
    fn r_requires_confirmation_and_n_cancels() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha")], 0);
        a.on_key(KeyCode::Char('r'));
        assert_eq!(a.mode, Mode::Confirm);
        a.on_key(KeyCode::Char('n'));
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(a.rows.len(), 1, "nothing removed");
        assert!(d.path().join("alpha/.ws").exists());
    }

    #[test]
    fn r_then_y_removes_the_workspace_and_the_row() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let r = real_row(d.path(), "alpha");
        crate::registry::register("alpha", &r.path).unwrap();
        let mut a = App::new(vec![r], 0);
        a.on_key(KeyCode::Char('r'));
        a.on_key(KeyCode::Char('y'));
        assert_eq!(a.mode, Mode::Browse);
        assert!(a.rows.is_empty(), "the row goes away with the workspace");
        assert!(!d.path().join("alpha/.ws").exists());
    }

    #[test]
    fn r_refuses_a_live_workspace_and_says_so() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let mut r = real_row(d.path(), "busy");
        r.live_pid = Some(std::process::id());
        crate::registry::register("busy", &r.path).unwrap();
        let mut a = App::new(vec![r], 0);

        a.on_key(KeyCode::Char('r'));
        a.on_key(KeyCode::Char('y'));

        assert_eq!(a.rows.len(), 1, "the row stays");
        assert!(d.path().join("busy/.ws").exists(), "and so does the workspace");
        let msg = a.message.clone().expect("a message was set");
        assert!(msg.contains("in use"), "the message explains why: {msg}");
    }

    #[test]
    fn r_refuses_when_the_registry_no_longer_agrees_about_the_path() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let r = real_row(d.path(), "alpha");
        // The row was captured at startup; another process has since moved it.
        let moved = d.path().join("moved-alpha");
        std::fs::create_dir_all(moved.join(".ws")).unwrap();
        crate::registry::register("alpha", &moved).unwrap();
        let mut a = App::new(vec![r], 0);

        a.on_key(KeyCode::Char('r'));
        a.on_key(KeyCode::Char('y'));

        assert!(d.path().join("alpha/.ws").exists(), "the stale path is not deleted");
        assert!(moved.join(".ws").exists(), "and neither is the real one");
        let msg = a.message.clone().expect("a message was set");
        assert!(msg.contains("moved"), "the message says why: {msg}");
    }

    // ---- I9: write failures must be *visible* -------------------------
    //
    // This phase exists because a full-screen TUI has no stderr: anything a
    // mutation swallows is swallowed for good. `meta::update` refuses to
    // overwrite an unparseable file, so a row pointing at a corrupt
    // `workspace.toml` makes `t`, `s` and `a` fail deterministically — no
    // permissions games needed. Each test asserts the failure *reaches
    // `app.message`*, not merely that it happened.

    /// A row whose `.ws/workspace.toml` exists but cannot be parsed.
    fn corrupt_row(dir: &std::path::Path, name: &str) -> WorkspaceRow {
        std::fs::create_dir_all(dir.join(name).join(".ws")).unwrap();
        std::fs::write(dir.join(name).join(".ws/workspace.toml"), "not toml {{{").unwrap();
        let mut r = row(name);
        r.path = dir.join(name);
        r
    }

    /// The corrupt file must still be there, byte for byte, after each attempt.
    fn assert_untouched(dir: &std::path::Path, name: &str) {
        assert_eq!(
            std::fs::read_to_string(dir.join(name).join(".ws/workspace.toml")).unwrap(),
            "not toml {{{",
            "a refused write must leave the file alone"
        );
    }

    #[test]
    fn a_failed_tag_write_is_reported_in_the_footer() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![corrupt_row(d.path(), "broken")], 0);
        a.on_key(KeyCode::Char('t'));
        for c in "rust".chars() {
            a.on_key(KeyCode::Char(c));
        }
        a.on_key(KeyCode::Enter);

        let msg = a.message.clone().expect("the failure must reach the user");
        assert!(msg.starts_with("failed:"), "{msg}");
        assert!(msg.contains("workspace.toml"), "and name the file: {msg}");
        assert_eq!(a.mode, Mode::Browse, "the prompt still closes");
        assert!(a.rows[0].tags.is_empty(), "and the row must not claim a tag it does not have");
        assert_untouched(d.path(), "broken");
    }

    #[test]
    fn a_failed_status_write_is_reported_in_the_footer() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![corrupt_row(d.path(), "broken")], 0);
        a.on_key(KeyCode::Char('s'));
        a.on_key(KeyCode::Char('x'));
        a.on_key(KeyCode::Enter);

        let msg = a.message.clone().expect("the failure must reach the user");
        assert!(msg.starts_with("failed:"), "{msg}");
        assert_eq!(a.rows[0].status, None, "the row must not show a status that was never written");
        assert_untouched(d.path(), "broken");
    }

    #[test]
    fn a_failed_archive_write_is_reported_and_the_row_does_not_move() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![corrupt_row(d.path(), "broken")], 0);
        a.on_key(KeyCode::Char('a'));

        let msg = a.message.clone().expect("the failure must reach the user");
        assert!(msg.starts_with("failed:"), "{msg}");
        assert!(!a.rows[0].archived, "a row must never claim to be archived when the write failed");
        assert!(a.visible().contains(&0), "and it must not vanish from the list");
        assert_untouched(d.path(), "broken");
    }

    /// `r` on a workspace whose deletion fails: the message must say so and
    /// the row must stay, because it is still accurate.
    #[test]
    fn a_failed_removal_is_reported_and_keeps_the_row() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        // `.ws` as a *file*: `remove_dir_all` fails with a real error, not
        // NotFound — see commands::remove_one's own test for this shape.
        let project = d.path().join("elsewhere/broken");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(".ws"), "not a directory").unwrap();
        let mut r = row("broken");
        r.path = project.clone();
        crate::registry::register("broken", &project).unwrap();

        let mut a = App::new(vec![r], 0);
        a.on_key(KeyCode::Char('r'));
        a.on_key(KeyCode::Char('y'));

        let msg = a.message.clone().expect("the failure must reach the user");
        assert!(msg.contains("failed to remove broken"), "{msg}");
        assert_eq!(a.rows.len(), 1, "the row stays — nothing was removed");
        assert!(project.join(".ws").exists());
        assert!(crate::registry::lookup("broken").is_some(), "and so does the registry entry");
    }

    #[test]
    fn mutation_keys_are_inert_when_nothing_is_selected() {
        let mut a = App::new(vec![], 0);
        for k in ['a', 't', 's', 'r'] {
            assert_eq!(a.on_key(KeyCode::Char(k)), Action::None);
            assert_eq!(a.mode, Mode::Browse, "{k} must not open a prompt with no rows");
        }
    }

    /// Re-review defect: `rows` is loaded with `include_archived: true` (the
    /// archive-toggle feature), so `selected` can point at a row that is
    /// archived while `show_archived` is false — the list pane then shows
    /// "No active workspaces" while `selected` still refers to the hidden
    /// row. The mutation keys must treat that as no selection, exactly like
    /// the empty-`rows` case above, not act on the hidden row.
    #[test]
    fn the_detail_cache_is_reused_until_the_selection_changes() {
        let d = TempDir::new().unwrap();
        let mut a = App::new(vec![real_row(d.path(), "alpha"), real_row(d.path(), "beta")], 0);
        std::fs::write(d.path().join("alpha/.ws/README.md"), "## Objective\n\nfirst\n").unwrap();

        let first = a.detail().objective.clone();
        assert_eq!(first.as_deref(), Some("first"));

        // Rewrite the file: a cached pane must not re-read it on the next draw.
        std::fs::write(d.path().join("alpha/.ws/README.md"), "## Objective\n\nsecond\n").unwrap();
        assert_eq!(a.detail().objective.as_deref(), Some("first"), "cached, not re-read");

        // Moving the cursor and coming back re-gathers.
        a.move_selection(1);
        let _ = a.detail();
        a.move_selection(-1);
        assert_eq!(a.detail().objective.as_deref(), Some("second"), "invalidated by selection change");
    }

    #[test]
    fn mutation_keys_are_inert_when_the_selected_row_is_hidden_by_archiving() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let mut r = real_row(d.path(), "alpha");
        r.archived = true;
        crate::registry::register("alpha", &r.path).unwrap();
        let mut a = App::new(vec![r], 0);
        assert!(!a.show_archived, "archived rows are hidden by default");
        assert!(a.visible().is_empty(), "the only row is archived and out of view");
        assert_eq!(a.selected, 0, "but the cursor index still refers to it");

        for k in ['t', 's', 'a', 'r'] {
            assert_eq!(a.on_key(KeyCode::Char(k)), Action::None, "{k} must be inert on a hidden row");
            assert_eq!(a.mode, Mode::Browse, "{k} must not open a prompt for a row that is not on screen");
        }

        // `r` specifically: it must never reach `Mode::Confirm` for a hidden
        // row, because a stray `y` right after would run `remove_one` — a
        // whole-directory `remove_dir_all` — on a workspace the list is
        // telling the user is not there. Confirming that here, on top of the
        // `mode` assertion above, pins the actual harm: nothing on disk moved.
        a.on_key(KeyCode::Char('y'));
        assert_eq!(a.mode, Mode::Browse, "a stray y after a gated r must not be treated as a confirmation");
        assert_eq!(a.rows.len(), 1, "no row was removed");
        assert!(d.path().join("alpha/.ws").exists(), "the workspace directory must be untouched");
        assert!(crate::registry::lookup("alpha").is_some(), "and the registry entry must survive");
        assert!(a.rows[0].tags.is_empty(), "no tag was written to a hidden row");
        assert_eq!(a.rows[0].status, None, "no status was written to a hidden row");
    }
}
