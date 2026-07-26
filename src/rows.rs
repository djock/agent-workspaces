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
    /// Recorded per-workspace tab color. Kept rather than dropped: it is the
    /// natural input for spec §14's per-workspace tab color, and `term::set_tab`
    /// (`commands::launch`) already consumes exactly this value — it just reads
    /// it from `meta` directly today rather than off the row.
    #[allow(dead_code)]
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
                    .map_err(std::io::Error::other)
            })
        {
            if newest.is_none_or(|n| secs > n) {
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

/// Every registered workspace as a typed row, filtered per `opts`.
/// Errors only when the registry itself cannot be read — a single broken
/// workspace is a `RowState::Corrupt` row, not a failed listing.
pub fn list_workspaces(opts: &ListOpts) -> Result<Vec<WorkspaceRow>> {
    Ok(list_all(opts)?.rows)
}

/// As `list_workspaces`, keeping the pre-filter count.
pub fn list_all(opts: &ListOpts) -> Result<Listing> {
    let cfg = crate::config::load();
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
