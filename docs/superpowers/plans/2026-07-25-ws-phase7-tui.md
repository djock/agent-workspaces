# ws Phase 7 — TUI (list + detail with agent/limit columns) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the spec §13 TUI — a ratatui workspace list (name, agent, live, status, tags, last activity, limits) with type-to-filter, Enter-to-open, `a`/`t`/`s`/`r` mutations, and a detail pane — on top of a typed read path that can tell "no workspaces" apart from "your data is broken".

**Architecture:** Task 1 extracts a typed, error-surfacing read path (`src/rows.rs`: `WorkspaceRow` + `list_workspaces()`), which both `-list` and the TUI consume — the TUI never parses prose output. Tasks 2–5 build the TUI as three separable pieces: `tui::app` (pure state + `on_key`, unit-testable with no terminal), `tui::render` (pure draw functions, tested through ratatui's `TestBackend` by asserting on the rendered buffer), and `tui::run` (the only part that touches a real terminal). Enter does not launch from inside the TUI: the event loop *returns* an outcome, `run()` restores the terminal, and `main` then calls the existing `commands::launch` exec path — so the agent replaces the TUI in the same terminal exactly as the spec asks.

**Tech Stack:** Rust 2021, `ratatui = "0.30.2"`, `crossterm = "0.29.0"` (re-exported as `ratatui::crossterm`), existing modules `meta`/`registry`/`limits`/`lock`/`readme`/`config`/`commands`.

## Global Constraints

- **cargo is not on PATH.** Every cargo invocation must be prefixed: `. "$HOME/.cargo/env"; cargo test`.
- `.cargo/config.toml` pins `RUST_TEST_THREADS=1`; unit tests mutate global env (`XDG_CONFIG_HOME`, `WS_ROOT`). Do not rely on that pin for correctness in new tests — if a new test mutates process env, serialize it explicitly with a module `static TEST_LOCK: Mutex<()>` the way `src/registry.rs` does.
- The full suite is the source of truth: `. "$HOME/.cargo/env"; cargo test` — **179 tests green** at HEAD. Every task ends green.
- Vocabulary: "workspace", never "session". Metadata dir is `.ws/`.
- **All shared-file writes are atomic and clobber-safe**: temp + rename, per-process temp name `*.tmp.<pid>`, and never overwrite a file that failed to parse.
- **Never `git add -A`.** Commit explicit paths only — the working tree carries unrelated `.cs/*` files and a stray untracked `.ws/`. Leave both alone.
- ratatui 0.30 API (verified live 2026-07-25, probe compiled and ran — do not code from 0.26-era memory): `f.area()` not `f.size()`; `Table::new(rows, widths)` with `.row_highlight_style(…)` not `.highlight_style(…)`; `TableState::default().with_selected(Some(0))`; `ratatui::init() -> DefaultTerminal` and `ratatui::restore()`; `Block::bordered()`; `List`/`ListItem`/`ListState`; `Paragraph::wrap(Wrap { trim: true })`; `Clear`; `ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers}`.
- **TestBackend snapshot idiom** (spec §16) — this is how every render test works:
  ```rust
  let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
  term.draw(|f| render_list(f, f.area(), &app)).unwrap();
  let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
  assert!(text.contains("expected"));
  ```
  Note: cell content is concatenated with no line breaks and **columns are truncated to their width constraint** (a 3-wide column renders the header `live` as `liv`). Assert on strings that actually fit the column.
- **Test hazard, hit three times already:** macOS temp paths contain `folders`, which contains `old` — bare short-substring assertions lie. Never assert the *absence* of a string the command echoes back.
- Decisions confirmed with the user for this phase: **bare `ws` with no args launches the full TUI** when stdout is a TTY and falls back to the current text list otherwise; the read path gets **typed rows with per-row corruption flags** while the existing lenient `registry::all()` / `meta::read()` stay for their current callers.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `src/rows.rs` (new) | `WorkspaceRow`, `RowState`, `ListOpts`, `list_workspaces()`, `ago()` — the one typed read path shared by `-list` and the TUI | 1 |
| `src/registry.rs` | add `all_checked() -> Result<…>`; `all()` becomes the warn-and-degrade wrapper | 1 |
| `src/meta.rs` | add `read_checked() -> Result<Option<Meta>>`; `read()` becomes the lenient wrapper | 1 |
| `src/lock.rs` | add `live_pid(&Path) -> Option<u32>` | 1 |
| `src/contract.rs` | fix the 4th instance of the fixed-temp-name bug (`state.toml`) | 1 |
| `src/commands.rs` | `list()` moves onto `rows::list_workspaces`; later `remove_one()` extracted for the TUI's `r` key | 1, 4 |
| `src/tui/mod.rs` (new) | `run()` — terminal lifecycle, event loop, `Outcome` | 2 |
| `src/tui/app.rs` (new) | `App`, `Mode`, `Action`, `on_key()` — pure, no terminal | 2, 3, 4 |
| `src/tui/render.rs` (new) | `render()`, `render_list()`, `render_detail()`, `render_dialog()` — pure draw | 2, 3, 4, 5 |
| `src/tui/detail.rs` (new) | `Detail`, `gather()` — README objective, notebook tail, timeline chain, queue/mail counts | 5 |
| `src/tui/theme.rs` (new) | `Theme`, `ThemeEnv`, `resolve()` — auto via OS appearance + `COLORFGBG`, config override wins | 5 |
| `src/cli.rs`, `src/main.rs` | `Cmd::Tui`, `-tui` flag, bare-`ws` TTY dispatch, help text | 2 |
| `tests/tui.rs` (new) | integration: non-TTY fallback, `-tui` under a pipe, corrupt-registry surfacing | 1, 2 |

---

### Task 1: Typed read path — `WorkspaceRow` + `list_workspaces()`

The TUI has no stderr the user is watching. Today `registry::all()` maps an unreadable registry to an empty vec (after a warning nobody will see under a full-screen TUI) and `meta::read()` maps a corrupt `workspace.toml` to `Meta::default()` — so a broken install renders as a serene "0 workspaces". This task gives the read path a `Result` and a per-row state, and moves `-list` onto it.

**Files:**
- Create: `src/rows.rs`
- Create: `tests/tui.rs` (first test lands here)
- Modify: `src/registry.rs` (add `all_checked`), `src/meta.rs` (add `read_checked`), `src/lock.rs` (add `live_pid`), `src/contract.rs:145` (temp-name fix), `src/commands.rs:248-279` (`list`), `src/main.rs` (add `mod rows;`)

**Interfaces:**
- Consumes: `meta::Meta`, `limits::{LimitsSnapshot, read}`, `config::{load, sessions_root}`, `workspace::resolve`.
- Produces (tasks 2–5 depend on these exact names):
  ```rust
  pub enum RowState { Ok, Missing, Corrupt(String) }
  pub struct WorkspaceRow {
      pub name: String, pub path: PathBuf, pub state: RowState,
      pub agent: String, pub live_pid: Option<u32>, pub archived: bool,
      pub tags: Vec<String>, pub status: Option<String>, pub color: Option<String>,
      pub last_activity: Option<i64>, pub limits: Option<limits::LimitsSnapshot>,
  }
  pub struct ListOpts { pub tag: Option<String>, pub include_archived: bool }
  pub fn list_workspaces(opts: &ListOpts) -> anyhow::Result<Vec<WorkspaceRow>>;
  pub fn ago(then: i64, now: i64) -> String;
  pub fn workspace_toml(path: &Path) -> PathBuf;
  ```

- [ ] **Step 1: Write the failing tests for the new leaf accessors**

Add to `src/meta.rs`'s `mod tests`:

```rust
#[test]
fn read_checked_distinguishes_missing_from_corrupt() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("workspace.toml");

    // missing → Ok(None)
    assert!(read_checked(&p).unwrap().is_none());

    // valid → Ok(Some)
    std::fs::write(&p, "name = \"alpha\"\narchived = true\n").unwrap();
    let m = read_checked(&p).unwrap().expect("parsed");
    assert_eq!(m.name, "alpha");
    assert!(m.archived);

    // corrupt → Err, and read() still degrades to defaults for existing callers
    std::fs::write(&p, "this is not toml {{{").unwrap();
    assert!(read_checked(&p).is_err(), "corrupt workspace.toml must not read as missing");
    assert_eq!(read(&p), Meta::default());
}
```

Add to `src/registry.rs`'s `mod tests`:

```rust
#[test]
fn all_checked_surfaces_a_corrupt_registry() {
    let _guard = lock();
    let _d = iso();
    let path = registry_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "this is not valid toml {{{").unwrap();

    assert!(all_checked().is_err(), "a corrupt registry must not read as zero workspaces");
    assert!(all().is_empty(), "lenient all() still degrades for existing callers");
}
```

Add to `src/lock.rs`'s `mod tests`:

```rust
#[test]
fn live_pid_reports_only_running_holders() {
    let d = TempDir::new().unwrap();
    let lf = d.path().join("lock");
    assert_eq!(live_pid(&lf), None, "no lock file → not live");

    std::fs::write(&lf, "pid = 999999\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n").unwrap();
    assert_eq!(live_pid(&lf), None, "dead pid → not live");

    let me = std::process::id();
    std::fs::write(&lf, format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();
    assert_eq!(live_pid(&lf), Some(me));
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `. "$HOME/.cargo/env"; cargo test read_checked all_checked live_pid`
Expected: FAIL — `cannot find function 'read_checked' in this scope` (and the same for `all_checked`, `live_pid`).

- [ ] **Step 3: Implement the three accessors**

In `src/meta.rs`, add above `read()`:

```rust
/// Read workspace metadata, surfacing failure instead of hiding it.
/// `Ok(None)` = the file does not exist; `Err` = it exists but could not be
/// read or parsed. Callers that walk many workspaces and want to tolerate a
/// half-built one keep using `read()`.
pub fn read_checked(ws_toml: &Path) -> Result<Option<Meta>> {
    let body = match std::fs::read_to_string(ws_toml) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", ws_toml.display())),
    };
    let t: toml::Table = toml::from_str(&body)
        .with_context(|| format!("{} is corrupt", ws_toml.display()))?;
    Ok(Some(from_table(&t)))
}
```

Refactor the body of `read()` into a shared `from_table` so the two cannot drift:

```rust
fn from_table(t: &toml::Table) -> Meta {
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(String::from);
    Meta {
        name: s("name").unwrap_or_default(),
        created: s("created").unwrap_or_default(),
        contract_version: t.get("contract_version").and_then(|v| v.as_integer()).unwrap_or(0) as u32,
        default_agent: s("default_agent"),
        archived: t.get("archived").and_then(|v| v.as_bool()).unwrap_or(false),
        tags: t
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        status: s("status"),
        color: s("color"),
    }
}

