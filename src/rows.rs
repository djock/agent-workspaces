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
    /// What the agent running here says it is doing, when one is running and
    /// publishes it. `None` covers three different things — nothing running,
    /// an agent that publishes no record, and a record ws would not trust —
    /// which the display treats identically because it has nothing to add.
    pub agent_state: Option<crate::agentstate::AgentState>,
    pub archived: bool,
    pub tags: Vec<String>,
    pub status: Option<String>,
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

/// Where `launch` records that a workspace was opened. The **mtime** is what
/// counts; the ISO timestamp inside is there so a human reading the file gets
/// an answer too.
pub fn opened_stamp(ws_dir: &Path) -> PathBuf {
    ws_dir.join("local/last-opened")
}

/// Record "opened now". Best-effort: a launch must not fail over the file that
/// only decides list order, and a workspace on a read-only checkout still opens.
pub fn stamp_opened(ws_dir: &Path) {
    let path = opened_stamp(ws_dir);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("{}\n", crate::time::now_iso()));
}

/// Newest mtime (epoch seconds) among the workspace documents worth calling
/// "activity". `.ws/local/` is excluded on purpose: the bash audit log and the
/// statusline's limits.json are written constantly and would make every
/// workspace look equally fresh.
///
/// `local/last-opened` is the one exception, and it is why this answers "last
/// used" rather than only "last written to". Opening a workspace is using it,
/// and a session where the agent never appended to a notebook used to leave no
/// trace at all — so the workspace you were in ten minutes ago could sort below
/// one you last touched in June.
fn last_activity(ws_dir: &Path) -> Option<i64> {
    let mut newest: Option<i64> = None;
    let mut consider = |p: PathBuf| {
        if let Ok(secs) = std::fs::metadata(&p).and_then(|m| m.modified()).and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .map_err(std::io::Error::other)
        }) {
            if newest.is_none_or(|n| secs > n) {
                newest = Some(secs);
            }
        }
    };
    consider(ws_dir.join("README.md"));
    consider(ws_dir.join("timeline.jsonl"));
    consider(opened_stamp(ws_dir));
    for sub in ["notebook", "handoffs"] {
        if let Ok(rd) = std::fs::read_dir(ws_dir.join(sub)) {
            for e in rd.flatten() {
                consider(e.path());
            }
        }
    }
    newest
}

/// The rows matching a `ListOpts`, plus how many were registered *before*
/// filtering.
///
/// I8: `commands::list` needs the unfiltered count to tell "you have no
/// workspaces at all" from "none matched this filter". Without it, plain
/// `ws -list` on an empty registry printed "no active workspaces (try:
/// ws -list --archived)" — sending the user to look for archived workspaces
/// that cannot exist.
#[derive(Debug)]
pub struct Listing {
    pub rows: Vec<WorkspaceRow>,
    /// Registered workspaces, ignoring `opts` entirely.
    pub total: usize,
}

