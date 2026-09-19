//! "Done" for a feature worktree: a marker the worktree writes about itself, and
//! the read side that lets its base find the ones worth merging.
//!
//! The marker records the commit it was written at and counts only while that
//! commit is still `HEAD` and the tree is clean, so it cannot outlive the work
//! it certified — nothing has to remember to delete it.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::worktree;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub head: String,
    pub at: String,
    /// `prompt` (the user answered a question) or `manual` (`ws -done`).
    pub by: String,
}

// `.ws/local/` is gitignored in every workspace (`contract.rs`), so these files
// are never the user's uncommitted work and need no entry in the dirty check.
fn marker_path(root: &Path) -> PathBuf {
    root.join(".ws/local/done.json")
}
fn declined_path(root: &Path) -> PathBuf {
    root.join(".ws/local/done-declined.json")
}

/// The marker, or `None` when absent *or* unreadable: a hook and a status line
/// both read this, and neither may fail on a damaged file.
pub fn read(root: &Path) -> Option<Marker> {
    serde_json::from_str(&std::fs::read_to_string(marker_path(root)).ok()?).ok()
}

pub fn is_fresh(root: &Path) -> bool {
    let Some(m) = read(root) else { return false };
    matches!(worktree::head_sha(root), Ok(h) if h == m.head)
        && matches!(worktree::is_clean(root), Ok(true))
}

/// Mark the worktree at `root` done at its current `HEAD`.
pub fn mark(root: &Path, by: &str) -> Result<String> {
    if !worktree::is_clean(root)? {
        bail!(
            "{} has uncommitted changes — commit or discard them before marking it done",
            root.display()
        );
    }
    let head = worktree::head_sha(root)?;
    let m = Marker { head: head.clone(), at: crate::now_iso(), by: by.to_string() };
    write_json(&marker_path(root), &m)?;
    Ok(head)
}

pub fn undo(root: &Path) -> Result<()> {
    match std::fs::remove_file(marker_path(root)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("cannot remove the done marker"),
    }
}

/// Remember "the user said no" for this `HEAD`, so the question is asked once
/// per commit rather than on every `/clear`.
pub fn decline(root: &Path) -> Result<()> {
    let m = Marker { head: worktree::head_sha(root)?, at: crate::now_iso(), by: "declined".into() };
    write_json(&declined_path(root), &m)
}

pub fn was_declined(root: &Path, head: &str) -> bool {
    std::fs::read_to_string(declined_path(root))
        .ok()
        .and_then(|s| serde_json::from_str::<Marker>(&s).ok())
        .is_some_and(|m| m.head == head)
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::atomic::atomic_write(path, serde_json::to_string(v)?)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Class {
    /// Fresh marker and nothing stands in the way of merging it.
    Ready,
    /// Fresh marker, but something (a live session, a dirty base…) blocks the merge.
    DoneBlocked(String),
    NotDone,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub feature: String,
    #[allow(dead_code)] // consumed by the CLI in a later task
    pub name: String,
    pub ahead: usize,
    pub class: Class,
}

/// Every `base@*` worktree, classified. The merge rule is `worktree::readiness`,
/// not a copy of it, so this cannot promise a merge the merge then refuses.
pub fn classify(base: &str, own_base_session: bool) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for f in worktree::features_as(base, own_base_session)? {
        let class = if !is_fresh(&f.path) {
            Class::NotDone
        } else if f.readiness.ready() {
            Class::Ready
        } else {
            Class::DoneBlocked(f.readiness.blockers[0].summary())
        };
        rows.push(Row { feature: f.feature, name: f.name, ahead: f.readiness.ahead, class });
    }
    Ok(rows)
}

/// The line `SessionStart` (source `clear`) adds to the context, or `None` when
/// there is nothing to say. Swallows every error: a hook must not fail a session.
#[allow(dead_code)] // consumed by the CLI/hook in a later task
pub fn clear_note(ws_name: &str, ws_root: &Path) -> Option<String> {
    if let Some(spec) = worktree::parse_name(ws_name) {
        // A feature worktree: offer to mark it done, once per commit.
        let head = worktree::head_sha(ws_root).ok()?;
        if is_fresh(ws_root) || was_declined(ws_root, &head) || !worktree::is_clean(ws_root).ok()? {
            return None;
        }
        let me = worktree::features(&spec.base).ok()?.into_iter().find(|f| f.name == ws_name)?;
        if me.readiness.ahead == 0 {
            return None;
        }
        return Some(format!(
            "ws: {ws_name} has {} commit(s) that {} does not. Ask the user once whether \
             to mark this feature done so {} can offer it for merge. If yes run `ws -done`; \
             if no run `ws -done --declined`. Do nothing else about it.",
            me.readiness.ahead, spec.base, spec.base
        ));
    }
    // A base workspace: report the worktrees that have marked themselves done.
    let rows = classify(ws_name, true).ok()?;
    let done: Vec<&Row> = rows.iter().filter(|r| r.class != Class::NotDone).collect();
    if done.is_empty() {
        return None;
    }
    let list = done
        .iter()
        .map(|r| match &r.class {
            Class::DoneBlocked(why) => format!("{} (blocked: {why})", r.feature),
            _ => format!("{} ({} commit(s))", r.feature, r.ahead),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "ws: worktrees of {ws_name} marked done: {list}. Ask the user whether to review \
         them now. If yes run `ws {ws_name} -done`, show its report, and merge each one the \
         user approves with `ws {ws_name}@<feature> --merge --from-session`."
    ))
}