pub fn read(ws_toml: &Path) -> Meta {
    match table(ws_toml) {
        Some(t) => from_table(&t),
        None => Meta::default(),
    }
}
```

In `src/registry.rs`, rename nothing; add:

```rust
/// The registry, with read/parse failure surfaced. The TUI has no stderr the
/// user is watching, so it must be able to tell "no workspaces" from "I could
/// not read the file that lists them".
pub fn all_checked() -> Result<Vec<(String, PathBuf)>> {
    Ok(load()?
        .workspaces
        .into_iter()
        .map(|(n, p)| (n, PathBuf::from(p)))
        .collect())
}
```

In `src/lock.rs`, make the existing helpers reusable:

```rust
/// The pid currently holding `lock_file`, if the lock exists and that process
/// is still running. A stale lock (dead pid) and a missing lock both read as
/// `None` — this is the "live" indicator, not the acquisition check.
pub fn live_pid(lock_file: &Path) -> Option<u32> {
    let pid = read_pid(lock_file)?;
    pid_alive(pid).then_some(pid)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test read_checked all_checked live_pid`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/meta.rs src/registry.rs src/lock.rs
git commit -m "feat: error-surfacing read accessors (meta::read_checked, registry::all_checked, lock::live_pid)"
```

- [ ] **Step 6: Write the failing tests for `rows.rs`**

Create `src/rows.rs` containing only this test module for now (plus `mod rows;` in `src/main.rs`, alphabetically between `readme` and `registry`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // list_workspaces() resolves the registry through the process-global
    // XDG_CONFIG_HOME. Serialize explicitly rather than leaning on the
    // RUST_TEST_THREADS pin in .cargo/config.toml (see registry.rs).
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A workspace on disk: `<home>/<name>/.ws/workspace.toml` with `body`.
    fn make_ws(home: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let root = home.join(name);
        std::fs::create_dir_all(root.join(".ws")).unwrap();
        std::fs::write(root.join(".ws/workspace.toml"), body).unwrap();
        crate::registry::register(name, &root).unwrap();
        root
    }

    fn iso(d: &TempDir) {
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
    }

    #[test]
    fn lists_rows_with_meta_agent_and_tags() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        make_ws(d.path(), "alpha", "name = \"alpha\"\ndefault_agent = \"codex\"\ntags = [\"rust\"]\nstatus = \"mid-refactor\"\n");

        let rows = list_workspaces(&ListOpts::default()).unwrap();
        let r = rows.iter().find(|r| r.name == "alpha").expect("alpha listed");
        assert_eq!(r.state, RowState::Ok);
        assert_eq!(r.agent, "codex");
        assert_eq!(r.tags, vec!["rust".to_string()]);
        assert_eq!(r.status.as_deref(), Some("mid-refactor"));
        assert!(r.live_pid.is_none());
    }

    #[test]
    fn corrupt_workspace_toml_becomes_a_corrupt_row_not_a_missing_one() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        make_ws(d.path(), "broken", "not toml {{{");

        let rows = list_workspaces(&ListOpts::default()).unwrap();
        let r = rows.iter().find(|r| r.name == "broken").expect("broken still listed");
        assert!(
            matches!(r.state, RowState::Corrupt(_)),
            "a corrupt workspace.toml must be reported, not defaulted away: {:?}",
            r.state
        );
    }

    #[test]
    fn a_registered_path_with_no_ws_dir_is_missing() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        crate::registry::register("ghost", &d.path().join("ghost")).unwrap();

        let rows = list_workspaces(&ListOpts::default()).unwrap();
        let r = rows.iter().find(|r| r.name == "ghost").unwrap();
        assert_eq!(r.state, RowState::Missing);
    }

    #[test]
    fn archived_are_hidden_unless_requested_and_tag_filters() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        make_ws(d.path(), "live-one", "tags = [\"keep\"]\n");
        make_ws(d.path(), "old-one", "archived = true\ntags = [\"keep\"]\n");

        let default = list_workspaces(&ListOpts::default()).unwrap();
        assert!(default.iter().any(|r| r.name == "live-one"));
        assert!(!default.iter().any(|r| r.name == "old-one"), "archived hidden by default");

        let with_archived = list_workspaces(&ListOpts { tag: None, include_archived: true }).unwrap();
        assert!(with_archived.iter().any(|r| r.name == "old-one"));

        let tagged = list_workspaces(&ListOpts { tag: Some("nope".into()), include_archived: true }).unwrap();
        assert!(tagged.is_empty(), "tag filter excludes everything untagged");
    }

    #[test]
    fn a_corrupt_registry_is_an_error_not_an_empty_list() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        let rp = crate::registry::registry_path();
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, "not toml {{{").unwrap();

        assert!(list_workspaces(&ListOpts::default()).is_err());
    }

    #[test]
    fn ago_formats_compactly() {
        let now = 1_000_000;
        assert_eq!(ago(now, now), "now");
        assert_eq!(ago(now - 45, now), "45s");
        assert_eq!(ago(now - 3 * 60, now), "3m");
        assert_eq!(ago(now - 5 * 3600, now), "5h");
        assert_eq!(ago(now - 3 * 86400, now), "3d");
        assert_eq!(ago(now + 60, now), "now", "clock skew must not print a negative age");
    }
}
```

- [ ] **Step 7: Run them and watch them fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib rows::`
Expected: FAIL to compile — `cannot find type 'ListOpts'`, `cannot find function 'list_workspaces'`.

- [ ] **Step 8: Implement `src/rows.rs`**

Put this above the test module:

