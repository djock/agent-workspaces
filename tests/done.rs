mod common;
use common::Env;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Raw git, for fixture setup and inspection only. Mirrors the `git()` helper
/// in `src/worktree.rs`'s own unit tests — the worktree/merge logic itself is
/// always driven through the `ws` binary in these tests, never called
/// directly.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Register `name` as a real git-backed workspace, through the binary
/// (`-adopt` needs no agent shim, unlike `ws <name>` launch). `-adopt` passes
/// `commit=false` to `contract::init`, so the `.ws/` bootstrap it writes is
/// still uncommitted afterward — give the repo an initial commit ourselves so
/// `git worktree add` has a HEAD to branch from, exactly as any real onboarded
/// workspace would.
fn base_workspace(env: &Env, name: &str) -> PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    git(&dir, &["config", "user.email", "dev@example.com"]);
    git(&dir, &["config", "user.name", "Dev"]);
    git(&dir, &["add", "-A"]);
    // `.ws/timeline.jsonl` is ws's own append-only bookkeeping; `contract::init`
    // writes it only *after* its own commit step, so a launch does not commit
    // it. Unstage it so this fixture matches that: committing it here would make
    // the *worktree's* later timeline append look like a tracked-file
    // modification, which the dirty check does not wave through (see the I2
    // comment in src/worktree.rs). Nothing stops a user's own `git add -A` from
    // staging it; that pre-existing behaviour is out of scope for these tests.
    git(&dir, &["reset", "--", ".ws/timeline.jsonl"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// Write `file`, stage just it and commit. Not `add -A`: that would also stage
/// `.ws/timeline.jsonl` (a real `git add -A` would too), and the resulting
/// dirt/merge behaviour is pre-existing and not what these tests are about, so
/// the fixture stays independent of it.
fn commit_in(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["config", "user.email", "dev@example.com"]);
    git(dir, &["config", "user.name", "Dev"]);
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
}

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

    env.cmd()
        .args(["api@done1", "-done"])
        .assert()
        .success()
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
    env.cmd()
        .args(["api", "-done", "--porcelain"])
        .assert()
        .success()
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
    env.cmd()
        .args(["api", "-done", "--porcelain"])
        .assert()
        .success()
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

    env.cmd()
        .args(["api", "-done", "--porcelain"])
        .assert()
        .success()
        .stdout(predicates::str::contains("f\tready"));
    env.cmd()
        .args(["api@f", "--merge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is in use by pid"));
    env.cmd().args(["api@f", "--merge", "--from-session"]).assert().success();
}

#[test]
fn marking_a_dirty_worktree_done_is_refused() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    std::fs::write(env.root.join("api@f/dirty.txt"), "x").unwrap();
    env.cmd()
        .args(["api@f", "-done"])
        .assert()
        .failure()
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
    env.cmd()
        .args(["api", "-done", "--porcelain"])
        .assert()
        .success()
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
    env.cmd()
        .current_dir(&f)
        .env("WS_WORKSPACE", "api@f")
        .env("WS_DIR", &f)
        .args(["-done", "--declined"])
        .assert()
        .success();
    assert!(!hook().contains("mark this feature done"), "a no is remembered for that commit");
    commit_in(&f, "b.txt");
    assert!(hook().contains("mark this feature done"), "a new commit is a new question");
}

/// Kills the child on drop, so a failed assertion never leaks a `sleep`.
struct Kill(std::process::Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_base_lock_held_by_a_non_ancestor_still_blocks_even_with_from_session() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    commit_in(&env.root.join("api@f"), "a.txt");
    env.cmd().args(["api@f", "-done"]).assert().success();
    // A sibling process, not an ancestor of the `ws` children below.
    let sibling = Kill(std::process::Command::new("sleep").arg("30").spawn().unwrap());
    std::fs::create_dir_all(base.join(".ws/local")).unwrap();
    std::fs::write(base.join(".ws/local/lock"), format!("pid = {}\n", sibling.0.id())).unwrap();

    env.cmd().args(["api", "-done", "--porcelain"]).assert().success().stdout(
        predicates::str::contains("f\tblocked").and(predicates::str::contains("f\tready").not()),
    );
    env.cmd()
        .args(["api@f", "--merge", "--from-session"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is in use by pid"));
}