/// How many `base@*` worktrees have a fresh marker, for the status line. Cached
/// for `CHIP_TTL_SECS`: the bar repaints once a second and each answer runs git.
const CHIP_TTL_SECS: u64 = 20;

#[allow(dead_code)] // consumed by the CLI/hook in a later task
pub fn chip_count(ws_name: &str, ws_root: &Path) -> usize {
    if worktree::parse_name(ws_name).is_some() {
        return 0; // worktrees show nothing; the chip belongs to the base
    }
    let cache = ws_root.join(".ws/local/done-chip.json");
    let now = crate::limits::now_epoch().max(0) as u64;
    if let Some((at, n)) = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|s| serde_json::from_str::<(u64, usize)>(&s).ok())
    {
        if now.saturating_sub(at) < CHIP_TTL_SECS {
            return n;
        }
    }
    let n = classify(ws_name, true)
        .map(|rows| rows.iter().filter(|r| r.class != Class::NotDone).count())
        .unwrap_or(0);
    let _ = write_json(&cache, &(now, n));
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repo with one commit and a `.ws/local/` (gitignored, as in a real workspace).
    fn repo(td: &TempDir) -> std::path::PathBuf {
        let d = td.path().join("wt");
        std::fs::create_dir_all(d.join(".ws/local")).unwrap();
        git(&d, &["init", "-q"]);
        git(&d, &["config", "user.email", "d@e.x"]);
        git(&d, &["config", "user.name", "D"]);
        std::fs::write(d.join(".gitignore"), ".ws/local/\n").unwrap();
        std::fs::write(d.join("a.txt"), "a").unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", "init"]);
        d
    }

    #[test]
    fn a_marker_is_fresh_until_head_moves() {
        let td = TempDir::new().unwrap();
        let d = repo(&td);
        assert!(!is_fresh(&d), "no marker, not done");
        mark(&d, "manual").unwrap();
        assert!(is_fresh(&d));
        std::fs::write(d.join("b.txt"), "b").unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", "more"]);
        assert!(!is_fresh(&d), "a new commit invalidates the marker without deleting it");
        assert!(read(&d).is_some(), "the stale marker is still readable");
    }

    #[test]
    fn a_marker_is_stale_while_the_tree_is_dirty_and_marking_a_dirty_tree_refuses() {
        let td = TempDir::new().unwrap();
        let d = repo(&td);
        mark(&d, "manual").unwrap();
        std::fs::write(d.join("dirty.txt"), "x").unwrap();
        assert!(!is_fresh(&d));
        assert!(mark(&d, "manual").is_err(), "certifying dirty work as done is a lie");
    }

    #[test]
    fn a_garbled_marker_reads_as_no_marker() {
        let td = TempDir::new().unwrap();
        let d = repo(&td);
        std::fs::write(d.join(".ws/local/done.json"), "{not json").unwrap();
        assert!(read(&d).is_none());
        assert!(!is_fresh(&d));
    }

    #[test]
    fn a_decline_is_remembered_for_that_head_only() {
        let td = TempDir::new().unwrap();
        let d = repo(&td);
        let head = crate::worktree::head_sha(&d).unwrap();
        assert!(!was_declined(&d, &head));
        decline(&d).unwrap();
        assert!(was_declined(&d, &head));
        assert!(!was_declined(&d, "0000000000000000000000000000000000000000"));
    }

    #[test]
    fn undo_removes_the_marker_and_is_quiet_when_there_is_none() {
        let td = TempDir::new().unwrap();
        let d = repo(&td);
        undo(&d).unwrap();
        mark(&d, "manual").unwrap();
        undo(&d).unwrap();
        assert!(read(&d).is_none());
    }
}