/// As `list_workspaces`, keeping the pre-filter count.
pub fn list_all(opts: &ListOpts) -> Result<Listing> {
    let cfg = crate::config::load();
    // Read once for the whole listing, not once per row: this is two passes
    // however many agents are running, and the picker repaints on every key.
    let states = crate::agentstate::by_directory();
    let mut out = Vec::new();
    let mut total = 0usize;
    for (name, path) in crate::registry::all_checked()? {
        total += 1;
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
            agent_state: crate::agentstate::for_directory(&states, &path),
            archived: meta.archived,
            tags: meta.tags.clone(),
            status: meta.status.clone(),
            last_activity: last_activity(&ws_dir),
            limits: limits::read(&ws_dir.join("local/limits.json")),
            name,
            path,
            state,
        });
    }
    // Most recently used first — the registry is a BTreeMap, so without this
    // both `-list` and the picker were ordered alphabetically, which puts the
    // workspace you were in a minute ago wherever its name happens to fall.
    // `None` (never touched, or a missing/unreadable `.ws/`) sorts last because
    // `Option::cmp` ranks it below every `Some`, and the name breaks ties so the
    // order is stable between runs.
    out.sort_by(|a, b| b.last_activity.cmp(&a.last_activity).then_with(|| a.name.cmp(&b.name)));
    Ok(Listing { rows: out, total })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // list_all() resolves the registry through the process-global
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

        let rows = list_all(&ListOpts::default()).unwrap();
        let r = rows.rows.iter().find(|r| r.name == "alpha").expect("alpha listed");
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

        let rows = list_all(&ListOpts::default()).unwrap();
        let r = rows.rows.iter().find(|r| r.name == "broken").expect("broken still listed");
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

        let rows = list_all(&ListOpts::default()).unwrap();
        let r = rows.rows.iter().find(|r| r.name == "ghost").unwrap();
        assert_eq!(r.state, RowState::Missing);
    }

    #[test]
    fn archived_are_hidden_unless_requested_and_tag_filters() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        make_ws(d.path(), "live-one", "tags = [\"keep\"]\n");
        make_ws(d.path(), "old-one", "archived = true\ntags = [\"keep\"]\n");

        let default = list_all(&ListOpts::default()).unwrap();
        assert!(default.rows.iter().any(|r| r.name == "live-one"));
        assert!(!default.rows.iter().any(|r| r.name == "old-one"), "archived hidden by default");

        let with_archived = list_all(&ListOpts { tag: None, include_archived: true }).unwrap();
        assert!(with_archived.rows.iter().any(|r| r.name == "old-one"));

        let tagged =
            list_all(&ListOpts { tag: Some("nope".into()), include_archived: true }).unwrap();
        assert!(tagged.rows.is_empty(), "tag filter excludes everything untagged");
    }

    #[test]
    fn a_corrupt_registry_is_an_error_not_an_empty_list() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        let rp = crate::registry::registry_path();
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, "not toml {{{").unwrap();

        assert!(list_all(&ListOpts::default()).is_err());
    }

    /// Give a workspace one activity source, aged `secs_ago`, so ordering is
    /// assertable without waiting on the wall clock.
    fn backdate(root: &std::path::Path, secs_ago: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        let path = root.join(".ws/README.md");
        std::fs::write(&path, "# ws\n").unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn rows_come_back_most_recently_used_first() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        // Named so alphabetical order is the exact reverse of recency: if the
        // sort silently went away, this test would still pass without this.
        let a = make_ws(d.path(), "aaa-stale", "");
        let z = make_ws(d.path(), "zzz-fresh", "");
        backdate(&a, 60 * 60 * 24 * 30);
        backdate(&z, 60);

        let rows = list_all(&ListOpts::default()).unwrap().rows;
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["zzz-fresh", "aaa-stale"], "most recently used first");
    }

    #[test]
    fn opening_a_workspace_lifts_it_to_the_top_without_writing_a_notebook() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        let old = make_ws(d.path(), "opened-just-now", "");
        let recent = make_ws(d.path(), "written-yesterday", "");
        backdate(&old, 60 * 60 * 24 * 30);
        backdate(&recent, 60 * 60 * 24);

        // The launch stamp is the only thing that changes — no document is
        // touched, which is exactly the session that used to leave no trace.
        stamp_opened(&old.join(".ws"));

        let rows = list_all(&ListOpts::default()).unwrap().rows;
        assert_eq!(rows[0].name, "opened-just-now", "opening a workspace counts as using it");
    }

    #[test]
    fn a_workspace_with_no_activity_at_all_sorts_last() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        iso(&d);
        let touched = make_ws(d.path(), "aaa-touched", "");
        backdate(&touched, 60 * 60 * 24 * 365);
        // Registered, no `.ws/` on disk: nothing to take an mtime from.
        crate::registry::register("zzz-ghost", &d.path().join("zzz-ghost")).unwrap();

        let rows = list_all(&ListOpts::default()).unwrap().rows;
        assert_eq!(rows.last().unwrap().name, "zzz-ghost", "unknown age sorts below any known age");
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
