# Finished-worktree sweep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A feature worktree can mark itself done; when the user `/clear`s on the base workspace, ws offers every fresh, merge-ready `base@*` worktree for review and merge.

**Architecture:** A new `src/done.rs` owns the marker (`.ws/local/done.json`, gitignored), freshness, and classification; it calls `worktree` for all git and readiness. `SessionStart` with `source == "clear"` appends a one-line note to the context ws already injects. `readiness`/`merge` gain a caller-aware variant so a sweep run from inside the base's own session is not blocked by that session's lock.

**Tech Stack:** Rust, `anyhow`, `serde_json`, `assert_cmd` integration tests.

**Spec:** `docs/superpowers/specs/2026-09-19-finished-worktree-sweep-design.md`

## Deviations from the spec (found by reading the code; the spec is updated to match)

- **Marker path is `.ws/local/done.json`, not `.ws/done`.** `.ws/local/` is already gitignored (`contract.rs:100`), so no change to `WS_BOOKKEEPING` or to the dirty check is needed.
- **The base-lock open point is resolved.** `LockGuard::keep` is called before `exec`, so the lock pid *is* the agent process, and a Bash-tool `ws` is its descendant. The rule is "the base lock's pid is this process or an ancestor of it" — no `--from-session` guess needed for detection. The explicit `--from-session` flag on `--merge` is still added, because a plain `--merge` must keep refusing (existing behaviour) and the sweep needs a way to opt in.
- **The sweep has two modes.** On a tty it prompts merge/skip/open. Without one (the agent's Bash tool has no tty) it prints the report and the exact `--merge --from-session` commands, exits 0, and never waits.
- **The statusline chip is cached for 20 s** (`.ws/local/done-chip.json`): the bar repaints once a second and each freshness check runs git.

## Global Constraints

- Every git call goes through `crate::git` (`ok` / `raw` / `maybe`); no `Command::new("git")` in new code.
- Files ws shares are written with `crate::atomic::atomic_write`.
- A hook must never fail the session: every hook-path function returns `Option`/swallows errors.
- No destructive action gated on a lock uses `live_pid` (the folding variant); use `live_pid_checked`.
- No prompt may block without a tty. Non-interactive = print and exit 0.
- Nothing in this feature calls the Stop hook's `{"decision":"block"}` channel.
- A new verb must appear in `ws -h`; the existing test that derives verbs from the parser must stay green.
- Before tagging: drive the real binary through a pty (project memory: picker unit tests never touch key mapping) and verify persistence across two processes (project memory: vault suite went green while storing nothing).

## File Structure

- Create `src/done.rs` — marker I/O, freshness, `classify`, notes, chip cache. One responsibility: "what does done mean and who is done".
- Modify `src/worktree.rs` — `head_sha`, `is_clean`, `Feature.path`, `readiness_as`, `features_as`, `merge_as`.
- Modify `src/lock.rs` — `is_self_or_ancestor`.
- Modify `src/cli.rs` — `Cmd::Done`, `-done` parsing in three places, `--from-session`, help text.
- Modify `src/commands.rs` — `done(...)` (mark / undo / declined / sweep).
- Modify `src/main.rs` — dispatch `Cmd::Done`; `mod done;`.
- Modify `src/internal.rs` — append the note on `source == "clear"`.
- Modify `src/statusline.rs` — `Chip.done`.
- Create `tests/done.rs` — integration tests.
- Modify `README.md`, `CHANGELOG.md`.

---

### Task 1: Caller-aware readiness and merge

**Files:**
- Modify: `src/lock.rs` (add after `live_pid_checked`)
- Modify: `src/worktree.rs` (`Feature`, `readiness`, `features`, `merge`, add `head_sha`, `is_clean`)
- Test: unit tests in both files' `#[cfg(test)]` modules

**Interfaces:**
- Produces:
  - `lock::is_self_or_ancestor(pid: u32) -> bool`
  - `worktree::head_sha(dir: &Path) -> Result<String>`
  - `worktree::is_clean(dir: &Path) -> Result<bool>` (uses `--no-optional-locks`, ignores ws bookkeeping via `user_dirt`)
  - `worktree::Feature { name, feature, path: PathBuf, readiness }`
  - `worktree::readiness_as(base_path, base_ws, feature_path, feature_ws, branch, own_base_session: bool) -> Result<Readiness>`; `readiness(..)` becomes `readiness_as(.., false)`
  - `worktree::features_as(base: &str, own_base_session: bool) -> Result<Vec<Feature>>`; `features(base)` becomes `features_as(base, false)`
  - `worktree::merge_as(spec: &Spec, own_base_session: bool) -> Result<()>`; `merge(spec)` becomes `merge_as(spec, false)`

- [ ] **Step 1: Write the failing lock test** in `src/lock.rs` tests module

```rust
#[test]
fn self_and_parent_are_ancestors_but_a_stranger_is_not() {
    assert!(is_self_or_ancestor(std::process::id()));
    assert!(is_self_or_ancestor(std::os::unix::process::parent_id()));
    // Above i32::MAX there is no such pid; must be false, not a panic.
    assert!(!is_self_or_ancestor(u32::MAX));
    assert!(!is_self_or_ancestor(0));
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib lock::tests::self_and_parent -- --nocapture`
Expected: FAIL — `is_self_or_ancestor` not found.

- [ ] **Step 3: Implement**

```rust
/// Is `pid` this process or one of its ancestors?
///
/// A session's lock holds the agent's pid (`LockGuard::keep` runs before `exec`),
/// and every `ws` the agent runs through its Bash tool descends from it. So "the
/// lock is held by my own ancestor" is how a command running *inside* a session
/// recognises that session as itself rather than as another writer.
///
/// Walks `ps -o ppid=` (present on macOS and Linux) and is bounded, so a cycle
/// or an unreadable process table reads as "not an ancestor" — the safe answer
/// for a check that only ever *relaxes* a refusal.
pub fn is_self_or_ancestor(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let mut cur = std::process::id();
    for _ in 0..64 {
        if cur == pid {
            return true;
        }
        if cur <= 1 {
            return false;
        }
        let parent = Command::new("ps")
            .args(["-o", "ppid=", "-p", &cur.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u32>().ok());
        match parent {
            Some(p) if p != cur => cur = p,
            _ => return false,
        }
    }
    false
}
```

- [ ] **Step 4: Run** `cargo test --lib lock::tests::self_and_parent` — Expected: PASS.

- [ ] **Step 5: Write failing worktree tests** in `src/worktree.rs` tests module (reuse its `git`/`base_repo` helpers; follow the shape of `merging_brings_the_branch_back_with_a_merge_commit_and_removes_the_worktree`)

```rust
#[test]
fn head_sha_and_is_clean_track_the_checkout() {
    let td = TempDir::new().unwrap();
    let base = base_repo(&td);
    let sha = head_sha(&base).unwrap();
    assert_eq!(sha.len(), 40);
    assert!(is_clean(&base).unwrap());
    std::fs::write(base.join("x.txt"), "x").unwrap();
    assert!(!is_clean(&base).unwrap(), "an untracked user file is dirt");
}

#[test]
fn a_base_lock_held_by_an_ancestor_only_blocks_a_plain_readiness() {
    let td = TempDir::new().unwrap();
    let base = base_repo(&td);
    let feat = td.path().join("feat");
    add_worktree(&base, &feat, "feat").unwrap();
    std::fs::create_dir_all(base.join(".ws/local")).unwrap();
    // The lock names this test process, which is an ancestor of itself.
    std::fs::write(base.join(".ws/local/lock"), format!("pid = {}\n", std::process::id())).unwrap();

    let plain = readiness_as(&base, "api", &feat, "api@feat", "feat", false).unwrap();
    assert!(plain.blockers.iter().any(|b| matches!(b, Blocker::Live { workspace, .. } if workspace == "api")));

    let own = readiness_as(&base, "api", &feat, "api@feat", "feat", true).unwrap();
    assert!(!own.blockers.iter().any(|b| matches!(b, Blocker::Live { .. })));
}

#[test]
fn a_feature_lock_is_never_relaxed_even_for_the_callers_own_session() {
    let td = TempDir::new().unwrap();
    let base = base_repo(&td);
    let feat = td.path().join("feat");
    add_worktree(&base, &feat, "feat").unwrap();
    std::fs::create_dir_all(feat.join(".ws/local")).unwrap();
    std::fs::write(feat.join(".ws/local/lock"), format!("pid = {}\n", std::process::id())).unwrap();
    let r = readiness_as(&base, "api", &feat, "api@feat", "feat", true).unwrap();
    assert!(r.blockers.iter().any(|b| matches!(b, Blocker::Live { workspace, .. } if workspace == "api@feat")),
        "removing a directory under a running agent is what this blocker prevents");
}
```

- [ ] **Step 4: Run** `cargo test --lib worktree::tests` — Expected: FAIL to compile (`head_sha`, `is_clean`, `readiness_as` missing).

- [ ] **Step 5: Implement in `src/worktree.rs`**

Add:

```rust
/// The commit `dir` has checked out.
pub fn head_sha(dir: &Path) -> Result<String> {
    Ok(crate::git::ok(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// No uncommitted work of the user's in `dir`. `--no-optional-locks` because the
/// status-line chip calls this on a timer and must not contend with the user's
/// own git commands; ws's bookkeeping files are not the user's work.
pub fn is_clean(dir: &Path) -> Result<bool> {
    let porcelain = crate::git::ok(dir, &["--no-optional-locks", "status", "--porcelain"])?;
    Ok(user_dirt(&porcelain).is_empty())
}
```

Change `readiness` into a wrapper and thread the flag. The only change inside is the base-lock block:

```rust
pub fn readiness(
    base_path: &Path, base_ws: &str, feature_path: &Path, feature_ws: &str, branch: &str,
) -> Result<Readiness> {
    readiness_as(base_path, base_ws, feature_path, feature_ws, branch, false)
}

/// `own_base_session`: the caller is running inside the base's own session, so
/// a base lock held by this process or an ancestor is the caller, not a rival.
/// The feature's lock is never relaxed: that agent is somebody else's, and the
/// merge deletes its directory.
pub fn readiness_as(
    base_path: &Path, base_ws: &str, feature_path: &Path, feature_ws: &str, branch: &str,
    own_base_session: bool,
) -> Result<Readiness> {
    // ... body unchanged, except:
    if let Some(pid) = crate::lock::live_pid_checked(&base_path.join(".ws/local/lock"))? {
        if !(own_base_session && crate::lock::is_self_or_ancestor(pid)) {
            blockers.push(Blocker::Live { workspace: base_ws.to_string(), pid });
        }
    }
    // ...
}
```

Add `pub path: PathBuf` to `Feature`; set it in `features` (`path: path.clone()`). Rename `features` body to `features_as(base, own_base_session)` passing the flag to `readiness_as`; keep `pub fn features(base: &str) -> Result<Vec<Feature>> { features_as(base, false) }`. Rename `merge` body to `merge_as(spec, own_base_session)` (its `readiness(...)` call becomes `readiness_as(..., own_base_session)`); keep `pub fn merge(spec: &Spec) -> Result<()> { merge_as(spec, false) }`.

- [ ] **Step 6: Run the whole crate**

Run: `cargo test --lib && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings. Fix any other `Feature { .. }` constructors the compiler flags (add `path`).

- [ ] **Step 7: Commit**

```bash
git add src/lock.rs src/worktree.rs
git commit -m "worktree: readiness and merge can treat the caller's own base session as itself"
```

---

### Task 2: The marker, freshness, and classification (`src/done.rs`)

**Files:**
- Create: `src/done.rs`
- Modify: `src/main.rs` (`mod done;` next to `mod worktree;`)
- Test: unit tests in `src/done.rs`

**Interfaces:**
- Consumes: Task 1's `worktree::{head_sha, is_clean, features_as, Feature}`; `crate::atomic::atomic_write`; `crate::now_iso()`.
- Produces:
  - `done::Marker { head: String, at: String, by: String }` (serde)
  - `done::mark(root: &Path, by: &str) -> Result<String>` — writes the marker, returns the sha; bails if the tree is dirty
  - `done::undo(root: &Path) -> Result<()>`
  - `done::decline(root: &Path) -> Result<()>` / `done::was_declined(root: &Path, head: &str) -> bool`
  - `done::read(root: &Path) -> Option<Marker>`
  - `done::is_fresh(root: &Path) -> bool`
  - `done::Class { Ready, DoneBlocked(String), NotDone }`
  - `done::Row { feature: String, name: String, ahead: usize, class: Class }`
  - `done::classify(base: &str, own_base_session: bool) -> Result<Vec<Row>>`
  - `done::clear_note(ws_name: &str, ws_root: &Path) -> Option<String>`
  - `done::chip_count(ws_name: &str, ws_root: &Path) -> usize`

- [ ] **Step 1: Write the failing marker tests** (create `src/done.rs` with only the tests module and `use super::*;`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git").args(args).current_dir(dir).status().unwrap().success();
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
```

- [ ] **Step 2: Run** `cargo test --lib done::tests` — Expected: FAIL to compile.

- [ ] **Step 3: Implement the marker half** (above the tests module)

```rust
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
    matches!(worktree::head_sha(root), Ok(h) if h == m.head) && matches!(worktree::is_clean(root), Ok(true))
}

/// Mark the worktree at `root` done at its current `HEAD`.
pub fn mark(root: &Path, by: &str) -> Result<String> {
    if !worktree::is_clean(root)? {
        bail!("{} has uncommitted changes — commit or discard them before marking it done", root.display());
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
```

- [ ] **Step 4: Run** `cargo test --lib done::tests` — Expected: PASS (5 tests).

- [ ] **Step 5: Write failing classification test** (append to the tests module). It needs a real registry, so follow `worktree.rs`'s unit tests: they take `TEST_LOCK` and set `WS_ROOT`/`XDG_CONFIG_HOME`. Copy that setup from `worktree::tests` (e.g. from the test that calls `features`/`merge` through the registry). If none of the unit tests reach the registry, put this test in `tests/done.rs` (Task 5) instead and keep only the pure tests here.

```rust
// classify: covered end-to-end in tests/done.rs (Task 5) because it reads the
// registry; the pure pieces above are unit-tested here.
```

- [ ] **Step 6: Implement classification, notes, and the chip count** (append after `write_json`)

```rust
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

pub fn chip_count(ws_name: &str, ws_root: &Path) -> usize {
    if worktree::parse_name(ws_name).is_some() {
        return 0; // worktrees show nothing; the chip belongs to the base
    }
    let cache = ws_root.join(".ws/local/done-chip.json");
    let now = crate::limits::now_epoch() as u64;
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
```

(If `limits::now_epoch` returns a signed type, cast as shown; adjust to the actual return type the compiler reports.)

- [ ] **Step 7: Run** `cargo test --lib && cargo clippy --all-targets -- -D warnings` — Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/done.rs src/main.rs
git commit -m "done: a worktree marks itself finished, and freshness is tied to HEAD"
```

---

### Task 3: The `-done` verb and `--from-session`

**Files:**
- Modify: `src/cli.rs` (`Cmd`, three parse sites, help text, parser unit tests)
- Modify: `src/commands.rs` (add `done`)
- Modify: `src/main.rs` (dispatch; `Cmd::Worktree` gains `from_session`)
- Test: `src/cli.rs` parser tests

**Interfaces:**
- Consumes: Task 1 `worktree::merge_as`; Task 2 `done::{mark, undo, decline, classify, Class}`.
- Produces:
  - `Cmd::Done { name: Option<String>, undo: bool, declined: bool, porcelain: bool }`
  - `Cmd::Worktree { spec: String, merge: bool, from_session: bool }`
  - `commands::done(name: Option<String>, undo: bool, declined: bool, porcelain: bool) -> Result<()>`

- [ ] **Step 1: Write the failing parser tests** (in `cli.rs` tests, using the existing `p(&[..])` helper; also update the two existing `Cmd::Worktree { .. }` assertions at ~938–941 to add `from_session: false`)

```rust
#[test]
fn parses_done_in_all_three_positions() {
    assert_eq!(p(&["-done"]), Cmd::Done { name: None, undo: false, declined: false, porcelain: false });
    assert_eq!(p(&["-done", "--declined"]), Cmd::Done { name: None, undo: false, declined: true, porcelain: false });
    assert_eq!(p(&["api", "-done"]), Cmd::Done { name: Some("api".into()), undo: false, declined: false, porcelain: false });
    assert_eq!(p(&["api", "-done", "--porcelain"]), Cmd::Done { name: Some("api".into()), undo: false, declined: false, porcelain: true });
    assert_eq!(p(&["api@retry", "-done", "--undo"]), Cmd::Done { name: Some("api@retry".into()), undo: true, declined: false, porcelain: false });
}

#[test]
fn from_session_only_applies_to_a_merge() {
    assert_eq!(
        p(&["api@retry", "--merge", "--from-session"]),
        Cmd::Worktree { spec: "api@retry".into(), merge: true, from_session: true }
    );
    assert!(parse(vec!["api@retry".into(), "--from-session".into()]).is_err());
}

#[test]
fn undo_and_declined_are_opposites() {
    assert!(parse(vec!["-done".into(), "--undo".into(), "--declined".into()]).is_err());
}
```

- [ ] **Step 2: Run** `cargo test --lib cli::tests` — Expected: FAIL (variants missing).

- [ ] **Step 3: Implement parsing**

Add to `Cmd`:

```rust
    /// `ws -done` / `ws <base> -done` / `ws <base>@<feature> -done` — mark a
    /// feature worktree finished, or (on a base) sweep the ones that have.
    Done { name: Option<String>, undo: bool, declined: bool, porcelain: bool },
```

Change `Worktree { spec, merge }` to `Worktree { spec, merge, from_session }`.

Add a shared helper used by all three sites:

```rust
/// The flags `-done` takes, after the verb.
fn parse_done_flags(name: Option<String>, rest: impl Iterator<Item = String>) -> Result<Cmd> {
    let (mut undo, mut declined, mut porcelain) = (false, false, false);
    for a in rest {
        match a.as_str() {
            "--undo" => undo = true,
            "--declined" => declined = true,
            "--porcelain" => porcelain = true,
            other => bail!("unexpected argument: {other}"),
        }
    }
    if undo && declined {
        bail!("--undo and --declined are opposites; pick one");
    }
    Ok(Cmd::Done { name, undo, declined, porcelain })
}
```

1. **Top level**, next to `"-whoami"` in the `match first.as_str()`: `"-done" => parse_done_flags(None, it),`
2. **Worktree arm** (`name if parse_name(name).is_some()`): replace the loop so it accepts `--merge`, `--from-session`, and `-done`. Collect the args first:

```rust
let rest: Vec<String> = it.collect();
if rest.first().map(String::as_str) == Some("-done") {
    return parse_done_flags(Some(name.to_string()), rest.into_iter().skip(1));
}
let (mut merge, mut from_session) = (false, false);
for a in rest {
    match a.as_str() {
        "--merge" => merge = true,
        "--from-session" => from_session = true,
        other => bail!("unexpected argument: {other}"),
    }
}
if from_session && !merge {
    bail!("--from-session only applies to --merge");
}
Ok(Cmd::Worktree { spec: name.to_string(), merge, from_session })
```

3. **Launch arm**: add `"-done" => done = true,` (declare `let mut done = false;` beside `features`). Flags `--undo`/`--declined` there: collect them into locals `undo`/`declined` in the same `match` (`"--undo" => undo = true, "--declined" => declined = true`), and after the loop, before the `features` check:

```rust
if done {
    if undo && declined { bail!("--undo and --declined are opposites; pick one"); }
    return Ok(Cmd::Done { name: Some(name.to_string()), undo, declined, porcelain });
}
if undo || declined {
    bail!("--undo and --declined only apply to `-done`");
}
```

Add the verb to `HELP` under "Worktrees":

```
 ws <base> -done               review the feature worktrees marked done and merge
                                 the ones you approve (--porcelain)
 ws -done                      mark this feature worktree done (--undo, --declined)
```

and `--from-session` to the `--merge` line: `(--from-session when run inside <base>'s own session)`. Add `"-done"` handling to the `VerbHelp` key derivation if the compiler/test for it requires (`v @ (...)` list near line 395).

- [ ] **Step 4: Run** `cargo test --lib cli::` — Expected: PASS, including the existing test that derives verbs from the parser and checks `ws -h`.

- [ ] **Step 5: Implement `commands::done`** (near `commands::features`)

```rust
/// `ws -done` — mark a feature worktree finished, or sweep a base's finished ones.
pub fn done(name: Option<String>, undo: bool, declined: bool, porcelain: bool) -> Result<()> {
    // Bare `-done` means "this workspace", as `-task add` does.
    let name = match name {
        Some(n) => n,
        None => crate::internal::current_ws()
            .map(|w| w.name)
            .ok_or_else(|| anyhow::anyhow!("not inside a ws workspace — name one: `ws <name> -done`"))?,
    };
    if let Some(spec) = crate::worktree::parse_name(&name) {
        let root = crate::registry::lookup_checked(&name)?
            .ok_or_else(|| anyhow::anyhow!("no workspace named {name}"))?;
        if undo {
            crate::done::undo(&root)?;
            println!("{name} is no longer marked done");
        } else if declined {
            crate::done::decline(&root)?;
        } else {
            let head = crate::done::mark(&root, "manual")?;
            println!("{name} marked done at {}; `ws {} -done` will offer it for merge", &head[..8.min(head.len())], spec.base);
        }
        return Ok(());
    }
    if undo || declined {
        anyhow::bail!("--undo and --declined apply to a feature worktree, not a base");
    }
    sweep(&name, porcelain)
}

fn sweep(base: &str, porcelain: bool) -> Result<()> {
    use crate::done::Class;
    let rows = crate::done::classify(base, true)?;
    if porcelain {
        for r in &rows {
            let (state, why) = match &r.class {
                Class::Ready => ("ready", String::new()),
                Class::DoneBlocked(w) => ("blocked", w.clone()),
                Class::NotDone => ("not-done", String::new()),
            };
            println!("{}\t{}\t{}\t{}", r.feature, state, r.ahead, why);
        }
        return Ok(());
    }
    if rows.is_empty() {
        println!("{base} has no feature worktrees (create one with `ws {base}@<feature>`)");
        return Ok(());
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let base_path = crate::registry::lookup_checked(base)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {base}"))?;
    let mut ready = 0;
    for r in &rows {
        match &r.class {
            Class::NotDone => println!("· {}  not marked done", r.feature),
            Class::DoneBlocked(why) => println!("✗ {}  done, but {why}", r.feature),
            Class::Ready => {
                ready += 1;
                println!("✓ {}  {} commit(s)", r.feature, r.ahead);
                if let Some(log) = crate::git::maybe(&base_path, &["log", "--oneline", &format!("HEAD..{}", r.feature)]) {
                    print!("{log}");
                }
                if let Some(stat) = crate::git::maybe(&base_path, &["diff", "--stat", &format!("HEAD...{}", r.feature)]) {
                    print!("{stat}");
                }
                if interactive {
                    sweep_prompt(base, &r.feature)?;
                } else {
                    println!("  merge with: ws {base}@{} --merge --from-session", r.feature);
                }
            }
        }
    }
    if ready == 0 {
        println!("\nNothing is ready to merge.");
    }
    Ok(())
}

/// One worktree, one answer. A failed merge is reported and the sweep goes on:
/// a conflict in one worktree must not strand the rest.
fn sweep_prompt(base: &str, feature: &str) -> Result<()> {
    use std::io::Write;
    print!("  [m]erge / [s]kip / [o]pen? ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    match line.trim().to_lowercase().as_str() {
        "m" | "merge" => {
            let spec = crate::worktree::Spec { base: base.into(), feature: feature.into() };
            if let Err(e) = crate::worktree::merge_as(&spec, true) {
                eprintln!("  {e:#}");
            }
        }
        "o" | "open" => println!("  open it with: ws {base}@{feature}"),
        _ => println!("  skipped"),
    }
    Ok(())
}
```

(`IsTerminal` is already imported in `commands.rs` if `features`-adjacent code uses it; add `use std::io::IsTerminal;` otherwise.)

- [ ] **Step 6: Dispatch in `src/main.rs`**

```rust
Cmd::Done { name, undo, declined, porcelain } => commands::done(name, undo, declined, porcelain)?,
```

and in the `Cmd::Worktree` arm: destructure `from_session`, and replace `worktree::merge(&s)?` with `worktree::merge_as(&s, from_session)?`.

- [ ] **Step 7: Run** `cargo build && cargo test --lib && cargo clippy --all-targets -- -D warnings` — Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/cli.rs src/commands.rs src/main.rs
git commit -m "add ws -done: mark a worktree finished, or sweep a base's finished ones"
```

---

### Task 4: The `/clear` note and the status-line chip

**Files:**
- Modify: `src/internal.rs` (`session_start`)
- Modify: `src/statusline.rs` (`Chip`, `run`, `render`)
- Test: unit tests in `statusline.rs`; hook behaviour in Task 5

**Interfaces:**
- Consumes: Task 2 `done::clear_note`, `done::chip_count`.
- Produces: `Chip.done: usize`

- [ ] **Step 1: Write the failing statusline test** (in `statusline.rs` tests, next to the existing chip tests; the `chip(color)` helper builds a `Chip` — add `done: 0` to it and to any other `Chip { .. }` literal the compiler flags)

```rust
#[test]
fn the_chip_shows_how_many_worktrees_are_done_and_nothing_when_none() {
    let mut c = chip(None);
    c.done = 2;
    let with = render(&input("Sonnet", 10.0, 1.0, 1.0), Some(&c), PLAIN);
    assert!(with.contains("done 2"), "{with}");
    c.done = 0;
    let without = render(&input("Sonnet", 10.0, 1.0, 1.0), Some(&c), PLAIN);
    assert!(!without.contains("done"), "{without}");
}
```

- [ ] **Step 2: Run** `cargo test --lib statusline::` — Expected: FAIL (no field).

- [ ] **Step 3: Implement.** Add `pub done: usize` to `Chip` (doc: "Feature worktrees marked done — see `done::chip_count`"). In `run()`, set `done: crate::done::chip_count(&ws.name, &ws.root)`. In `render`, directly after the `if c.unread > 0 { parts.push(format!("mail {}", c.unread)); }` block:

```rust
if c.done > 0 {
    parts.push(format!("done {}", c.done));
}
```

- [ ] **Step 4: Append the note in `session_start`** (`src/internal.rs`), replacing the final `println!`:

```rust
let mut ctx = build_context(&ws);
// `/clear` is where the user says "this task is finished". Only then does ws
// raise the finished-worktree question — never at startup or resume, when
// nothing has just finished.
if h.source == "clear" {
    if let Some(note) = crate::done::clear_note(&ws.name, &ws.root) {
        ctx.push_str("\n\n");
        ctx.push_str(&note);
    }
}
println!("{}", hookio::additional_context("SessionStart", &ctx));
```

- [ ] **Step 5: Run** `cargo test --lib && cargo clippy --all-targets -- -D warnings` — Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/internal.rs src/statusline.rs
git commit -m "raise the finished-worktree question on /clear, and show a done chip"
```

---

### Task 5: Integration tests, docs, and the real-binary check

**Files:**
- Create: `tests/done.rs`
- Modify: `README.md`, `CHANGELOG.md`, `docs/superpowers/specs/2026-09-19-finished-worktree-sweep-design.md` (apply the four deviations above)

- [ ] **Step 1: Write `tests/done.rs`.** Start with `mod common;`, the `git` and `base_workspace` helpers copied verbatim from `tests/worktree.rs` lines 1–41, and a `commit_in(dir, file)` helper that writes a file, `git add -A`, `git commit`. Then:

```rust
#[test]
fn only_the_worktree_that_marked_itself_done_is_offered_and_it_merges() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().args(["api@done1"]).assert().success();
    env.cmd().args(["api@wip"]).assert().success();
    let done1 = env.root.join("api@done1");
    let wip = env.root.join("api@wip");
    commit_in(&done1, "a.txt");
    commit_in(&wip, "b.txt");

    env.cmd().args(["api@done1", "-done"]).assert().success()
        .stdout(predicates::str::contains("marked done"));

    env.cmd().args(["api", "-done", "--porcelain"]).assert().success().stdout(
        predicates::str::contains("done1\tready\t1")
            .and(predicates::str::contains("wip\tnot-done")),
    );

    env.cmd().args(["api@done1", "--merge", "--from-session"]).assert().success();
    assert!(base.join("a.txt").is_file(), "the approved worktree landed on main");
    assert!(!base.join("b.txt").exists(), "the unmarked one was not touched");
}

#[test]
fn a_new_commit_after_marking_makes_the_worktree_not_done_again() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, "a.txt");
    env.cmd().args(["api@f", "-done"]).assert().success();
    commit_in(&f, "more.txt");
    env.cmd().args(["api", "-done", "--porcelain"]).assert().success()
        .stdout(predicates::str::contains("f\tnot-done"));
}

#[test]
fn a_done_worktree_whose_agent_is_still_open_is_listed_blocked_not_offered() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, "a.txt");
    env.cmd().args(["api@f", "-done"]).assert().success();
    std::fs::create_dir_all(f.join(".ws/local")).unwrap();
    // This test process is alive, so the lock reads as a running agent.
    std::fs::write(f.join(".ws/local/lock"), format!("pid = {}\n", std::process::id())).unwrap();
    env.cmd().args(["api", "-done", "--porcelain"]).assert().success()
        .stdout(predicates::str::contains("f\tblocked\t1"));
}

#[test]
fn the_sweep_is_not_blocked_by_the_base_sessions_own_lock_but_a_plain_merge_is() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, "a.txt");
    env.cmd().args(["api@f", "-done"]).assert().success();
    std::fs::create_dir_all(base.join(".ws/local")).unwrap();
    // The `ws` child below descends from this test process: it is the caller's own session.
    std::fs::write(base.join(".ws/local/lock"), format!("pid = {}\n", std::process::id())).unwrap();

    env.cmd().args(["api", "-done", "--porcelain"]).assert().success()
        .stdout(predicates::str::contains("f\tready"));
    env.cmd().args(["api@f", "--merge"]).assert().failure()
        .stderr(predicates::str::contains("is in use by pid"));
    env.cmd().args(["api@f", "--merge", "--from-session"]).assert().success();
}

#[test]
fn marking_a_dirty_worktree_done_is_refused() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    std::fs::write(env.root.join("api@f/dirty.txt"), "x").unwrap();
    env.cmd().args(["api@f", "-done"]).assert().failure()
        .stderr(predicates::str::contains("uncommitted changes"));
}

/// The persistence check project memory asks for: mark in one process, read in another.
#[test]
fn the_marker_survives_across_processes() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    commit_in(&env.root.join("api@f"), "a.txt");
    env.cmd().args(["api@f", "-done"]).assert().success();
    assert!(env.root.join("api@f/.ws/local/done.json").is_file());
    env.cmd().args(["api", "-done", "--porcelain"]).assert().success()
        .stdout(predicates::str::contains("f\tready"));
}

#[test]
fn clear_on_a_base_names_the_finished_worktrees_and_startup_does_not() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    commit_in(&env.root.join("api@f"), "a.txt");
    env.cmd().args(["api@f", "-done"]).assert().success();

    let hook = |source: &str| {
        env.cmd()
            .env("WS_WORKSPACE", "api")
            .env("WS_DIR", &base)
            .args(["internal", "session-start"])
            .write_stdin(format!(r#"{{"source":"{source}"}}"#))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };
    let on_clear = String::from_utf8_lossy(&hook("clear")).to_string();
    assert!(on_clear.contains("marked done: f"), "{on_clear}");
    let on_startup = String::from_utf8_lossy(&hook("startup")).to_string();
    assert!(!on_startup.contains("marked done"), "{on_startup}");
}

#[test]
fn clear_in_a_worktree_asks_once_per_commit() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, "a.txt");

    let hook = || {
        String::from_utf8_lossy(
            &env.cmd()
                .env("WS_WORKSPACE", "api@f")
                .env("WS_DIR", &f)
                .args(["internal", "session-start"])
                .write_stdin(r#"{"source":"clear"}"#)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .to_string()
    };
    assert!(hook().contains("mark this feature done"));
    env.cmd().current_dir(&f).env("WS_WORKSPACE", "api@f").env("WS_DIR", &f)
        .args(["-done", "--declined"]).assert().success();
    assert!(!hook().contains("mark this feature done"), "a no is remembered for that commit");
    commit_in(&f, "b.txt");
    assert!(hook().contains("mark this feature done"), "a new commit is a new question");
}
```

- [ ] **Step 2: Run** `cargo test --test done` — Expected: PASS. If a `session-start` test fails on hook plumbing (e.g. the hook needs `WS_AGENT` or a git repo in `WS_DIR`), fix the fixture, not the assertion.

- [ ] **Step 3: Full verification**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all green. Per `docs/releasing.md`, also run the musl cross-check before any tag.

- [ ] **Step 4: Drive the real binary through a pty** (interactive sweep prompt; never covered by unit tests). In a scratch dir with `HOME`/`WS_ROOT` pointing at a temp tree: create `api`, two worktrees with commits, mark one, then run `ws api -done` under `script -q /dev/null` (or `expect`), answer `m`, and confirm the merge lands and a bad answer skips.

- [ ] **Step 5: Docs.** README: a short "Finishing feature worktrees" section (mark, `/clear` note, `-done` sweep, chip, and that a still-open worktree session blocks its merge). CHANGELOG: an entry under the next version. Update the spec's marker path, `--from-session`, the resolved lock question, and the two-mode sweep.

- [ ] **Step 5b: Notebook.** Append the outcome and any surprises to `.ws/notebook/notebook.im-ionutmocanu-gmail-com.md`.

- [ ] **Step 6: Commit**

```bash
git add tests/done.rs README.md CHANGELOG.md docs
git commit -m "test and document the finished-worktree sweep"
```

---

## Self-review

- **Spec coverage:** marker + freshness (T2); worktree-side ask on `/clear` once per HEAD (T2 `clear_note`, T4 hook, T5 test); manual `-done`/`--undo` (T3); base-side note on `/clear` (T2, T4); sweep classification into Ready / Done-blocked / Not-done (T2); diffstat + merge/skip/open (T3); live-session blockers incl. base-lock ancestry (T1); chip (T2, T4); non-tty never blocks (T3 `sweep`); error tolerance for hooks (T2 returns `Option`); tests incl. cross-process persistence and pty (T5). Zero-token `SessionEnd` spike is deliberately not a task — it is optional and unverified.
- **Placeholders:** none. Two spots tell the implementer to adapt to compiler output (`now_epoch` type; other `Chip`/`Feature` literals) — both are mechanical.
- **Type consistency:** `mark(root, by)`, `undo(root)`, `decline(root)`, `was_declined(root, head)`, `classify(base, own)`, `features_as`, `merge_as`, `Cmd::Done{name,undo,declined,porcelain}`, `Cmd::Worktree{spec,merge,from_session}` are used identically across tasks.
