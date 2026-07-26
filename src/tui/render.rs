//! Pure drawing. Every function takes `&App` and a `Rect` and renders — no
//! state changes, no I/O — so `TestBackend` can snapshot all of it.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::rows::{ago, RowState, WorkspaceRow};
use crate::tui::app::{App, InputField, Mode};

/// ASCII on purpose: the list must stay readable without a Nerd Font, and
/// `config.nerd_fonts` glyphs are a Phase 9 concern.
pub const LIVE_MARK: &str = "*";

/// Shown in the status column of an archived row once `A` reveals it. Archived
/// and active rows otherwise render identically, which makes `a` a coin flip
/// between archiving and unarchiving. Wording matches `ws -list`'s `[archived]`
/// so the two surfaces agree.
pub const ARCHIVED_MARK: &str = "[archived]";

fn limits_cell(r: &WorkspaceRow, now: i64) -> String {
    match &r.limits {
        Some(s) => format!(
            "{}% {}",
            s.five_hour.used_pct.round() as i64,
            crate::limits::countdown(s.five_hour.resets_at, now)
        ),
        None => "—".into(),
    }
}

fn state_cell(r: &WorkspaceRow) -> String {
    let state = match &r.state {
        RowState::Ok => r.status.clone().unwrap_or_default(),
        RowState::Missing => "(missing)".into(),
        RowState::Corrupt(_) => "(corrupt)".into(),
    };
    // Archived rows are only ever on screen because `A` revealed them, so the
    // marker leads: it is why the row is visible at all. Any status text the
    // user set still follows it.
    match (r.archived, state.is_empty()) {
        (false, _) => state,
        (true, true) => ARCHIVED_MARK.to_string(),
        (true, false) => format!("{ARCHIVED_MARK} {state}"),
    }
}

pub fn render_list(f: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible();
    if visible.is_empty() {
        // "nothing registered", "nothing matched the filter" and "everything
        // here is archived" are three different situations; saying "no
        // workspaces yet" for the last one would hide the rows `A` reveals.
        let empty = if app.rows.is_empty() {
            "No workspaces yet — create one with: ws <name>".to_string()
        } else if !app.filter.is_empty() {
            format!("Nothing matches {:?}", app.filter)
        } else {
            "No active workspaces — press A to show archived".to_string()
        };
        f.render_widget(
            Paragraph::new(empty).block(Block::bordered().title("workspaces")),
            area,
        );
        return;
    }

    let dim = Style::new().fg(app.theme.dim);
    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| {
            let r = &app.rows[i];
            let state_style = if matches!(r.state, RowState::Corrupt(_)) {
                Style::new().fg(app.theme.warn)
            } else {
                Style::default()
            };
            // An archived row is dimmed whole, so a revealed one reads as
            // secondary at a glance rather than sitting among the active
            // workspaces looking identical to them.
            let row_style = if r.archived { dim } else { Style::default() };
            Row::new(vec![
                Cell::from(Span::styled(r.name.clone(), row_style)),
                Cell::from(Span::styled(r.agent.clone(), row_style)),
                Cell::from(Span::styled(
                    if r.live_pid.is_some() { LIVE_MARK.to_string() } else { " ".into() },
                    Style::new().fg(app.theme.live),
                )),
                Cell::from(Span::styled(
                    state_cell(r),
                    if r.archived { dim } else { state_style },
                )),
                Cell::from(Span::styled(r.tags.join(","), dim)),
                Cell::from(Span::styled(
                    r.last_activity.map(|t| ago(t, app.now)).unwrap_or_else(|| "—".into()),
                    dim,
                )),
                Cell::from(limits_cell(r, app.now)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(20), // name
        Constraint::Length(8),  // agent
        Constraint::Length(2),  // live
        Constraint::Min(12),    // status
        Constraint::Length(16), // tags
        Constraint::Length(6),  // activity
        Constraint::Length(12), // limits
    ];

    let mut state = TableState::default().with_selected(Some(
        visible.iter().position(|&i| i == app.selected).unwrap_or(0),
    ));
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["name", "agent", "", "status", "tags", "act", "limits"])
                .style(Style::new().add_modifier(Modifier::BOLD)),
        )
        .block(Block::bordered().title("workspaces"))
        .row_highlight_style(Style::new().reversed());
    f.render_stateful_widget(table, area, &mut state);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let text = match (&app.message, &app.mode) {
        (Some(m), _) => m.clone(),
        (None, Mode::Filter) => format!("filter: {}", app.filter),
        (None, Mode::Input(InputField::Tag)) => format!("tag: {}", app.buffer),
        (None, Mode::Input(InputField::Status)) => format!("status: {}", app.buffer),
        (None, Mode::Confirm) => String::new(), // the dialog carries the question
        (None, Mode::Browse) => format!(
            "enter open   / filter   a archive   A {}   t tag   s status   r remove   q quit",
            if app.show_archived { "hide archived" } else { "show archived" }
        ),
    };
    f.render_widget(Paragraph::new(text), area);
}

fn render_confirm(f: &mut Frame, area: Rect, app: &App) {
    let Some(r) = app.selected_row() else { return };
    // Three content lines plus borders (five rows), and wide enough (80% of
    // width instead of 60%) that a realistic path doesn't get clipped.
    let vertical = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Length(5),
        Constraint::Min(0),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage(10),
        Constraint::Percentage(80),
        Constraint::Percentage(10),
    ])
    .split(vertical[1]);
    let dialog = horizontal[1];
    f.render_widget(Clear, dialog);
    // The scope distinction (whole directory vs. just `.ws/`) is
    // `remove_one`'s to make, not the render layer's — computing it here
    // separately would risk the dialog disagreeing with what actually
    // happens, which is exactly the C3 failure mode the exported predicate
    // exists to avoid. But the predicate reads `config.toml` and calls
    // `canonicalize()`, so calling it from here would put file I/O back into
    // the draw path on every keystroke. `on_key` runs it once when the dialog
    // opens and stores the answer; this reads it.
    let what = if app.confirm_deletes_whole_directory {
        "the whole workspace directory"
    } else {
        "only its .ws/ (the project itself stays)"
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::raw(format!("Remove {}?", r.name)),
            Line::raw(r.path.display().to_string()),
            Line::styled(format!("This deletes {what}. [y/N]"), Style::new().fg(app.theme.warn)),
        ])
        .block(Block::bordered()),
        dialog,
    );
}