```rust
//! The one typed read path over the registry — shared by `-list` and the TUI.
//!
//! Deliberately *not* lenient: a registry that cannot be read is an error, and
//! a workspace whose `workspace.toml` is corrupt is a row that says so. The TUI
//! renders full-screen with no stderr in view, so anything swallowed here is
//! swallowed for good.
use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::limits::{self, LimitsSnapshot};

#[derive(Debug, Clone, PartialEq)]
pub enum RowState {
    /// `.ws/workspace.toml` read cleanly (or the workspace exists with no metadata yet).
    Ok,
    /// Registered, but there is no `.ws/` directory at that path any more.
    Missing,
    /// `.ws/` is there but its metadata could not be read; the message says why.
    Corrupt(String),
}

#[derive(Debug, Clone)]
pub struct WorkspaceRow {
    pub name: String,
    pub path: PathBuf,
    pub state: RowState,
    /// Recorded default agent, falling back to the config default.
    pub agent: String,
    /// `Some(pid)` when a live process holds the workspace lock.
    pub live_pid: Option<u32>,
    pub archived: bool,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub color: Option<String>,
    /// Newest mtime across the workspace's documents, epoch seconds.
    pub last_activity: Option<i64>,
    pub limits: Option<LimitsSnapshot>,
}

#[derive(Debug, Clone, Default)]
pub struct ListOpts {
    pub tag: Option<String>,
    pub include_archived: bool,
}

pub fn workspace_toml(path: &Path) -> PathBuf {
    path.join(".ws/workspace.toml")
}

/// Newest mtime (epoch seconds) among the workspace documents worth calling
/// "activity". `.ws/local/` is excluded on purpose: the bash audit log and the
/// statusline's limits.json are written constantly and would make every
/// workspace look equally fresh.
fn last_activity(ws_dir: &Path) -> Option<i64> {
    let mut newest: Option<i64> = None;
    let mut consider = |p: PathBuf| {
        if let Ok(secs) = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            })
        {
            if newest.map_or(true, |n| secs > n) {
                newest = Some(secs);
            }
        }
    };
    consider(ws_dir.join("README.md"));
    consider(ws_dir.join("timeline.jsonl"));
    for sub in ["notebook", "handoffs"] {
        if let Ok(rd) = std::fs::read_dir(ws_dir.join(sub)) {
            for e in rd.flatten() {
                consider(e.path());
            }
        }
    }
    newest
}

/// Every registered workspace as a typed row, filtered per `opts`.
/// Errors only when the registry itself cannot be read — a single broken
/// workspace is a `RowState::Corrupt` row, not a failed listing.
pub fn list_workspaces(opts: &ListOpts) -> Result<Vec<WorkspaceRow>> {
    let cfg = crate::config::load();
    let mut out = Vec::new();
    for (name, path) in crate::registry::all_checked()? {
        let ws_dir = path.join(".ws");
        let (state, meta) = if !ws_dir.is_dir() {
            (RowState::Missing, crate::meta::Meta::default())
        } else {
            match crate::meta::read_checked(&workspace_toml(&path)) {
                Ok(Some(m)) => (RowState::Ok, m),
                Ok(None) => (RowState::Ok, crate::meta::Meta::default()),
                Err(e) => (RowState::Corrupt(format!("{e:#}")), crate::meta::Meta::default()),
            }
        };

        if meta.archived && !opts.include_archived {
            continue;
        }
        if let Some(t) = &opts.tag {
            if !meta.tags.iter().any(|x| x == t) {
                continue;
            }
        }

        out.push(WorkspaceRow {
            agent: meta.default_agent.clone().unwrap_or_else(|| cfg.default_agent.clone()),
            live_pid: crate::lock::live_pid(&ws_dir.join("local/lock")),
            archived: meta.archived,
            tags: meta.tags.clone(),
            status: meta.status.clone(),
            color: meta.color.clone(),
            last_activity: last_activity(&ws_dir),
            limits: limits::read(&ws_dir.join("local/limits.json")),
            name,
            path,
            state,
        });
    }
    Ok(out)
}

/// Compact relative age for the "last activity" column: `45s`, `3m`, `5h`, `3d`.
/// A future timestamp (clock skew, a file touched by another machine) reads as
/// `now` rather than a negative age.
pub fn ago(then: i64, now: i64) -> String {
    let d = now - then;
    if d <= 0 {
        return "now".into();
    }
    match d {
        0..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86_399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86_400),
    }
}
```

**Check the lock path before you trust it:** the constant above is `.ws/local/lock`. Confirm it matches `Workspace::lock_file()` in `src/workspace.rs` and fix the string if it differs — a wrong path silently makes every workspace look idle.

- [ ] **Step 9: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test --lib rows::`
Expected: PASS (6 tests).

- [ ] **Step 10: Commit**

```bash
git add src/rows.rs src/main.rs
git commit -m "feat: typed workspace read path (rows::WorkspaceRow, list_workspaces)"
```

- [ ] **Step 11: Write the failing integration test for `-list` on the typed path**

Create `tests/tui.rs`:

```rust
mod common;
use common::Env;

#[test]
fn list_reports_a_corrupt_workspace_instead_of_hiding_it() {
    let e = Env::new();
    let ws = e.root.join("broken");
    std::fs::create_dir_all(ws.join(".ws")).unwrap();
    std::fs::write(ws.join(".ws/workspace.toml"), "not toml {{{").unwrap();
    e.cmd().args(["-adopt", "broken"]).current_dir(&ws).assert().success();

    let out = e.cmd().arg("-list").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("broken"), "the workspace is still listed: {text}");
    assert!(text.contains("corrupt"), "and its state is reported: {text}");
}

#[test]
fn list_fails_loudly_when_the_registry_itself_is_unreadable() {
    let e = Env::new();
    let reg = e.home.path().join(".config/ws/registry.toml");
    std::fs::create_dir_all(reg.parent().unwrap()).unwrap();
    std::fs::write(&reg, "not toml {{{").unwrap();

    let out = e.cmd().arg("-list").output().unwrap();
    assert!(!out.status.success(), "a corrupt registry must not exit 0 with an empty list");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("registry.toml"), "stderr names the file: {err}");
}
```

Check `-adopt`'s actual argument form in `src/cli.rs` before running — if it takes the name positionally as written above, keep it; otherwise adopt from inside the directory with no name.

- [ ] **Step 12: Run and watch it fail**

Run: `. "$HOME/.cargo/env"; cargo test --test tui`
Expected: FAIL — `-list` neither says "corrupt" nor exits non-zero.

- [ ] **Step 13: Move `commands::list` onto the typed path**

Replace `src/commands.rs:248-279` with:

```rust
pub fn list(tag: Option<String>, archived: bool) -> Result<()> {
    let opts = crate::rows::ListOpts { tag: tag.clone(), include_archived: archived };
    let rows = crate::rows::list_workspaces(&opts)?;
    if rows.is_empty() {
        match tag {
            Some(t) => println!("no workspaces tagged {t}"),
            None if archived => println!("no workspaces yet — create one with: ws <name>"),
            None => println!("no active workspaces (try: ws -list --archived)"),
        }
        return Ok(());
    }
    for r in rows {
        let state = match &r.state {
            crate::rows::RowState::Ok => String::new(),
            crate::rows::RowState::Missing => "  (missing)".to_string(),
            crate::rows::RowState::Corrupt(e) => format!("  (corrupt: {e})"),
        };
        let flag = if r.archived { "  [archived]" } else { "" };
        let tags = if r.tags.is_empty() { String::new() } else { format!("  [{}]", r.tags.join(" ")) };
        let status = r.status.map(|s| format!("  — {s}")).unwrap_or_default();
        println!("{}\t{}{state}{flag}{tags}{status}", r.name, r.path.display());
    }
    Ok(())
}
```

Note the behavior change this locks in: with a corrupt registry, `list_workspaces` returns `Err`, `main` prints `ws: …` and exits 1 — replacing the old warn-and-print-nothing path. That is the point of the task.

- [ ] **Step 14: Run the whole suite**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS. Existing `-list` integration tests may assert on the old empty-registry wording — if one fails, read it and update the *assertion* to the new contract; do not soften the code back.

- [ ] **Step 15: Fix the fourth instance of the fixed-temp-name bug**

`src/contract.rs:145` still writes `state.toml` through a shared temp name, the same shape fixed in `meta::update`, `registry::save`, and `limits::write`. `write_session_id` is called from hook handlers, and several `ws` processes are routinely live. Change:

```rust
    let tmp = state_toml.with_extension(format!("toml.tmp.{}", std::process::id()));
```

and make the failure path clean up after itself the way the siblings do:

```rust
    let write_result = std::fs::write(&tmp, toml::to_string_pretty(&t)?);
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result?;
    if let Err(e) = std::fs::rename(&tmp, state_toml) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).context("failed to rename state.toml into place");
    }
    Ok(())
