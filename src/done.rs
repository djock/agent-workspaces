//! "Done" for a feature worktree: a marker the worktree writes about itself, and
//! the read side that lets its base find the ones worth merging.
//!
//! The marker records the commit it was written at and counts only while that
//! commit is still `HEAD` and the tree is clean, so it cannot outlive the work
//! it certified — nothing has to remember to delete it.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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
    /// The feature worktree's checkout path.
    pub path: PathBuf,
    pub ahead: usize,
    pub class: Class,
}

/// Every `base@*` worktree, classified. The merge rule is `worktree::readiness_as`,
/// not a copy of it, so this cannot promise a merge the merge then refuses.
pub fn classify(base: &str, own_base_session: bool) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for f in worktree::features_as(base, own_base_session)? {
        let class = if !is_fresh(&f.path) {
            Class::NotDone
        } else {
            match f.readiness.blockers.first() {
                None => Class::Ready,
                Some(b) => Class::DoneBlocked(b.summary()),
            }
        };
        rows.push(Row { feature: f.feature, path: f.path, ahead: f.readiness.ahead, class });
    }
    Ok(rows)
}

/// The line `SessionStart` (source `clear`) adds to the context, or `None` when
/// there is nothing to say. Swallows every error: a hook must not fail a session.
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

/// What the Stop hook asks at the end of a turn, or `None` when there is
/// nothing new worth an extra turn. Swallows every error: a hook must not fail
/// a turn.
///
/// `asked` holds every question already put to the user, as `feature:head` pairs
/// for a base (a bare `head` for a worktree). A question is new only when the
/// current ready set contains a pair not in it, so a set that shrinks (one was
/// merged) or flaps (a lock came and went) is silent, while a new commit is a new
/// sha and asks again. The returned set is what to store next: `asked` plus the
/// current pairs, pruned to worktrees that still exist so the file cannot grow
/// for ever.
///
/// This runs at the end of every turn, so the cheap checks come first and git
/// runs only once something could be asked.
pub fn stop_prompt(
    ws_name: &str,
    ws_root: &Path,
    asked: &BTreeSet<String>,
) -> Option<(BTreeSet<String>, String)> {
    if let Some(spec) = worktree::parse_name(ws_name) {
        // A feature worktree: one question per commit, and only for finished-looking
        // work (clean, not already marked, not already declined, something to merge).
        let head = worktree::head_sha(ws_root).ok()?;
        if asked.contains(&head) || was_declined(ws_root, &head) || is_fresh(ws_root) {
            return None;
        }
        if !worktree::is_clean(ws_root).ok()? {
            return None;
        }
        // One rev-list, not the readiness of every sibling to find our own `ahead`.
        let base_path = crate::registry::lookup_checked(&spec.base).ok()??;
        let ahead: usize = crate::git::ok(
            &base_path,
            &["rev-list", "--count", &format!("HEAD..{}", spec.feature)],
        )
        .ok()?
        .trim()
        .parse()
        .ok()?;
        if ahead == 0 {
            return None;
        }
        let directive = format!(
            "ws: {ws_name} has {ahead} commit(s) that {} does not. Ask the user once whether \
             to mark this feature done so {} can offer it for merge. If yes run `ws -done`; \
             if no run `ws -done --declined`. Do not do anything else about it, and do not \
             start new work on your own.",
            spec.base, spec.base
        );
        let mut next = asked.clone();
        next.insert(head);
        return Some((next, directive));
    }
    // A base. Nothing can be ready unless some sibling has written a marker, and
    // that is a file check: skip every git call when none has.
    let prefix = format!("{ws_name}@");
    let siblings: Vec<(String, PathBuf)> = crate::registry::all_checked()
        .ok()?
        .into_iter()
        .filter(|(n, _)| n.strip_prefix(&prefix).is_some_and(|f| !f.is_empty()))
        .collect();
    if !siblings.iter().any(|(_, p)| marker_path(p).is_file()) {
        return None;
    }
    // Only worktrees that could actually be merged are worth a turn; a set that is
    // all blocked is something the user cannot act on from here.
    let rows = classify(ws_name, true).ok()?;
    let ready: Vec<&Row> = rows.iter().filter(|r| r.class == Class::Ready).collect();
    let mut current = BTreeSet::new();
    for r in &ready {
        current.insert(format!("{}:{}", r.feature, worktree::head_sha(&r.path).ok()?));
    }
    if current.is_empty() || current.is_subset(asked) {
        return None;
    }
    let mut next: BTreeSet<String> = asked.union(&current).cloned().collect();
    next.retain(|pair| {
        pair.rsplit_once(':').is_some_and(|(f, _)| rows.iter().any(|r| r.feature == f))
    });
    let list =
        ready.iter().map(|r| format!("{} ({} commit(s))", r.feature, r.ahead)).collect::<Vec<_>>();
    let blocked = rows
        .iter()
        .filter(|r| matches!(r.class, Class::DoneBlocked(_)))
        .map(|r| r.feature.as_str())
        .collect::<Vec<_>>();
    let also = if blocked.is_empty() {
        String::new()
    } else {
        format!(" Also marked done but still open or blocked: {}.", blocked.join(", "))
    };
    let directive = format!(
        "ws: worktrees of {ws_name} are marked done and ready to merge: {}.{also} Ask the \
         user whether to review them now. Do NOT merge anything unless they say yes. If yes \
         run `ws {ws_name} -done`, show its report, and merge each one the user approves \
         with `ws {ws_name}@<feature> --merge --from-session`. If they decline, drop the \
         subject; this will not ask again until the set of ready worktrees changes.",
        list.join(", ")
    );
    Some((next, directive))
}

