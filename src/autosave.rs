//! Crash recovery: a snapshot of the working tree that costs nothing to keep.
//!
//! An agent session edits files for an hour and then the terminal dies, the
//! laptop sleeps badly, or the process is killed. Everything uncommitted goes
//! with it, and `git stash` cannot help because nobody ran it.
//!
//! Each snapshot is an ordinary git commit written **beside** the branch — a
//! tree built in a private index, committed with `commit-tree`, and pointed at
//! by `refs/ws/session/<conversation>`. Nothing touches the user's index, HEAD,
//! stash, or working tree, so a snapshot can be taken mid-turn without the
//! agent (or the user, in another terminal) ever noticing. Recovery is
//! `git checkout <ref> -- <path>`, and the snapshots are invisible to
//! `git log`/`git branch` because they live outside `refs/heads`.
//!
//! Two rules keep this honest, both learned from cs:
//!
//! * **One ref per conversation.** A single shared ref meant two sessions on one
//!   checkout read, wrote and deleted each other's snapshots — and a worktree
//!   shares its parent's ref namespace, so this is the ordinary case, not an
//!   exotic one.
//! * **A restore is offered only when HEAD has not moved.** Each snapshot
//!   records the commit it sat on. If HEAD has since moved, a whole-tree restore
//!   would write an hour-old tree over committed work, so the notice points at
//!   per-file inspection instead.
use anyhow::{Context, Result};
use std::path::Path;

/// Where a conversation's snapshot lives.
///
/// Under `refs/ws/` rather than `refs/heads/` so it is not a branch: it does not
/// appear in `git branch`, is not pushed by a default `git push`, and cannot be
/// checked out by accident.
pub fn ref_name(conversation: &str) -> String {
    format!("refs/ws/session/{}", slug(conversation))
}

/// A conversation id reduced to what may appear in a ref name.
///
/// git's own rules reject a great deal (`..`, `~`, `^`, `:`, control bytes,
/// a trailing `.lock`), and a conversation id comes from the agent rather than
/// from ws. Anything outside the safe set becomes `-`, which cannot collide two
/// live conversations in practice — ids are UUIDs — and cannot produce a ref
/// name git will refuse.
fn slug(conversation: &str) -> String {
    let s: String = conversation
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if s.is_empty() {
        "unknown".to_string()
    } else {
        s
    }
}

/// The commit HEAD points at, or `None` in a repo with no commits yet.
fn head(root: &Path) -> Option<String> {
    crate::git::maybe(root, &["rev-parse", "HEAD"])
}