```

- [ ] **Step 16: Run the suite and commit**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS (182+ tests).

```bash
git add src/commands.rs src/contract.rs tests/tui.rs
git commit -m "feat: -list reports corrupt/missing workspaces; fix state.toml temp-name race"
```

---

### Task 2: TUI skeleton — workspace table, `-tui` and bare `ws`

**Files:**
- Create: `src/tui/mod.rs`, `src/tui/app.rs`, `src/tui/render.rs`
- Modify: `src/main.rs` (`mod tui;`, dispatch, help), `src/cli.rs` (`Cmd::Tui`, `-tui`, bare-`ws` TTY branch)
- Test: unit tests inside `src/tui/app.rs` and `src/tui/render.rs`; integration in `tests/tui.rs`

**Interfaces:**
- Consumes: `rows::{WorkspaceRow, RowState, ListOpts, list_workspaces, ago}`, `limits::{countdown, now_epoch}`.
- Produces:
  ```rust
  // src/tui/app.rs
  pub enum Mode { Browse }                       // grows in tasks 3–4
  pub enum Action { None, Quit }                 // grows in tasks 3–4
  pub struct App { pub rows: Vec<WorkspaceRow>, pub selected: usize, pub now: i64,
                   pub mode: Mode, pub message: Option<String>, pub show_archived: bool }
  impl App { pub fn new(rows: Vec<WorkspaceRow>, now: i64) -> Self;
             pub fn visible(&self) -> Vec<usize>;
             pub fn selected_row(&self) -> Option<&WorkspaceRow>;
             pub fn on_key(&mut self, key: KeyCode) -> Action; }
  // src/tui/render.rs
  pub fn render(f: &mut Frame, app: &App);
  pub fn render_list(f: &mut Frame, area: Rect, app: &App);
  // src/tui/mod.rs
  pub enum Outcome { Quit }                      // grows in task 3
  pub fn run() -> anyhow::Result<Outcome>;
  ```

- [ ] **Step 1: Add the dependencies**

In `Cargo.toml` under `[dependencies]`:

```toml
ratatui = "0.30.2"
crossterm = "0.29.0"
```

Run: `. "$HOME/.cargo/env"; cargo build`
Expected: both resolve and the crate still builds.

- [ ] **Step 2: Write the failing render test**

Create `src/tui/render.rs` with only this test module:

```rust
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
        let app = crate::tui::app::App::new(vec![row("alpha", "claude"), row("beta", "codex")], 0);
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
        let text = draw(&app, 100, 12);
        assert!(text.contains(LIVE_MARK), "live workspace is marked: {text}");
        assert!(text.contains("62%"), "per-agent limit state is visible: {text}");
    }

    #[test]
    fn a_corrupt_workspace_says_so_on_screen() {
        let mut r = row("broken", "claude");
        r.state = RowState::Corrupt("workspace.toml is corrupt".into());
        let app = crate::tui::app::App::new(vec![r], 0);
        let text = draw(&app, 100, 12);
        assert!(text.contains("corrupt"), "the TUI must never render breakage as emptiness: {text}");
    }

    #[test]
    fn empty_registry_says_it_is_empty() {
        let app = crate::tui::app::App::new(vec![], 0);
        let text = draw(&app, 100, 12);
        assert!(text.contains("No workspaces"), "{text}");
    }
}
```

- [ ] **Step 3: Run and watch it fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::`
Expected: FAIL to compile — `render`, `LIVE_MARK`, and `tui::app::App` do not exist.

- [ ] **Step 4: Implement `App` (state only, no keys yet)**

Create `src/tui/app.rs`:

```rust
//! TUI state and key handling — deliberately free of any terminal I/O so the
//! whole interaction model is unit-testable.
use crate::rows::WorkspaceRow;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
}

/// What the event loop should do after a key. `None` = redraw and keep going.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
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
    pub show_archived: bool,
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
        }
    }

    /// Indices of the rows currently on screen. Task 3 adds the filter; for
    /// now this is every row.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.rows.len()).collect()
    }

    pub fn selected_row(&self) -> Option<&WorkspaceRow> {
        self.rows.get(self.selected)
    }
}
```

- [ ] **Step 5: Implement `render.rs` above its test module**

```rust
//! Pure drawing. Every function takes `&App` and a `Rect` and renders — no
//! state changes, no I/O — so `TestBackend` can snapshot all of it.
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::rows::{ago, RowState, WorkspaceRow};
use crate::tui::app::App;

/// ASCII on purpose: the list must stay readable without a Nerd Font, and
/// `config.nerd_fonts` glyphs are a Phase 9 concern.
pub const LIVE_MARK: &str = "*";

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
    match &r.state {
        RowState::Ok => r.status.clone().unwrap_or_default(),
        RowState::Missing => "(missing)".into(),
        RowState::Corrupt(_) => "(corrupt)".into(),
    }
}

pub fn render_list(f: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible();
    if visible.is_empty() {
        f.render_widget(
            Paragraph::new("No workspaces yet — create one with: ws <name>")
                .block(Block::bordered().title("workspaces")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| {
            let r = &app.rows[i];
            Row::new(vec![
                r.name.clone(),
                r.agent.clone(),
                if r.live_pid.is_some() { LIVE_MARK.to_string() } else { " ".into() },
                state_cell(r),
                r.tags.join(","),
                r.last_activity.map(|t| ago(t, app.now)).unwrap_or_else(|| "—".into()),
                limits_cell(r, app.now),
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
    let text = match &app.message {
        Some(m) => m.clone(),
        None => "enter open   q quit".to_string(),
    };
    f.render_widget(Paragraph::new(text), area);
}

pub fn render(f: &mut Frame, app: &App) {
    let areas = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());
    render_list(f, areas[0], app);
    render_footer(f, areas[1], app);
}
```

The corrupt-row test asserts on the word `corrupt`, which `state_cell` renders as `(corrupt)` into a `Min(12)` column — wide enough at the 100-column test size.

- [ ] **Step 6: Run the render tests**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::`
Expected: PASS (4 tests). If `62%` fails, print `text` and check the limits column width — widen the constraint rather than weakening the assertion.

- [ ] **Step 7: Implement the terminal lifecycle**

Create `src/tui/mod.rs`:

```rust
//! The ratatui dashboard. `run()` owns the terminal; everything it needs to
//! decide is computed by `app` and drawn by `render`, both terminal-free.
pub mod app;
pub mod render;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use self::app::{Action, App};

/// What the TUI wants done after it gives the terminal back.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Quit,
}

/// Draw/handle-key loop. Restores the terminal on every exit path, including
/// an error, so a panic-free failure never leaves the user in raw mode.
pub fn run() -> Result<Outcome> {
    let rows = crate::rows::list_workspaces(&crate::rows::ListOpts::default())?;
    let mut app = App::new(rows, crate::limits::now_epoch());

    let mut term = ratatui::init();
    let result = event_loop(&mut term, &mut app);
    ratatui::restore();
    result
}

fn event_loop(term: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<Outcome> {
    loop {
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
            Action::None => {}
        }
    }
}
```

Add `on_key` to `App`:

```rust
    /// Handle one key. Pure: the caller decides what `Action` means.
    pub fn on_key(&mut self, key: KeyCode) -> Action {
        self.message = None;
        match key {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            _ => Action::None,
        }
    }
```

with `use ratatui::crossterm::event::KeyCode;` at the top of `app.rs`.

- [ ] **Step 8: Wire `-tui`, bare `ws`, and the help text**

In `src/cli.rs`, add `Tui` to `enum Cmd`, and in `parse`:

```rust
        "-tui" => Ok(Cmd::Tui),
```

and change the no-args arm — the user's decision is that bare `ws` **is** the TUI on a terminal, with the text list as the non-interactive fallback:

```rust
    let first = match it.next() {
        // Bare `ws` opens the dashboard interactively; piped or redirected
        // (scripts, `ws | grep`) it stays the plain list it has always been.
        None => {
            return Ok(if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                Cmd::Tui
            } else {
                Cmd::List { tag: None, archived: false }
            })
        }
        Some(a) => a,
    };
```

with `use std::io::IsTerminal;` imported (or the fully-qualified call above). In `src/main.rs` add `mod tui;`, dispatch:

```rust
        Cmd::Tui => match tui::run()? {
            tui::Outcome::Quit => {}
        },
```

and add to `print_help()`:

```
         ws                   open the workspace dashboard (TUI)\n\
         ws -tui              same, explicitly\n\