fn render_detail(f: &mut Frame, area: Rect, app: &App) {
    // Not `selected_row()`: `selected` can point at a row `visible()` is
    // hiding (e.g. every remaining row just got archived), and the detail
    // pane must not draw a workspace the list is telling the user is not
    // there.
    // `render` only gets `&App` — gathering (README, notebook, timeline I/O)
    // happens once per loop iteration in the event loop, before `term.draw`;
    // this draws whatever `app.detail()` last put in the cache. `None` here
    // means either nothing is selected or the event loop hasn't gathered yet.
    let (Some(r), Some(det)) = (app.visible_selection(), app.cached_detail()) else {
        f.render_widget(Block::bordered().title("detail"), area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(r.name.clone(), Style::new().fg(app.theme.accent)),
        Span::raw(format!("  {}  ", r.agent)),
        Span::styled(
            if r.live_pid.is_some() { "live" } else { "idle" },
            Style::new().fg(if r.live_pid.is_some() { app.theme.live } else { app.theme.dim }),
        ),
    ]));
    if let RowState::Corrupt(e) = &r.state {
        lines.push(Line::styled(format!("corrupt: {e}"), Style::new().fg(app.theme.warn)));
    }
    lines.push(Line::raw(det.objective.clone().unwrap_or_else(|| "(no objective yet)".into())));
    lines.push(Line::styled(
        format!("queue {}   mail {}", det.queue, det.mail),
        Style::new().fg(app.theme.dim),
    ));
    if !det.notebook.is_empty() {
        lines.push(Line::styled("notebook", Style::new().fg(app.theme.dim)));
        lines.extend(det.notebook.iter().cloned().map(Line::raw));
    }
    if !det.chain.is_empty() {
        lines.push(Line::styled("chain", Style::new().fg(app.theme.dim)));
        lines.extend(
            det.chain
                .iter()
                .map(|c| Line::raw(format!("{}  {}  {}", c.ts, c.kind, c.actor))),
        );
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title("detail"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

pub fn render(f: &mut Frame, app: &App) {
    let areas = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());
    // The detail pane sits under the list rather than beside it: the list is
    // seven columns wide and would be unreadable at half the terminal width.
    let panes = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(areas[0]);
    render_list(f, panes[0], app);
    render_detail(f, panes[1], app);
    render_footer(f, areas[1], app);
    if app.mode == Mode::Confirm {
        render_confirm(f, f.area(), app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{RowState, WorkspaceRow};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    pub(crate) fn row(name: &str, agent: &str) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            path: format!("/tmp/{name}").into(),
            state: RowState::Ok,
            agent: agent.into(),
            live_pid: None,
            archived: false,
            tags: vec![],
            status: None,
            color: None,
            last_activity: None,
            limits: None,
        }
    }

    /// Render an App to a fixed-size TestBackend and return the buffer's text.
    /// Cells concatenate with no line breaks, and every column is truncated to
    /// its width constraint — assert on strings that fit.
    pub(crate) fn draw(app: &crate::tui::app::App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn shows_each_workspace_with_its_agent() {
        // `render_detail` also emits the *selected* row's agent id, so a
        // TestBackend buffer (one flat string, no line breaks) can't tell
        // list output from detail output if the row under test is selected.
        // Select gamma: its agent ("gemini") is never asserted below, so the
        // only source of "claude" and "codex" in the buffer is the list's
        // agent column on the two *unselected* rows.
        let mut app = crate::tui::app::App::new(
            vec![row("alpha", "claude"), row("beta", "codex"), row("gamma", "gemini")],
            0,
        );
        app.selected = 2;
        let text = draw(&app, 100, 12);
        assert!(text.contains("alpha"), "{text}");
        assert!(text.contains("beta"), "{text}");
        assert!(text.contains("claude"), "agent column is first-class: {text}");
        assert!(text.contains("codex"), "{text}");
    }

    #[test]
    fn shows_live_marker_and_limits_for_a_running_workspace() {
        let mut r = row("alpha", "claude");
        r.live_pid = Some(4242);
        r.limits = Some(crate::limits::LimitsSnapshot {
            agent: "claude".into(),
            five_hour: crate::limits::Window { used_pct: 62.0, resets_at: 1_000_000 },
            seven_day: crate::limits::Window { used_pct: 20.0, resets_at: 2_000_000 },
            stamped_at: 900_000,
        });
        let app = crate::tui::app::App::new(vec![r], 900_000);
        // `text.contains(LIVE_MARK)` was an assertion on a single "*" inside a
        // flat, line-break-free buffer — any status text, tag or limits string
        // containing an asterisk would have satisfied it. Locate the marker as
        // a *styled cell* instead: `fg_of` finds it by rendered text and the
        // color is what makes it the live marker rather than stray punctuation.
        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        assert_eq!(
            fg_of(term.backend().buffer(), LIVE_MARK),
            Some(app.theme.live),
            "live workspace is marked"
        );
        let text = draw(&app, 100, 12);
        assert!(text.contains("62%"), "per-agent limit state is visible: {text}");
    }

    #[test]
    fn a_corrupt_workspace_says_so_on_screen() {
        // `render_detail` independently emits "corrupt: {e}" for whichever row
        // is selected. Put the corrupt row at index 0 but select the healthy
        // row at index 1, so the detail pane renders no corrupt text at all —
        // the "corrupt" substring below can only have come from the list's
        // state cell.
        let mut r = row("broken", "claude");
        r.state = RowState::Corrupt("workspace.toml is corrupt".into());
        let mut app = crate::tui::app::App::new(vec![r, row("ok", "claude")], 0);
        app.selected = 1;
        let text = draw(&app, 100, 12);
        assert!(text.contains("corrupt"), "the TUI must never render breakage as emptiness: {text}");
    }

    /// I9, end to end. A mutation that fails must be *on screen* — the TUI
    /// has no stderr, so `app.message` reaching the footer is the whole
    /// mechanism by which a failure is visible at all.
    #[test]
    fn a_failed_mutation_shows_up_in_the_footer() {
        let d = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(d.path().join(".ws")).unwrap();
        std::fs::write(d.path().join(".ws/workspace.toml"), "not toml {{{").unwrap();
        let mut r = row("alpha", "claude");
        r.path = d.path().to_path_buf();

        let mut app = crate::tui::app::App::new(vec![r], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('a'));
        // Wide on purpose: the footer is one line and truncates, and the
        // reason sits at the end of a message that carries a temp-dir path.
        let text = draw(&app, 240, 24);
        assert!(text.contains("failed:"), "the write failure must be drawn: {text}");
        assert!(text.contains("refusing to overwrite"), "with the reason: {text}");
    }

    /// The `A` hint has to be in the browse footer, or the toggle that makes
    /// archiving reversible is undiscoverable.
    #[test]
    fn the_browse_footer_offers_the_archived_toggle() {
        let mut app = crate::tui::app::App::new(vec![row("alpha", "claude")], 0);
        assert!(draw(&app, 100, 12).contains("A show archived"), "hidden → offer to show");
        app.on_key(ratatui::crossterm::event::KeyCode::Char('A'));
        app.message = None; // the toggle's own confirmation would occupy the footer
        assert!(draw(&app, 100, 12).contains("A hide archived"), "shown → offer to hide");
    }

    /// `A` reveals archived workspaces into the same list as the active ones.
    /// Without a marker they are indistinguishable, so `a` on a revealed row is
    /// a coin flip between archiving and unarchiving — and the user has no way
    /// to tell which rows the default view was hiding from them.
    #[test]
    fn a_revealed_archived_row_is_marked_and_dimmed() {
        let mut archived = row("alpha", "claude");
        archived.archived = true;
        let mut app = crate::tui::app::App::new(vec![archived, row("beta", "codex")], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('A'));
        app.message = None; // the toggle's confirmation would occupy the footer

        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(text.contains("alpha"), "the archived row is revealed: {text}");
        assert!(text.contains("beta"), "beside the active one: {text}");
        assert!(text.contains(ARCHIVED_MARK), "and is marked: {text}");
        assert_eq!(
            fg_of(&buf, "alpha"),
            Some(app.theme.dim),
            "the archived row reads as secondary"
        );
        assert_ne!(
            fg_of(&buf, "beta"),
            Some(app.theme.dim),
            "while the active row does not — the marker must discriminate"
        );
    }

    #[test]
    fn empty_registry_says_it_is_empty() {
        let app = crate::tui::app::App::new(vec![], 0);
        let text = draw(&app, 100, 12);
        assert!(text.contains("No workspaces"), "{text}");
    }

    #[test]
    fn filter_mode_shows_what_is_being_typed() {
        let mut app = crate::tui::app::App::new(vec![row("alpha", "claude"), row("beta", "codex")], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('/'));
        // "l" rather than "a": both "alpha" and "beta" contain the letter "a",
        // so that query would not discriminate between them. "l" is unique to
        // "alpha" and genuinely exercises the filtering-out of "beta".
        app.on_key(ratatui::crossterm::event::KeyCode::Char('l'));
        let text = draw(&app, 100, 12);
        assert!(text.contains("filter: l"), "{text}");
        assert!(!text.contains("beta"), "filtered-out rows are gone: {text}");
    }

    #[test]
    fn input_mode_shows_the_prompt_and_buffer() {
        let mut app = crate::tui::app::App::new(vec![row("alpha", "claude")], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('t'));
        app.on_key(ratatui::crossterm::event::KeyCode::Char('r'));
        let text = draw(&app, 100, 12);
        assert!(text.contains("tag: r"), "{text}");
    }

    #[test]
    fn confirm_mode_shows_a_dialog_naming_the_workspace() {
        let mut app = crate::tui::app::App::new(vec![row("alpha", "claude")], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('r'));
        let text = draw(&app, 100, 12);
        assert!(text.contains("Remove alpha"), "{text}");
        assert!(text.contains("[y/N]"), "{text}");
    }

    #[test]
    fn detail_pane_shows_the_selected_workspaces_objective_and_counts() {
        let d = tempfile::TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("notebook")).unwrap();
        std::fs::write(ws.join("README.md"), "## Objective\n\nShip the TUI.\n").unwrap();
        std::fs::write(ws.join("notebook/notebook.me.md"), "found the bug\n").unwrap();

        let mut r = row("alpha", "claude");
        r.path = d.path().to_path_buf();
        let mut app = crate::tui::app::App::new(vec![r], 0);
        // `render` reads the detail cache rather than gathering itself now;
        // the event loop primes it with `app.detail()` before every draw, so
        // the test must too.
        app.detail();
        let text = draw(&app, 100, 24);
        assert!(text.contains("Ship the TUI"), "objective: {text}");
        assert!(text.contains("found the bug"), "notebook tail: {text}");
        assert!(text.contains("queue 0"), "queue count shown even when Phase 8 is absent: {text}");
    }

    #[test]
    fn detail_pane_with_no_selection_renders_without_panicking() {
        let app = crate::tui::app::App::new(vec![], 0);
        let text = draw(&app, 100, 24);
        assert!(text.contains("No workspaces"), "{text}");
    }

    /// Find the foreground color of the cell that starts a run of `needle`,
    /// searching the buffer row by row. Locating cells by their rendered text
    /// (rather than hand-computed column offsets) keeps this test resilient
    /// to width/spacing changes in the table layout.
    fn fg_of(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<ratatui::style::Color> {
        let area = buf.area;
        let wanted: Vec<char> = needle.chars().collect();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let matches = wanted.iter().enumerate().all(|(i, ch)| {
                    buf.cell((x + i as u16, y))
                        .map(|c| c.symbol() == ch.to_string())
                        .unwrap_or(false)
                });
                if matches {
                    return buf.cell((x, y)).map(|c| c.fg);
                }
            }
        }
        None
    }

    #[test]
    fn list_applies_theme_colors_to_live_tags_and_corrupt_cells() {
        // No render test anywhere else asserts on color — every other
        // assertion in this module is text-only, so the styling code in
        // `render_list` has had zero regression coverage. Assert on the
        // buffer's cell colors directly, keyed by unique on-screen text so
        // the lookups don't depend on hand-computed column math.
        let mut live_row = row("alpha", "claude");
        live_row.live_pid = Some(1234);
        live_row.tags = vec!["urgent".into()];

        let mut corrupt_row = row("broken", "claude");
        corrupt_row.state = RowState::Corrupt("workspace.toml is corrupt".into());

        let mut app = crate::tui::app::App::new(vec![live_row, corrupt_row], 0);
        // Select neither row under test for color, so `render_detail`'s own
        // (differently-colored) header/corrupt lines can't be mistaken for
        // the list's cells at the text positions we search for.
        app.rows.push(row("gamma", "gemini"));
        app.selected = 2;

        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| render(f, &app)).unwrap();
        let buf = term.backend().buffer();

        assert_eq!(
            fg_of(buf, LIVE_MARK),
            Some(app.theme.live),
            "the live marker must be styled with theme.live"
        );
        assert_eq!(
            fg_of(buf, "urgent"),
            Some(app.theme.dim),
            "the tags cell must be styled with theme.dim"
        );
        assert_eq!(
            fg_of(buf, "(corrupt)"),
            Some(app.theme.warn),
            "a corrupt row's state cell must be styled with theme.warn"
        );
    }

    #[test]
    fn the_confirm_dialog_names_the_path_and_what_will_go() {
        let mut r = row("alpha", "claude");
        r.path = "/tmp/projects/alpha".into();
        let mut app = crate::tui::app::App::new(vec![r], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('r'));
        let text = draw(&app, 120, 24);
        assert!(text.contains("/tmp/projects/alpha"), "the path is shown: {text}");
        assert!(text.contains("[y/N]"), "{text}");
    }

    /// Deferred item 2. Asserting the path and `[y/N]` says nothing about the
    /// *scope* sentence, so a regression that hardcoded either branch would
    /// pass. This is the only test guarding the wording of a dialog that
    /// immediately precedes `remove_dir_all`, so it must exercise both
    /// branches and assert each one excludes the other.
    #[test]
    fn the_confirm_dialog_states_the_deletion_scope_for_both_branches() {
        const WHOLE: &str = "the whole workspace directory";
        const WS_ONLY: &str = "only its .ws/";

        // A workspace that is a direct child of the sessions root: the whole
        // directory goes. Real directories, because the predicate
        // canonicalizes.
        let d = tempfile::TempDir::new().unwrap();
        let root = d.path().join("roots");
        let managed = root.join("alpha");
        std::fs::create_dir_all(&managed).unwrap();
        std::env::set_var("WS_ROOT", &root);

        let mut r = row("alpha", "claude");
        r.path = managed.clone();
        let mut app = crate::tui::app::App::new(vec![r], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('r'));
        let managed_text = draw(&app, 120, 24);

        // An adopted project living outside the sessions root: only `.ws/`.
        let adopted = d.path().join("elsewhere").join("beta");
        std::fs::create_dir_all(&adopted).unwrap();
        let mut r = row("beta", "claude");
        r.path = adopted;
        let mut app = crate::tui::app::App::new(vec![r], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('r'));
        let adopted_text = draw(&app, 120, 24);

        // Both draws done before any assertion: a panic here would otherwise
        // leak WS_ROOT into every test that runs after this one.
        std::env::remove_var("WS_ROOT");

        assert!(managed_text.contains(WHOLE), "a managed workspace must say the whole directory goes: {managed_text}");
        assert!(!managed_text.contains(WS_ONLY), "and must not also offer the narrow wording: {managed_text}");
        assert!(adopted_text.contains(WS_ONLY), "an adopted project must say only .ws/ goes: {adopted_text}");
        assert!(!adopted_text.contains(WHOLE), "and must not claim the whole directory: {adopted_text}");
    }
}