fn is_repo(root: &Path) -> bool {
    // `--is-inside-work-tree` rather than a `.git` existence test: in a linked
    // worktree `.git` is a *file* holding a gitdir pointer, and probing for a
    // directory is what made every `base@feature` workspace look like no repo at
    // all elsewhere in this codebase.
    crate::git::maybe(root, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

/// The trailer naming the commit a snapshot was taken on top of.
const BASE_TRAILER: &str = "ws-base:";
/// The trailer naming the process that took it.
const PID_TRAILER: &str = "ws-pid:";
/// The trailer holding that process's start time, as `ps` prints it.
///
/// A pid alone does not identify a process: the kernel reuses pids within hours
/// on a busy machine, and a snapshot ref outlives the session that wrote it by
/// design. Without this, a crashed session whose pid has since been handed to
/// something else looks *live* — so its recovery notice is never shown and `gc`
/// never reclaims it. `agentstate` learned this for session records and wrote it
/// down; this is the same rule for the same reason, and now the same code.
const START_TRAILER: &str = "ws-start:";

/// Take a snapshot of the working tree for `conversation`.
///
/// Returns the new commit, or `None` when there was nothing to do: outside a git
/// repo, or when the tree is byte-identical to the previous snapshot (the
/// common case on a turn that only read files — writing a commit per turn
/// regardless would grow the object store for no recoverable difference).
///
/// Best-effort by contract. This runs from a hook on every turn boundary, and a
/// failure to snapshot must never fail the turn: callers ignore the error, and
/// the reasons it can fail (a concurrent index lock, a full disk) are all
/// transient enough that the next turn tries again.
pub fn snapshot(root: &Path, conversation: &str) -> Result<Option<String>> {
    if !is_repo(root) {
        return Ok(None);
    }
    let refname = ref_name(conversation);
    let previous = crate::git::maybe(root, &["rev-parse", "--verify", "--quiet", &refname]);

    // A private index, so the user's staged/unstaged split is untouched. Without
    // this, `git add -A` here would stage the user's whole tree behind their
    // back — the snapshot would be correct and their next `git commit` would
    // include everything they had deliberately left out.
    //
    // Asked of git rather than assumed to be `<root>/.git`: in a linked worktree
    // that path is a *file* holding a gitdir pointer, so writing an index beside
    // it fails — and every `base@feature` workspace is a linked worktree, which
    // would have made this whole feature a silent no-op in exactly the
    // workspaces most likely to hold unmerged work.
    //
    // Kept between turns, and named for the conversation rather than the
    // process. A fresh index has no stat cache, so `add -A` re-hashes every file
    // in the repository: measured at 0.9s per turn on a 12k-file tree against
    // 0.04s for a warm `git status`, flat across runs because the old code
    // deleted the index each time. Hooks are killed at 10 seconds, so on a large
    // enough repository that cost is not slow, it is a snapshot that never
    // happens. `discard` and `gc` remove the file with the ref it belongs to.
    let index = index_path(root, conversation).context("cannot locate the git directory")?;
    let idx = index.to_string_lossy().to_string();
    // `add -A` respects .gitignore, so build artifacts and `.ws/local/` stay out
    // — the snapshot holds what the user would lose, not what they can rebuild.
    git_with_index(root, &idx, &["add", "-A"])?;
    let tree = git_with_index(root, &idx, &["write-tree"])?.trim().to_string();

    if let Some(prev) = &previous {
        // Same tree as last time: nothing has changed on disk, so a new commit
        // would record only the passage of time.
        if crate::git::maybe(root, &["rev-parse", &format!("{prev}^{{tree}}")]).as_deref()
            == Some(tree.as_str())
        {
            return Ok(None);
        }
    }

    let base = head(root);
    let message = format!(
        "ws autosave\n\n\
         Working tree of a live ws session, saved outside the branch.\n\
         Restore one file with: git checkout {refname} -- <path>\n\n\
         {BASE_TRAILER} {}\n\
         {PID_TRAILER} {}\n\
         {START_TRAILER} {}\n",
        base.clone().unwrap_or_else(|| "none".into()),
        std::process::id(),
        crate::agentstate::start_time(std::process::id()).unwrap_or_else(|| "none".into()),
    );

    let mut args: Vec<String> = vec!["commit-tree".into(), tree, "-m".into(), message];
    // Chained to the previous snapshot so a session's history is walkable and
    // git's own gc keeps the whole chain reachable from one ref.
    if let Some(prev) = &previous {
        args.push("-p".into());
        args.push(prev.clone());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let commit = crate::git::ok(root, &arg_refs)?.trim().to_string();

    // Compare-and-swap against what we read, so a concurrent snapshot for the
    // same conversation cannot be silently clobbered.
    let mut update: Vec<&str> = vec!["update-ref", &refname, &commit];
    if let Some(prev) = &previous {
        update.push(prev);
    }
    crate::git::ok(root, &update)?;
    Ok(Some(commit))
}

/// Where a conversation's private index lives, inside the git directory.
///
/// Asked of git rather than assumed to be `<root>/.git`: in a linked worktree
/// that path is a *file* holding a gitdir pointer, so writing an index beside it
/// fails — and every `base@feature` workspace is a linked worktree.
fn index_path(root: &Path, conversation: &str) -> Option<std::path::PathBuf> {
    let git_dir = crate::git::maybe(root, &["rev-parse", "--absolute-git-dir"])?;
    Some(Path::new(&git_dir).join(format!("ws-autosave-index.{}", slug(conversation))))
}

/// Remove the index belonging to a snapshot ref, given the ref name.
fn remove_index_for_ref(root: &Path, refname: &str) {
    let Some(slug) = refname.strip_prefix("refs/ws/session/") else { return };
    if let Some(p) = index_path(root, slug) {
        let _ = std::fs::remove_file(p);
    }
}

fn git_with_index(root: &Path, index: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_INDEX_FILE", index)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!("git {} failed: {}", args.join(" "), crate::git::combined(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// One snapshot ref found in the repository.
#[derive(Debug, PartialEq)]
pub struct Snapshot {
    pub refname: String,
    pub commit: String,
    /// The commit HEAD was on when the snapshot was taken.
    pub base: Option<String>,
    pub pid: Option<u32>,
    /// The start time of the process named by `pid`, as `ps` printed it when the
    /// snapshot was written. Compared as text against `ps` today.
    pub start: Option<String>,
    /// Human-readable, for the notice.
    pub when: String,
    /// The same instant as seconds since the epoch, for `gc`. Taken from git
    /// rather than computed, so a snapshot written by another machine with a
    /// different clock is still compared against its own recorded time.
    pub when_unix: i64,
}

/// Every snapshot ref in this repository.
pub fn all(root: &Path) -> Vec<Snapshot> {
    let Some(out) = crate::git::maybe(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)%09%(committerdate:iso-strict)%09%(committerdate:unix)",
            "refs/ws/session",
        ],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let refname = f.next()?.to_string();
            let commit = f.next()?.to_string();
            let when = f.next().unwrap_or("").to_string();
            let when_unix = f.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let body =
                crate::git::maybe(root, &["log", "-1", "--format=%B", &commit]).unwrap_or_default();
            let trailer = |key: &str| {
                body.lines()
                    .find_map(|l| l.trim().strip_prefix(key))
                    .map(|v| v.trim().to_string())
                    .filter(|v| v != "none")
            };
            Some(Snapshot {
                base: trailer(BASE_TRAILER),
                pid: trailer(PID_TRAILER).and_then(|p| p.parse().ok()),
                start: trailer(START_TRAILER),
                refname,
                commit,
                when,
                when_unix,
            })
        })
        .collect()
}

/// Snapshots left behind by a session that is no longer running.
///
/// A snapshot whose recorded process is still running belongs to a *live*
/// session — a second terminal, or a linked worktree sharing this ref namespace
/// — and is never reported as a crash. `conversation` is this session's own id
/// where one is known, excluded so a running session never reports itself; at
/// launch time the agent has not assigned one yet, and `None` leaves the
/// liveness check to do the whole job, which is what it is for.
///
/// "Still running" means both halves: the pid is alive *and* the process still
/// reports the start time the snapshot recorded. A pid alone was the first
/// version and it fails in the direction that costs work — a reused pid makes a
/// crashed session look live, so its notice is never shown and `gc` never
/// reclaims the ref. A snapshot with no recorded start time (written by an older
/// ws) falls back to liveness alone rather than being called dead.
pub fn orphans(root: &Path, conversation: Option<&str>) -> Vec<Snapshot> {
    let mine = conversation.map(ref_name);
    let candidates: Vec<Snapshot> =
        all(root).into_iter().filter(|s| Some(&s.refname) != mine.as_ref()).collect();

    // One `ps` for every pid at once, like `agentstate::by_directory`: asking per
    // snapshot would scale a launch-time check with the number of crashed
    // sessions.
    let pids: Vec<u32> = candidates.iter().filter_map(|s| s.pid).collect();
    let starts = crate::agentstate::process_starts(&pids);

    candidates
        .into_iter()
        .filter(|s| {
            let Some(pid) = s.pid else { return true };
            let Some(actual) = starts.get(&pid) else { return true };
            match &s.start {
                Some(recorded) => recorded != actual,
                None => false,
            }
        })
        .collect()
}

/// What to tell the user at launch about a crashed session's snapshot.
///
/// Deliberately a notice and a command rather than an automatic restore: this
/// runs before an agent session starts, and overwriting the working tree without
/// being asked is precisely the failure mode a crash-recovery feature must not
/// have.
pub fn recovery_notice(root: &Path, conversation: Option<&str>) -> Option<String> {
    let orphans = orphans(root, conversation);
    if orphans.is_empty() {
        return None;
    }
    let head_now = head(root);
    let mut out = String::from("A previous session ended without closing cleanly.\n");
    for s in &orphans {
        out.push_str(&format!("  saved {} at {}\n", s.when, s.refname));
        match (&s.base, &head_now) {
            // The tree it was taken from is still the tree you are on, so the
            // whole-tree restore is safe to offer.
            (Some(base), Some(now)) if base == now => {
                out.push_str(&format!(
                    "    inspect:  git diff {}\n    restore:  git checkout {} -- .\n",
                    s.refname, s.refname
                ));
            }
            // HEAD moved: a whole-tree restore would put an old tree over work
            // that has since been committed or rebased.
            _ => {
                out.push_str(&format!(
                    "    HEAD has moved since it was saved — restoring everything would \
                     overwrite committed work.\n    inspect:  git diff {} -- <path>\n    \
                     restore one file: git checkout {} -- <path>\n",
                    s.refname, s.refname
                ));
            }
        }
    }
    out.push_str("  discard:  git update-ref -d <ref>\n");
    Some(out)
}

/// Drop this conversation's snapshot. Called when a session ends cleanly: what
/// is left behind is what `orphans` reports as a crash, so a clean exit must
/// leave nothing.
pub fn discard(root: &Path, conversation: &str) {
    let refname = ref_name(conversation);
    let _ = crate::git::maybe(root, &["update-ref", "-d", &refname]);
    // The index is kept between turns for the stat cache, so it outlives the
    // snapshot unless it is removed with it.
    remove_index_for_ref(root, &refname);
}

/// Delete snapshots older than `max_age_days` that no live process owns.
///
/// Without this a machine accumulates one ref per crashed conversation forever,
/// and every one of them keeps a whole tree of objects reachable.
pub fn gc(root: &Path, conversation: Option<&str>, max_age_days: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for s in orphans(root, conversation) {
        // A snapshot with no readable timestamp is left alone rather than
        // deleted: the whole point of these refs is to hold work nobody else
        // has, so an unreadable one is the last thing to guess about.
        if s.when_unix == 0 {
            continue;
        }
        if (now - s.when_unix) > max_age_days * 86_400 {
            let _ = crate::git::maybe(root, &["update-ref", "-d", &s.refname]);
            remove_index_for_ref(root, &s.refname);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> TempDir {
        let d = TempDir::new().unwrap();
        let p = d.path();
        crate::git::ok(p, &["init", "-q"]).unwrap();
        crate::git::ok(p, &["config", "user.email", "dev@example.com"]).unwrap();
        crate::git::ok(p, &["config", "user.name", "Dev"]).unwrap();
        std::fs::write(p.join("tracked.txt"), "committed\n").unwrap();
        crate::git::ok(p, &["add", "."]).unwrap();
        crate::git::ok(p, &["commit", "-q", "-m", "first"]).unwrap();
        d
    }

    fn show(root: &Path, refname: &str, path: &str) -> Option<String> {
        crate::git::maybe(root, &["show", &format!("{refname}:{path}")])
    }

    #[test]
    fn a_snapshot_captures_uncommitted_and_untracked_work() {
        let d = repo();
        std::fs::write(d.path().join("tracked.txt"), "edited but not committed\n").unwrap();
        std::fs::write(d.path().join("new.txt"), "never added\n").unwrap();

        snapshot(d.path(), "conv-1").unwrap().expect("a changed tree must produce a snapshot");

        let r = ref_name("conv-1");
        assert_eq!(show(d.path(), &r, "tracked.txt").unwrap(), "edited but not committed");
        assert_eq!(show(d.path(), &r, "new.txt").unwrap(), "never added");
    }

    /// The property that lets this run on every turn: the user's own git state
    /// is exactly as they left it. Staging their whole tree behind their back
    /// would put files they deliberately left out into their next commit.
    #[test]
    fn snapshotting_does_not_touch_the_index_head_or_branch() {
        let d = repo();
        let p = d.path();
        std::fs::write(p.join("staged.txt"), "deliberately staged\n").unwrap();
        crate::git::ok(p, &["add", "staged.txt"]).unwrap();
        std::fs::write(p.join("unstaged.txt"), "deliberately not staged\n").unwrap();

        let head_before = crate::git::ok(p, &["rev-parse", "HEAD"]).unwrap();
        let status_before = crate::git::ok(p, &["status", "--porcelain"]).unwrap();

        snapshot(p, "conv-1").unwrap().unwrap();

        assert_eq!(crate::git::ok(p, &["rev-parse", "HEAD"]).unwrap(), head_before);
        assert_eq!(
            crate::git::ok(p, &["status", "--porcelain"]).unwrap(),
            status_before,
            "the staged/unstaged split must survive a snapshot"
        );
        // And the snapshot is not a branch: `git branch` must not list it.
        let branches = crate::git::ok(p, &["branch", "--list"]).unwrap();
        assert!(!branches.contains("autosave"), "snapshots must not appear as branches");
    }

    #[test]
    fn an_unchanged_tree_writes_no_second_commit() {
        let d = repo();
        std::fs::write(d.path().join("new.txt"), "x\n").unwrap();
        let first = snapshot(d.path(), "conv-1").unwrap().unwrap();
        assert_eq!(snapshot(d.path(), "conv-1").unwrap(), None, "an idle turn writes nothing");
        assert_eq!(
            crate::git::maybe(d.path(), &["rev-parse", &ref_name("conv-1")]).unwrap(),
            first
        );
    }

    #[test]
    fn each_conversation_gets_its_own_ref() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "from one\n").unwrap();
        snapshot(d.path(), "conv-1").unwrap().unwrap();
        std::fs::write(d.path().join("a.txt"), "from two\n").unwrap();
        snapshot(d.path(), "conv-2").unwrap().unwrap();

        assert_eq!(show(d.path(), &ref_name("conv-1"), "a.txt").unwrap(), "from one");
        assert_eq!(show(d.path(), &ref_name("conv-2"), "a.txt").unwrap(), "from two");
    }

    /// A snapshot belonging to a process that is still running is a live
    /// session's in-flight state — a second terminal, or a linked worktree
    /// sharing this ref namespace — not a crash to recover from.
    #[test]
    fn a_live_sessions_snapshot_is_never_reported_as_a_crash() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "x\n").unwrap();
        // Written by *this* process, which is alive by definition.
        snapshot(d.path(), "conv-live").unwrap().unwrap();
        assert!(orphans(d.path(), Some("conv-other")).is_empty());
    }

    /// The other side of that: a pid the kernel has since handed to something
    /// else. Liveness alone answered "still running" and the crashed session's
    /// work was never offered back — and `gc` never reclaimed the ref either,
    /// because it asks the same question. The recorded start time is what tells
    /// the two processes apart.
    #[test]
    fn a_snapshot_whose_pid_belongs_to_another_process_is_still_a_crash() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "unsaved work\n").unwrap();
        snapshot(d.path(), "conv-reused").unwrap().unwrap();

        // This pid *is* alive — it is ours — but the recorded start time is not.
        let tree = crate::git::ok(
            d.path(),
            &["rev-parse", &format!("{}^{{tree}}", ref_name("conv-reused"))],
        )
        .unwrap()
        .trim()
        .to_string();
        let msg = format!(
            "ws autosave\n\n{PID_TRAILER} {}\n{START_TRAILER} Mon Jan  1 00:00:00 2001\n",
            std::process::id()
        );
        let c = crate::git::ok(d.path(), &["commit-tree", &tree, "-m", &msg]).unwrap();
        crate::git::ok(d.path(), &["update-ref", &ref_name("conv-reused"), c.trim()]).unwrap();

        let found = orphans(d.path(), Some("conv-new"));
        assert_eq!(found.len(), 1, "a reused pid must not keep a dead session alive: {found:?}");
    }

    /// The index is the stat cache. Deleting it after every snapshot made
    /// `add -A` re-hash the whole repository on every turn — 0.9s per turn on a
    /// 12k-file tree, against 0.04s for a warm `git status`, and hooks are killed
    /// at ten seconds.
    #[test]
    fn the_index_survives_between_turns_and_goes_with_the_ref() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        snapshot(d.path(), "conv-idx").unwrap().unwrap();

        let index = index_path(d.path(), "conv-idx").unwrap();
        assert!(index.exists(), "the stat cache must outlive the turn that built it");

        std::fs::write(d.path().join("a.txt"), "two\n").unwrap();
        snapshot(d.path(), "conv-idx").unwrap().unwrap();
        assert_eq!(show(d.path(), &ref_name("conv-idx"), "a.txt").unwrap(), "two");

        // A clean exit takes both, so nothing accumulates in the git directory.
        discard(d.path(), "conv-idx");
        assert!(!index.exists(), "the index must go with the ref it belongs to");
    }

    #[test]
    fn a_dead_sessions_snapshot_is_offered_for_recovery() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "unsaved work\n").unwrap();
        snapshot(d.path(), "conv-dead").unwrap().unwrap();
        // Rewrite the snapshot with a pid that cannot be running. Pid 0 is never
        // a real process here, and `pid_alive` rejects it outright.
        let tree = crate::git::ok(
            d.path(),
            &["rev-parse", &format!("{}^{{tree}}", ref_name("conv-dead"))],
        )
        .unwrap()
        .trim()
        .to_string();
        let head = crate::git::ok(d.path(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let msg = format!("ws autosave\n\n{BASE_TRAILER} {head}\n{PID_TRAILER} 0\n");
        let c = crate::git::ok(d.path(), &["commit-tree", &tree, "-m", &msg]).unwrap();
        let c = c.trim();
        crate::git::ok(d.path(), &["update-ref", &ref_name("conv-dead"), c]).unwrap();

        let found = orphans(d.path(), Some("conv-new"));
        assert_eq!(found.len(), 1, "a dead session's snapshot must be reported: {found:?}");

        let notice = recovery_notice(d.path(), Some("conv-new")).expect("a notice");
        assert!(notice.contains("ended without closing cleanly"), "{notice}");
        // HEAD has not moved, so the whole-tree restore is safe to offer.
        assert!(notice.contains("checkout"), "{notice}");
        assert!(!notice.contains("HEAD has moved"), "{notice}");
    }

    /// The refusal cs had to learn: a whole-tree restore onto a moved HEAD
    /// writes an hour-old tree over work that has since been committed.
    #[test]
    fn a_moved_head_downgrades_the_offer_to_per_file() {
        let d = repo();
        let p = d.path();
        std::fs::write(p.join("a.txt"), "unsaved\n").unwrap();
        snapshot(p, "conv-dead").unwrap().unwrap();
        let tree =
            crate::git::ok(p, &["rev-parse", &format!("{}^{{tree}}", ref_name("conv-dead"))])
                .unwrap()
                .trim()
                .to_string();
        let msg = format!("ws autosave\n\n{BASE_TRAILER} 0000000000000000000000000000000000000000\n{PID_TRAILER} 0\n");
        let c = crate::git::ok(p, &["commit-tree", &tree, "-m", &msg]).unwrap();
        crate::git::ok(p, &["update-ref", &ref_name("conv-dead"), c.trim()]).unwrap();

        let notice = recovery_notice(p, Some("conv-new")).expect("a notice");
        assert!(notice.contains("HEAD has moved"), "{notice}");
        assert!(!notice.contains("checkout {} -- .\n"), "no whole-tree restore: {notice}");
    }

    /// A linked worktree's `.git` is a file holding a gitdir pointer, not a
    /// directory, so an index written beside it cannot be created. Every
    /// `base@feature` workspace is a linked worktree — assuming `<root>/.git`
    /// made this feature a silent no-op in exactly the workspaces most likely to
    /// be holding unmerged work.
    #[test]
    fn a_linked_worktree_snapshots_like_any_other_checkout() {
        let d = repo();
        // Somewhere of its own: `git worktree add` refuses a path that exists,
        // and a sibling of the repo's temp directory outlives the test and makes
        // the second run fail on the first run's leftovers.
        let elsewhere = TempDir::new().unwrap();
        let wt = elsewhere.path().join("wt-feature");
        crate::git::ok(d.path(), &["worktree", "add", "-q", "-b", "feature", wt.to_str().unwrap()])
            .unwrap();
        let wt = wt.canonicalize().unwrap();
        assert!(wt.join(".git").is_file(), "a linked worktree's .git is a file");

        std::fs::write(wt.join("in-the-worktree.txt"), "unmerged work\n").unwrap();
        snapshot(&wt, "conv-wt").unwrap().expect("a worktree must snapshot like any checkout");
        assert_eq!(
            show(&wt, &ref_name("conv-wt"), "in-the-worktree.txt").unwrap(),
            "unmerged work"
        );
    }

    #[test]
    fn a_clean_end_leaves_nothing_to_recover() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "x\n").unwrap();
        snapshot(d.path(), "conv-1").unwrap().unwrap();
        discard(d.path(), "conv-1");
        assert!(all(d.path()).is_empty(), "a clean exit must leave no snapshot behind");
    }

    #[test]
    fn outside_a_repo_everything_is_a_no_op() {
        let d = TempDir::new().unwrap();
        assert_eq!(snapshot(d.path(), "conv-1").unwrap(), None);
        assert!(all(d.path()).is_empty());
        assert_eq!(recovery_notice(d.path(), Some("conv-1")), None);
    }

    #[test]
    fn a_hostile_conversation_id_cannot_escape_the_ref_namespace() {
        for id in ["../../heads/main", "a b", "x^y~z", "..", ""] {
            let r = ref_name(id);
            assert!(r.starts_with("refs/ws/session/"), "{id:?} produced {r}");
            assert!(!r.contains(".."), "{id:?} produced {r}");
            // And git itself must accept the result.
            let d = repo();
            std::fs::write(d.path().join("a.txt"), "x\n").unwrap();
            snapshot(d.path(), id).unwrap().expect("a valid ref name");
        }
    }
}