```

- [ ] **Step 9: Write and run the integration test for the non-TTY fallback**

Append to `tests/tui.rs`:

```rust
#[test]
fn bare_ws_falls_back_to_the_text_list_when_not_a_tty() {
    let e = Env::new();
    // assert_cmd pipes stdout, so this exercises the non-TTY branch. A TUI
    // would emit terminal escape sequences or hang waiting for a keypress.
    let out = e.cmd().output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no workspaces yet"), "{text}");
}
```

Run: `. "$HOME/.cargo/env"; cargo test --test tui`
Expected: PASS.

Do **not** add an integration test that runs `ws -tui` under a pipe: `ratatui::init()` on a non-terminal either errors or blocks on `event::read()`, and a hanging test is worse than no test. The interactive path is covered by the `App`/render unit tests.

- [ ] **Step 10: Run the whole suite and commit**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS.

```bash
git add Cargo.toml Cargo.lock src/tui/mod.rs src/tui/app.rs src/tui/render.rs src/main.rs src/cli.rs tests/tui.rs
git commit -m "feat: ratatui workspace dashboard skeleton (ws -tui, bare ws on a TTY)"
```

---

### Task 3: Type-to-filter, selection, and Enter-opens-in-this-terminal

**Files:**
- Modify: `src/tui/app.rs` (filter state, movement, `Action::Launch`), `src/tui/mod.rs` (`Outcome::Launch`), `src/tui/render.rs` (filter line), `src/main.rs` (launch after restore)
- Test: unit tests in `src/tui/app.rs` and `src/tui/render.rs`

**Interfaces:**
- Consumes: task 2's `App`, `Action`, `Outcome`, `render`.
- Produces:
  ```rust
  pub enum Mode { Browse, Filter }
  pub enum Action { None, Quit, Launch(String) }
  pub enum Outcome { Quit, Launch(String) }
  impl App { pub fn filter: String; pub fn move_selection(&mut self, delta: i32); }
  ```

- [ ] **Step 1: Write the failing key-handling tests**

Add to `src/tui/app.rs`:

```rust
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
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::app`
Expected: FAIL — `Mode::Filter`, `a.filter`, and `Action::Launch` do not exist; movement keys do nothing.

- [ ] **Step 3: Implement filtering and movement**

In `src/tui/app.rs`, extend the enums and `App`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
    Filter,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    Launch(String),
}
```

Add `pub filter: String` to `App` (initialize to `String::new()` in `new`), and replace `visible`/`on_key`:

```rust
    /// Indices of rows matching the current filter (case-insensitive substring
    /// over the name), in registry order.
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&i| needle.is_empty() || self.rows[i].name.to_lowercase().contains(&needle))
            .collect()
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
    fn reconcile_selection(&mut self) {
        let visible = self.visible();
        if visible.is_empty() || visible.contains(&self.selected) {
            return;
        }
        self.selected = visible[0];
    }

    pub fn on_key(&mut self, key: KeyCode) -> Action {
        self.message = None;
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
                KeyCode::Enter => self.launch_selected(),
                _ => Action::None,
            },
            Mode::Browse => match key {
                KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                KeyCode::Char('/') => { self.mode = Mode::Filter; Action::None }
                KeyCode::Char('j') | KeyCode::Down => { self.move_selection(1); Action::None }
                KeyCode::Char('k') | KeyCode::Up => { self.move_selection(-1); Action::None }
                KeyCode::Enter => self.launch_selected(),
                _ => Action::None,
            },
        }
    }

    fn launch_selected(&mut self) -> Action {
        if !self.visible().contains(&self.selected) {
            return Action::None;
        }
        match self.selected_row() {
            Some(r) => Action::Launch(r.name.clone()),
            None => Action::None,
        }
    }
```

Note `Enter` stays live in filter mode: typing three letters and hitting Enter is the whole point of a picker.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::app`
Expected: PASS (6 tests).

- [ ] **Step 5: Write the failing render test for the filter line**

Add to `src/tui/render.rs`'s test module:

```rust
    #[test]
    fn filter_mode_shows_what_is_being_typed() {
        let mut app = crate::tui::app::App::new(vec![row("alpha", "claude"), row("beta", "codex")], 0);
        app.on_key(ratatui::crossterm::event::KeyCode::Char('/'));
        app.on_key(ratatui::crossterm::event::KeyCode::Char('a'));
        let text = draw(&app, 100, 12);
        assert!(text.contains("filter: a"), "{text}");
        assert!(!text.contains("beta"), "filtered-out rows are gone: {text}");
    }
```

`beta` is safe to assert absent here: nothing else on screen echoes it (the footer shows only key hints, and the paths are not rendered).

- [ ] **Step 6: Run it, then render the filter line**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::render`
Expected: FAIL on `filter: a`.

In `render_footer`, prefer the filter over the hints:

```rust
fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let text = match (&app.message, &app.mode) {
        (Some(m), _) => m.clone(),
        (None, Mode::Filter) => format!("filter: {}", app.filter),
        (None, Mode::Browse) => "enter open   / filter   q quit".to_string(),
    };
    f.render_widget(Paragraph::new(text), area);
}
```

with `use crate::tui::app::Mode;`.

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::render`
Expected: PASS.

- [ ] **Step 7: Hand the launch back to `main` so the agent replaces the TUI**

In `src/tui/mod.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Quit,
    /// The user picked a workspace. The caller launches it *after* `run()` has
    /// restored the terminal — the agent then takes over this same terminal,
    /// which is what spec §13 means by "opens in the current terminal".
    Launch(String),
}
```

and in `event_loop`:

```rust
            Action::Launch(name) => return Ok(Outcome::Launch(name)),
```

In `src/main.rs`:

```rust
        Cmd::Tui => match tui::run()? {
            tui::Outcome::Quit => {}
            // run() has already restored the terminal; launch execs into the
            // agent from here, replacing this process.
            tui::Outcome::Launch(name) => commands::launch(name, None, false, false, false)?,
        },
```

- [ ] **Step 8: Run the whole suite and commit**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS.

```bash
git add src/tui/app.rs src/tui/mod.rs src/tui/render.rs src/main.rs
git commit -m "feat: TUI type-to-filter, selection, and enter-to-open"
```

- [ ] **Step 9: Drive the real TUI once by hand**

The unit tests cannot prove `ratatui::init()`/`restore()` behave on a real terminal. Run `. "$HOME/.cargo/env"; cargo run -- -tui` in this terminal, confirm the list draws, arrow keys move, `/` filters, `q` exits, **and the shell prompt comes back working (no raw mode, no hidden cursor)**. Then pick a workspace with Enter and confirm the agent starts in the same terminal. Report what you saw in the task report — this is the one check the suite cannot make.

---

### Task 4: The `a`/`t`/`s`/`r` mutations

**Files:**
- Modify: `src/tui/app.rs` (`Mode::Input`, `Mode::Confirm`, mutation dispatch), `src/tui/render.rs` (input + confirm dialog), `src/commands.rs` (extract `remove_one`)
- Test: unit tests in both TUI modules; a `commands` test for `remove_one`

**Interfaces:**
- Consumes: `meta::{add_tags, set_status, set_archived}`, task 3's `App`.
- Produces:
  ```rust
  pub enum InputField { Tag, Status }
  pub enum Mode { Browse, Filter, Input(InputField), Confirm }
  impl App { pub fn buffer: String; pub fn apply(&mut self) -> anyhow::Result<()>; pub fn reload(&mut self); }
  // src/commands.rs
  pub fn remove_one(name: &str, path: &Path) -> anyhow::Result<()>;
  ```

- [ ] **Step 1: Extract the non-interactive core of `rm` (failing test first)**

Add to `src/commands.rs` a test module (or extend the existing one) with:

```rust
#[cfg(test)]
mod remove_tests {
    use tempfile::TempDir;

    #[test]
    fn remove_one_deletes_only_ws_for_an_adopted_project() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::fs::write(project.join("keep-me.txt"), "source code").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        super::remove_one("myproj", &project).unwrap();

        assert!(!project.join(".ws").exists(), ".ws is gone");
        assert!(project.join("keep-me.txt").exists(), "an adopted project itself must survive");
        assert!(crate::registry::lookup("myproj").is_none(), "and the registry entry is cleared");
    }
}
```

This test sets process-global env; if it races under `--test-threads>1`, give it the same explicit `Mutex` treatment as `registry.rs`.

- [ ] **Step 2: Run and watch it fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib remove_one`
Expected: FAIL — `cannot find function 'remove_one'`.

- [ ] **Step 3: Extract it, and make `rm` call it**

In `src/commands.rs`, lift the body of the loop in `rm` (everything after the confirmation prompt) into:

```rust
/// Remove one workspace with no prompting: the whole directory when it lives
/// under the workspaces root, otherwise just its `.ws/` (an adopted project
/// keeps its source), then drop the registry entry.
pub fn remove_one(name: &str, path: &std::path::Path) -> Result<()> {
    let cfg = config::load();
    let root = config::sessions_root(&cfg);
    // Canonicalize before comparing: on macOS temp dirs live under a symlink
    // (/var -> /private/var), so a literal prefix check can mismatch even when
    // one path is truly nested under the other.
    let root_c = root.canonicalize().unwrap_or_else(|_| root.clone());
    let path_c = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let result = if path_c.starts_with(&root_c) {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_dir_all(path.join(".ws"))
    };
    if let Err(e) = result {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e).with_context(|| format!("failed to remove {name}"));
        }
        // NotFound → already gone; fall through and unregister the stale entry.
    }
    crate::registry::unregister(name)
}
```