/// How many `base@*` worktrees have a fresh marker, for the status line. Cached
/// for `CHIP_TTL_SECS`: the bar repaints once a second and each answer runs git.
const CHIP_TTL_SECS: u64 = 20;

pub fn chip_count(ws_name: &str, ws_root: &Path) -> usize {
    if worktree::parse_name(ws_name).is_some() {
        return 0; // worktrees show nothing; the chip belongs to the base
    }
    let cache = ws_root.join(".ws/local/done-chip.json");
    let now = crate::limits::now_epoch().max(0) as u64;
    let cached = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|s| serde_json::from_str::<(u64, usize)>(&s).ok());
    if let Some(n) = cache_hit(cached, now) {
        return n;
    }
    // A failed classify is not "0 done": answer 0 for now but cache nothing, so
    // the next repaint tries again.
    match classify(ws_name, true) {
        Ok(rows) => {
            let n = rows.iter().filter(|r| r.class != Class::NotDone).count();
            let _ = write_json(&cache, &(now, n));
            n
        }
        Err(_) => 0,
    }
}

/// A cached chip count is usable only when it is younger than the TTL and not
/// stamped in the future (a clock step back must not pin a stale answer).
fn cache_hit(cached: Option<(u64, usize)>, now: u64) -> Option<usize> {
    let (at, n) = cached?;
    (at <= now && now - at < CHIP_TTL_SECS).then_some(n)
}

/// The approval a user gave was for the commit they were shown. It holds only
/// while that commit is still the worktree's `HEAD` and the marker is still fresh.
pub fn still_the_reviewed_commit(shown: &str, now: &str, fresh: bool) -> bool {
    fresh && shown == now
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
    fn an_approval_holds_only_for_the_commit_that_was_shown() {
        assert!(still_the_reviewed_commit("abc", "abc", true));
        assert!(!still_the_reviewed_commit("abc", "def", true), "HEAD moved");
        assert!(!still_the_reviewed_commit("abc", "abc", false), "marker went stale");
    }

    #[test]
    fn the_chip_cache_expires_and_ignores_a_future_stamp() {
        assert_eq!(cache_hit(Some((100, 3)), 105), Some(3));
        assert_eq!(cache_hit(Some((100, 3)), 100 + CHIP_TTL_SECS), None, "expired");
        assert_eq!(cache_hit(Some((200, 3)), 100), None, "a future stamp is stale");
        assert_eq!(cache_hit(None, 100), None);
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