Rewrite `rm`'s loop body to keep its existing behavior — the TTY confirmation, the `ws: failed to remove …` warning, `continue`-on-error, and `println!("removed {name}")` — on top of `remove_one`. Add `use anyhow::Context;` if it is not already imported.

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS, including the existing `rm` integration tests.

- [ ] **Step 4: Write the failing tests for the mutation keys**

Add to `src/tui/app.rs`'s test module:

```rust
    use tempfile::TempDir;

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
    fn mutation_keys_are_inert_when_nothing_is_selected() {
        let mut a = App::new(vec![], 0);
        for k in ['a', 't', 's', 'r'] {
            assert_eq!(a.on_key(KeyCode::Char(k)), Action::None);
            assert_eq!(a.mode, Mode::Browse, "{k} must not open a prompt with no rows");
        }
    }
```

The last two tests mutate global env; take the same `TEST_LOCK` guard used by the `rows` tests (add one to this module) so they cannot interleave.

- [ ] **Step 5: Run and watch them fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::app`
Expected: FAIL — `InputField`, `Mode::Input`, `Mode::Confirm`, `App::buffer` do not exist.

- [ ] **Step 6: Implement the mutations**

In `src/tui/app.rs`:

```rust
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
```

Add `pub buffer: String` to `App` (empty in `new`). Extend `on_key`'s `Mode::Browse` arm:

```rust
                KeyCode::Char('t') if self.selected_row().is_some() => {
                    self.mode = Mode::Input(InputField::Tag);
                    self.buffer.clear();
                    Action::None
                }
                KeyCode::Char('s') if self.selected_row().is_some() => {
                    self.mode = Mode::Input(InputField::Status);
                    // Pre-fill with the current status so `s` edits rather than retypes.
                    self.buffer = self.selected_row().and_then(|r| r.status.clone()).unwrap_or_default();
                    Action::None
                }
                KeyCode::Char('a') if self.selected_row().is_some() => {
                    self.toggle_archived();
                    Action::None
                }
                KeyCode::Char('r') if self.selected_row().is_some() => {
                    self.mode = Mode::Confirm;
                    Action::None
                }
```

(placed **before** the `KeyCode::Char('j')`/`'k'` arms only if no letters collide — `a`, `t`, `s`, `r` do not collide with `j`/`k`/`q`/`/`, so ordering is free. The `if self.selected_row().is_some()` guards are what make the empty-list test pass.)

Add the two new modes to `on_key`'s outer `match`:

```rust
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
                    if let Err(e) = self.remove_selected() {
                        self.message = Some(format!("failed: {e:#}"));
                    }
                    self.mode = Mode::Browse;
                    Action::None
                }
                _ => { self.mode = Mode::Browse; Action::None }
            },
```

and the three operations. Each writes through `meta::*` (atomic and clobber-safe) and then updates the in-memory row, so the screen never disagrees with the disk:

```rust
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
                    "{} {}",
                    self.rows[self.selected].name,
                    if next { "archived" } else { "unarchived" }
                ));
            }
            Err(e) => self.message = Some(format!("failed: {e:#}")),
        }
    }

    fn remove_selected(&mut self) -> anyhow::Result<()> {
        let Some(r) = self.rows.get(self.selected) else { return Ok(()) };
        let (name, path) = (r.name.clone(), r.path.clone());
        crate::commands::remove_one(&name, &path)?;
        self.rows.remove(self.selected);
        // The removed row's index now points at its successor (or past the end).
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        self.reconcile_selection();
        self.message = Some(format!("removed {name}"));
        Ok(())
    }
```

Confirm `meta::set_archived`'s exact signature in `src/meta.rs` before writing the call — the plan assumes `set_archived(&Path, bool) -> Result<()>`.

Note the archived row stays on screen after `a` even though `show_archived` is false: re-filtering it away mid-keystroke would make the list jump under the user's hands. The footer message is the feedback; the next launch of the TUI hides it.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::app`
Expected: PASS (13 tests).

- [ ] **Step 8: Write the failing render test for the prompt and dialog**

Add to `src/tui/render.rs`'s test module:

```rust
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
```

`t` needs a selected row, and `row()` builds one with `path: /tmp/alpha` — the render test never writes, so a non-existent path is fine here.

- [ ] **Step 9: Run it, then render them**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::render`
Expected: FAIL on both.

Extend `render_footer`'s match:

```rust
        (None, Mode::Input(InputField::Tag)) => format!("tag: {}", app.buffer),
        (None, Mode::Input(InputField::Status)) => format!("status: {}", app.buffer),
        (None, Mode::Confirm) => String::new(), // the dialog carries the question
        (None, Mode::Browse) => "enter open   / filter   a archive   t tag   s status   r remove   q quit".to_string(),
```

and add the dialog to `render`, drawn last so it sits on top:

```rust
fn render_confirm(f: &mut Frame, area: Rect, app: &App) {
    let Some(r) = app.selected_row() else { return };
    // A small centered box: half the width, three lines tall.
    let vertical = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .split(vertical[1]);
    let dialog = horizontal[1];
    f.render_widget(Clear, dialog);
    f.render_widget(
        Paragraph::new(format!("Remove {}? [y/N]", r.name)).block(Block::bordered()),
        dialog,
    );
}

pub fn render(f: &mut Frame, app: &App) {
    let areas = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());
    render_list(f, areas[0], app);
    render_footer(f, areas[1], app);
    if app.mode == Mode::Confirm {
        render_confirm(f, f.area(), app);
    }
}
```

with `use ratatui::widgets::Clear;` and `use crate::tui::app::{InputField, Mode};`. Give the dialog enough width at the 100-column test size that `Remove alpha? [y/N]` is not truncated — 60% of 100 minus borders is 58 characters, which fits.

- [ ] **Step 10: Run everything and commit**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS.

```bash
git add src/tui/app.rs src/tui/render.rs src/commands.rs
git commit -m "feat: TUI archive/tag/status/remove keys with confirm dialog"
```

---

### Task 5: Detail pane and theme

**Files:**
- Create: `src/tui/detail.rs`, `src/tui/theme.rs`
- Modify: `src/tui/render.rs` (split layout, detail pane, themed styles), `src/tui/mod.rs` (`pub mod detail; pub mod theme;`, pass the theme), `src/tui/app.rs` (hold the resolved theme)
- Test: unit tests in `detail.rs`, `theme.rs`, and `render.rs`

**Interfaces:**
- Consumes: `readme::objective_of`, `rows::WorkspaceRow`, `config::Config`.
- Produces:
  ```rust
  // src/tui/detail.rs
  pub struct ChainEntry { pub ts: String, pub kind: String, pub actor: String }
  pub struct Detail { pub objective: Option<String>, pub notebook: Vec<String>,
                      pub chain: Vec<ChainEntry>, pub queue: usize, pub mail: usize }
  pub fn gather(row: &WorkspaceRow, max_lines: usize) -> Detail;
  // src/tui/theme.rs
  pub struct ThemeEnv { pub no_color: bool, pub colorfgbg: Option<String>, pub os_dark: Option<bool> }
  pub struct Theme { pub plain: bool, pub accent: Color, pub dim: Color, pub live: Color, pub warn: Color }
  impl ThemeEnv { pub fn detect() -> Self; }
  pub fn resolve(cfg_theme: &str, env: &ThemeEnv) -> Theme;
  ```

- [ ] **Step 1: Write the failing detail tests**

Create `src/tui/detail.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{RowState, WorkspaceRow};
    use tempfile::TempDir;

    fn ws_at(path: std::path::PathBuf) -> WorkspaceRow {
        WorkspaceRow {
            name: "alpha".into(), path, state: RowState::Ok, agent: "claude".into(),
            live_pid: None, archived: false, tags: vec![], status: None, color: None,
            last_activity: None, limits: None,
        }
    }

    #[test]
    fn gathers_objective_notebook_tail_and_chain() {
        let d = TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("notebook")).unwrap();
        std::fs::write(
            ws.join("README.md"),
            "# Workspace: alpha\n\n## Objective\n\nShip the TUI.\n\n## Environment\n\nmacOS\n",
        )
        .unwrap();
        std::fs::write(ws.join("notebook/notebook.me.md"), "line one\nline two\nline three\n").unwrap();
        std::fs::write(
            ws.join("timeline.jsonl"),
            "{\"ts\":\"2026-07-01T00:00:00Z\",\"kind\":\"created\",\"actor\":\"me\"}\n\
             {\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"me\"}\n",
        )
        .unwrap();

        let det = gather(&ws_at(d.path().to_path_buf()), 2);
        assert_eq!(det.objective.as_deref(), Some("Ship the TUI."));
        assert_eq!(det.notebook, vec!["line two".to_string(), "line three".to_string()],
                   "the tail, newest last");
        assert_eq!(det.chain.len(), 2);
        assert_eq!(det.chain[1].kind, "opened", "newest last");
        assert_eq!(det.queue, 0);
        assert_eq!(det.mail, 0);
    }

    #[test]
    fn a_workspace_with_nothing_in_it_gathers_empty_not_panicking() {
        let d = TempDir::new().unwrap();
        let det = gather(&ws_at(d.path().to_path_buf()), 5);
        assert!(det.objective.is_none());
        assert!(det.notebook.is_empty());
        assert!(det.chain.is_empty());
    }

    #[test]
    fn a_corrupt_timeline_line_is_skipped_not_fatal() {
        let d = TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("timeline.jsonl"),
            "not json at all\n{\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"me\"}\n",
        )
        .unwrap();
        let det = gather(&ws_at(d.path().to_path_buf()), 5);
        assert_eq!(det.chain.len(), 1);
    }

    #[test]
    fn counts_queue_and_mail_when_phase_8_creates_them() {
        let d = TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("queue")).unwrap();
        std::fs::create_dir_all(ws.join("mail")).unwrap();
        std::fs::write(ws.join("queue/task-1.md"), "x").unwrap();
        std::fs::write(ws.join("queue/task-2.md"), "x").unwrap();
        std::fs::write(ws.join("mail/msg.json"), "x").unwrap();
        let det = gather(&ws_at(d.path().to_path_buf()), 5);
        assert_eq!(det.queue, 2);
        assert_eq!(det.mail, 1);
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::detail`
Expected: FAIL — `gather` does not exist.

- [ ] **Step 3: Implement `detail.rs`**

```rust
//! Everything the detail pane shows for one workspace, read on demand.
//!
//! Nothing here is fatal: a workspace mid-creation, a half-written notebook, or
//! a timeline line from a future version all degrade to "less to show".
use crate::rows::WorkspaceRow;

#[derive(Debug, Clone, PartialEq)]
pub struct ChainEntry {
    pub ts: String,
    pub kind: String,
    pub actor: String,
}

#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub objective: Option<String>,
    /// Tail of the most recently modified notebook, oldest first.
    pub notebook: Vec<String>,
    /// Tail of `timeline.jsonl`, oldest first — the workspace's conversation chain.
    pub chain: Vec<ChainEntry>,
    pub queue: usize,
    pub mail: usize,
}

fn count_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| e.path().is_file()).count())
        .unwrap_or(0)
}

/// The most recently modified `notebook.<actor>.md`.
fn newest_notebook(notebook_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(notebook_dir).ok()?.flatten() {
        let p = e.path();
        let name = p.file_name()?.to_string_lossy().to_string();
        if !(name.starts_with("notebook.") && name.ends_with(".md")) {
            continue;
        }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            if newest.as_ref().map_or(true, |(t, _)| m > *t) {
                newest = Some((m, p));
            }
        }
    }
    newest.map(|(_, p)| p)
}

pub fn gather(row: &WorkspaceRow, max_lines: usize) -> Detail {
    let ws = row.path.join(".ws");

    let objective = std::fs::read_to_string(ws.join("README.md"))
        .ok()
        .and_then(|s| crate::readme::objective_of(&s));

    let notebook = newest_notebook(&ws.join("notebook"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            let lines: Vec<String> = s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
            lines[lines.len().saturating_sub(max_lines)..].to_vec()
        })
        .unwrap_or_default();

    let chain = std::fs::read_to_string(ws.join("timeline.jsonl"))
        .map(|s| {
            let all: Vec<ChainEntry> = s
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .map(|v| ChainEntry {
                    ts: v["ts"].as_str().unwrap_or_default().to_string(),
                    kind: v["kind"].as_str().unwrap_or_default().to_string(),
                    actor: v["actor"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            all[all.len().saturating_sub(max_lines)..].to_vec()
        })
        .unwrap_or_default();

    Detail {
        objective,
        notebook,
        chain,
        // Phase 8 creates these; until then they are absent and count as zero.
        queue: count_files(&ws.join("queue")),
        mail: count_files(&ws.join("mail")),
    }
}
```

Check `readme::objective_of`'s signature first — the plan assumes it takes the README *body* (`&str`) and returns `Option<String>`, per `src/readme.rs:11`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::detail`
Expected: PASS (4 tests).

- [ ] **Step 5: Write the failing theme tests**

Create `src/tui/theme.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn env(no_color: bool, fgbg: Option<&str>, os_dark: Option<bool>) -> ThemeEnv {
        ThemeEnv { no_color, colorfgbg: fgbg.map(String::from), os_dark }
    }

    #[test]
    fn config_override_beats_detection() {
        // OS says light, COLORFGBG says light — an explicit config wins anyway.
        let t = resolve("dark", &env(false, Some("0;15"), Some(false)));
        assert!(t.dark, "config theme = dark must win over detection");
        let t = resolve("light", &env(false, Some("15;0"), Some(true)));
        assert!(!t.dark);
    }

    #[test]
    fn auto_reads_colorfgbg_background_field() {
        // COLORFGBG is "<fg>;<bg>"; a background of 0-6 or 8 means a dark terminal.
        assert!(resolve("auto", &env(false, Some("15;0"), None)).dark);
        assert!(!resolve("auto", &env(false, Some("0;15"), None)).dark);
        // Three-field form ("<fg>;<default>;<bg>") — the background is still last.
        assert!(resolve("auto", &env(false, Some("15;default;0"), None)).dark);
    }

    #[test]
    fn auto_falls_back_to_os_appearance_then_to_dark() {
        assert!(!resolve("auto", &env(false, None, Some(false))).dark);
        assert!(resolve("auto", &env(false, None, Some(true))).dark);
        assert!(resolve("auto", &env(false, None, None)).dark, "unknowable → dark, the common terminal");
        // Garbage COLORFGBG must not win over a known OS appearance.
        assert!(!resolve("auto", &env(false, Some("nonsense"), Some(false))).dark);
    }

    #[test]
    fn no_color_forces_plain() {
        let t = resolve("dark", &env(true, Some("15;0"), Some(true)));
        assert!(t.plain, "NO_COLOR must strip color regardless of theme");
        assert_eq!(t.accent, Color::Reset);
        assert_eq!(t.live, Color::Reset);
    }
}
```

- [ ] **Step 6: Run and watch it fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::theme`
Expected: FAIL — `ThemeEnv`/`resolve` do not exist.

- [ ] **Step 7: Implement `theme.rs`**

```rust
//! Theme resolution. `auto` reads the terminal's own hint (`COLORFGBG`) first,
//! then the OS appearance; an explicit `config theme` always wins. No tmux DCS
//! passthrough (spec §13).
use ratatui::style::Color;

/// The inputs to theme detection, injected so `resolve` stays pure and testable.
#[derive(Debug, Clone, Default)]
pub struct ThemeEnv {
    pub no_color: bool,
    pub colorfgbg: Option<String>,
    /// `Some(true)` = the OS reports a dark appearance; `None` = unknown.
    pub os_dark: Option<bool>,
}

impl ThemeEnv {
    pub fn detect() -> Self {
        ThemeEnv {
            no_color: std::env::var_os("NO_COLOR").is_some(),
            colorfgbg: std::env::var("COLORFGBG").ok(),
            os_dark: macos_dark(),
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_dark() -> Option<bool> {
    // `defaults read -g AppleInterfaceStyle` prints "Dark" in dark mode and
    // exits non-zero (key absent) in light mode.
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .ok()?;
    if !out.status.success() {
        return Some(false);
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "Dark")
}

#[cfg(not(target_os = "macos"))]
fn macos_dark() -> Option<bool> {
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// NO_COLOR: every color resolves to `Color::Reset` so the terminal's own
    /// palette shows through untouched.
    pub plain: bool,
    pub accent: Color,
    pub dim: Color,
    pub live: Color,
    pub warn: Color,
}

/// Parse the background field of `COLORFGBG` ("<fg>;<bg>" or "<fg>;<x>;<bg>").
/// ANSI 0-6 and 8 are the dark backgrounds; 7 and 9-15 are light.
fn fgbg_is_dark(v: &str) -> Option<bool> {
    let bg: u8 = v.rsplit(';').next()?.trim().parse().ok()?;
    Some(matches!(bg, 0..=6 | 8))
}

pub fn resolve(cfg_theme: &str, env: &ThemeEnv) -> Theme {
    let dark = match cfg_theme {
        "dark" => true,
        "light" => false,
        // "auto" and anything unrecognized: the terminal's hint, then the OS,
        // then dark — the overwhelmingly common terminal background.
        _ => env
            .colorfgbg
            .as_deref()
            .and_then(fgbg_is_dark)
            .or(env.os_dark)
            .unwrap_or(true),
    };

    if env.no_color {
        return Theme {
            dark,
            plain: true,
            accent: Color::Reset,
            dim: Color::Reset,
            live: Color::Reset,
            warn: Color::Reset,
        };
    }

    Theme {
        dark,
        plain: false,
        accent: if dark { Color::Cyan } else { Color::Blue },
        dim: if dark { Color::DarkGray } else { Color::Gray },
        live: Color::Green,
        warn: if dark { Color::Yellow } else { Color::Red },
    }
}
```

- [ ] **Step 8: Run the theme tests**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::theme`
Expected: PASS (4 tests).

- [ ] **Step 9: Write the failing render test for the detail pane**

Add to `src/tui/render.rs`'s test module:

```rust
    #[test]
    fn detail_pane_shows_the_selected_workspaces_objective_and_counts() {
        let d = tempfile::TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("notebook")).unwrap();
        std::fs::write(ws.join("README.md"), "## Objective\n\nShip the TUI.\n").unwrap();
        std::fs::write(ws.join("notebook/notebook.me.md"), "found the bug\n").unwrap();

        let mut r = row("alpha", "claude");
        r.path = d.path().to_path_buf();
        let app = crate::tui::app::App::new(vec![r], 0);
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
```

- [ ] **Step 10: Run and watch it fail, then implement the split layout**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::render`
Expected: FAIL — nothing renders the objective.

Give `App` a theme and a detail cache. In `src/tui/app.rs`:

```rust
    pub theme: crate::tui::theme::Theme,
```

initialized in `new` as `crate::tui::theme::resolve("auto", &crate::tui::theme::ThemeEnv::default())` so unit tests get a deterministic theme with no env reads, plus a constructor the real entry point uses:

```rust
    /// Same as `new`, with the theme resolved from config + environment.
    pub fn with_theme(rows: Vec<WorkspaceRow>, now: i64, theme: crate::tui::theme::Theme) -> Self {
        let mut a = App::new(rows, now);
        a.theme = theme;
        a
    }
```

In `src/tui/mod.rs`'s `run()`:

```rust
    let cfg = crate::config::load();
    let theme = theme::resolve(&cfg.theme, &theme::ThemeEnv::detect());
    let mut app = App::with_theme(rows, crate::limits::now_epoch(), theme);
```

In `src/tui/render.rs`, split the screen and add the pane:

```rust
fn render_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(r) = app.selected_row() else {
        f.render_widget(Block::bordered().title("detail"), area);
        return;
    };
    let det = crate::tui::detail::gather(r, 5);
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
    lines.push(Line::raw(det.objective.unwrap_or_else(|| "(no objective yet)".into())));
    lines.push(Line::styled(
        format!("queue {}   mail {}", det.queue, det.mail),
        Style::new().fg(app.theme.dim),
    ));
    if !det.notebook.is_empty() {
        lines.push(Line::styled("notebook", Style::new().fg(app.theme.dim)));
        lines.extend(det.notebook.into_iter().map(Line::raw));
    }
    if !det.chain.is_empty() {
        lines.push(Line::styled("chain", Style::new().fg(app.theme.dim)));
        lines.extend(
            det.chain
                .into_iter()
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
```

with `use ratatui::text::{Line, Span};` and `use ratatui::widgets::Wrap;` added. Also apply the theme to the list: style the live marker with `app.theme.live`, the tags/activity columns with `app.theme.dim`, and a corrupt row's state cell with `app.theme.warn`. Skip styling entirely when `app.theme.plain` — or rely on `Color::Reset`, which the plain theme already supplies.

Earlier tests render at height 12; the detail pane takes 45% of it, so re-run the full render module and adjust any test that no longer has room (raise its height rather than shrinking the pane).

- [ ] **Step 11: Run the tests, then the whole suite**

Run: `. "$HOME/.cargo/env"; cargo test --lib tui::` then `. "$HOME/.cargo/env"; cargo test`
Expected: PASS.

- [ ] **Step 12: Add `theme` to the doctor/config surface check**

`config theme` already exists (`src/config.rs:12`, default `"auto"`). Verify `ws config set theme dark` round-trips and that the TUI honors it:

Run: `. "$HOME/.cargo/env"; cargo run -- config set theme light && cargo run -- config get theme`
Expected: prints `light`. Then reset: `cargo run -- config set theme auto`.

- [ ] **Step 13: Commit**

```bash
git add src/tui/detail.rs src/tui/theme.rs src/tui/render.rs src/tui/mod.rs src/tui/app.rs
git commit -m "feat: TUI detail pane (objective, notebook, chain, queue/mail) + theme resolution"
```

- [ ] **Step 14: Drive the finished TUI by hand and record what you saw**

Run `. "$HOME/.cargo/env"; cargo run -- -tui` against the real registry. Check: the list shows every workspace with its agent and any limit data; `/` filters; `j`/`k` move and the detail pane follows; `t` adds a tag that survives quitting and reopening; `a` archives; `r` asks before removing; `q` restores the terminal cleanly. Then `COLORFGBG=0;15 cargo run -- -tui` and confirm the light theme differs. Put the results in the task report — a TUI that passes its snapshots and looks wrong on a real terminal has still failed.

---

## Self-Review

**1. Spec coverage (§13 + §17.7).**

| Spec requirement | Task |
|---|---|
| Workspace list: name, agent icon, live dot, status text, tags, last activity, limits | 2 (all seven columns in `render_list`) |
| Type-to-filter | 3 |
| Enter = open in current terminal, replacing the TUI | 3 (`Outcome::Launch` → restore → `commands::launch` exec) |
| `a`rchive, `t`ag, `s`tatus, `r`emove (confirm), `q`uit | 4 (`q` lands in 2) |
| Detail pane: README objective, latest notebook entries, queue/mail counts, conversation chain | 5 |
| Agent info first-class: per-workspace agent + per-agent limit state | 2 (agent + limits columns), 5 (detail header) |
| Theme: `auto` = OS appearance + `COLORFGBG`, config override wins, no tmux DCS | 5 |
| `TestBackend` snapshot testing (§16) | 2, 3, 4, 5 |
| Opus's read-path warning (Phase 7 task 1 per the handoff) | 1 |

Deliberately **not** in scope, and why: queue/mail are Phase 8 — task 5 counts the files and shows `queue 0` when the directories do not exist, which is the "show zero/absent gracefully" the handoff asked for. Nerd-font glyphs stay Phase 9 (`LIVE_MARK` is ASCII `*`). No scrolling viewport beyond what `TableState` gives for free; no mouse.

**2. Placeholder scan.** Every step has runnable commands and complete code. The three places I deliberately wrote "check this before coding" rather than asserting a value — the `.ws/local/lock` path (task 1 step 8), `meta::set_archived`'s signature (task 4 step 6), and `readme::objective_of`'s parameter (task 5 step 3) — are verification instructions against files the implementer has open, not gaps: each names the file, the line, and the assumed shape.

**3. Type consistency.** `WorkspaceRow`'s field list is written out identically in the task 1 definition, the task 2/3/4 test fixtures, and the task 5 fixture (11 fields, `live_pid` not `live`). `Mode` and `Action` grow monotonically — task 2 defines `Browse`/`None`+`Quit`, task 3 adds `Filter`/`Launch`, task 4 adds `Input(InputField)`/`Confirm` — and every later `match` covers the earlier variants. `Outcome::Launch(String)` in task 3 matches the `Action::Launch(String)` it is produced from. `rows::workspace_toml()` is defined in task 1 and used in task 4. `theme::resolve(&str, &ThemeEnv)` is called with `cfg.theme` in task 5, matching `Config::theme: String`.

**One risk worth naming up front:** task 1 changes `-list` from "warn and print nothing" to "exit 1" on a corrupt registry. That is the intended contract, but existing integration tests may assert the old behavior. The instruction in task 1 step 14 is to update the assertion, not to soften the code — a reviewer seeing that diff should know it was deliberate.
