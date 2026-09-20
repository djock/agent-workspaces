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

// ---- the Stop-hook prompt (`done_check`) -----------------------------------

/// Run `ws internal stop` as workspace `name` rooted at `dir`; returns stdout.
fn stop_with(env: &Env, name: &str, dir: &Path, payload: &str) -> String {
    let out = env
        .cmd()
        .env("WS_WORKSPACE", name)
        .env("WS_DIR", dir)
        .args(["internal", "stop"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8_lossy(&out).to_string()
}

fn stop(env: &Env, name: &str, dir: &Path) -> String {
    stop_with(env, name, dir, "{}")
}

/// A base with `feature` created, one commit on it, and marked done.
fn done_feature(env: &Env, base: &str, feature: &str, file: &str) {
    env.cmd().arg(format!("{base}@{feature}")).assert().success();
    let f = env.root.join(format!("{base}@{feature}"));
    commit_in(&f, file);
    env.cmd().args([&format!("{base}@{feature}"), "-done"]).assert().success();
}

#[test]
fn the_base_stop_hook_asks_once_per_change_of_the_ready_set() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "one", "a.txt");

    let first = stop(&env, "api", &base);
    assert!(first.contains("\"decision\":\"block\""), "{first}");
    assert!(first.contains("one (1 commit(s))"), "{first}");
    assert!(first.contains("ws api -done"), "{first}");
    assert_eq!(stop(&env, "api", &base), "", "the same ready set is not asked twice");

    done_feature(&env, "api", "two", "b.txt");
    let again = stop(&env, "api", &base);
    assert!(again.contains("\"decision\":\"block\"") && again.contains("two"), "{again}");
    assert_eq!(stop(&env, "api", &base), "");
}

#[test]
fn a_base_with_only_blocked_worktrees_does_not_prompt() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "f", "a.txt");
    let f = env.root.join("api@f");
    std::fs::create_dir_all(f.join(".ws/local")).unwrap();
    std::fs::write(f.join(".ws/local/lock"), format!("pid = {}\n", std::process::id())).unwrap();
    assert_eq!(stop(&env, "api", &base), "", "nothing actionable, so nothing to ask");
}

#[test]
fn a_worktree_stop_hook_asks_once_per_commit_and_respects_the_answer() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, "a.txt");

    let first = stop(&env, "api@f", &f);
    assert!(first.contains("\"decision\":\"block\""), "{first}");
    assert!(first.contains("mark this feature done"), "{first}");
    assert_eq!(stop(&env, "api@f", &f), "", "asked once for this commit");

    env.cmd()
        .current_dir(&f)
        .env("WS_WORKSPACE", "api@f")
        .env("WS_DIR", &f)
        .args(["-done", "--declined"])
        .assert()
        .success();
    commit_in(&f, "b.txt");
    let new_head = stop(&env, "api@f", &f);
    assert!(new_head.contains("mark this feature done"), "a new commit is a new question");

    env.cmd().args(["api@f", "-done"]).assert().success();
    // Drop the stamp so silence can only come from the fresh marker, not from
    // this head having been asked about already.
    let _ = std::fs::remove_file(f.join(".ws/local/done-prompt.stamp"));
    assert_eq!(stop(&env, "api@f", &f), "", "already marked done");
}

#[test]
fn done_prompt_false_silences_the_hook_and_writes_no_stamp() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "f", "a.txt");
    env.cmd().args(["config", "set", "done_prompt", "false"]).assert().success();
    assert_eq!(stop(&env, "api", &base), "");
    assert!(!base.join(".ws/local/done-prompt.stamp").exists(), "opted out: nothing consumed");
    env.cmd().args(["config", "set", "done_prompt", "true"]).assert().success();
    assert!(stop(&env, "api", &base).contains("\"decision\":\"block\""), "opting back in asks");
}

#[test]
fn a_continuation_stop_never_asks() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "f", "a.txt");
    let out = stop_with(&env, "api", &base, r#"{"stop_hook_active":true}"#);
    assert_eq!(out, "");
    assert!(!base.join(".ws/local/done-prompt.stamp").exists());
}

#[test]
fn a_dirty_worktree_is_not_asked_about() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, "a.txt");
    std::fs::write(f.join("dirty.txt"), "x").unwrap();
    assert_eq!(stop(&env, "api@f", &f), "");
}

// ---- a locally modified tracked timeline in the base is bookkeeping ---------

/// Track `.ws/timeline.jsonl` in the base (as in a repo whose owner ran
/// `git add -A` once) and append a line, so it reads ` M`. Call *after* the
/// feature worktrees exist: a worktree branched from a base that tracks the file
/// would see its own launch append as a modification of a tracked file.
fn track_and_modify_timeline(base: &Path) -> String {
    git(base, &["add", "-f", ".ws/timeline.jsonl"]);
    git(base, &["commit", "-q", "-m", "track the timeline"]);
    let line = "{\"local\":\"edit\"}\n";
    let path = base.join(".ws/timeline.jsonl");
    let mut s = std::fs::read_to_string(&path).unwrap();
    s.push_str(line);
    std::fs::write(&path, s).unwrap();
    line.to_string()
}

fn sweep(env: &Env) -> String {
    let out = env.cmd().args(["api", "-done", "--porcelain"]).assert().success();
    String::from_utf8_lossy(&out.get_output().stdout).to_string()
}

fn done_feature_only(env: &Env, file: &str) -> PathBuf {
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    commit_in(&f, file);
    f
}

#[test]
fn a_modified_tracked_timeline_in_the_base_does_not_block_and_survives_the_merge() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature_only(&env, "a.txt");
    let edit = track_and_modify_timeline(&base);
    env.cmd().args(["api@f", "-done"]).assert().success();

    assert!(sweep(&env).contains("f\tready"), "the timeline edit is not dirt");
    env.cmd().args(["api@f", "--merge", "--from-session"]).assert().success();
    assert!(base.join("a.txt").is_file(), "the merge landed");
    let after = std::fs::read_to_string(base.join(".ws/timeline.jsonl")).unwrap();
    assert!(after.contains(&edit), "the local timeline edit is still there: {after}");
}

#[test]
fn a_feature_that_touches_the_timeline_keeps_it_as_base_dirt() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    let f = done_feature_only(&env, "a.txt");
    git(&f, &["add", "-f", ".ws/timeline.jsonl"]);
    git(&f, &["commit", "-q", "-m", "feature timeline"]);
    track_and_modify_timeline(&base);
    env.cmd().args(["api@f", "-done"]).assert().success();

    assert!(sweep(&env).contains("f\tblocked"), "{}", sweep(&env));
    env.cmd()
        .args(["api@f", "--merge", "--from-session"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("uncommitted changes"));
}

#[test]
fn a_staged_timeline_change_is_still_dirt() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature_only(&env, "a.txt");
    track_and_modify_timeline(&base);
    git(&base, &["add", ".ws/timeline.jsonl"]);
    env.cmd().args(["api@f", "-done"]).assert().success();
    assert!(sweep(&env).contains("f\tblocked"), "staged is not the unstaged bookkeeping case");
    env.cmd().args(["api@f", "--merge", "--from-session"]).assert().failure();
}

#[test]
fn another_modified_tracked_file_next_to_the_timeline_is_still_dirt() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature_only(&env, "a.txt");
    commit_in(&base, "t.txt");
    track_and_modify_timeline(&base);
    std::fs::write(base.join("t.txt"), "edited").unwrap();
    env.cmd().args(["api@f", "-done"]).assert().success();
    assert!(sweep(&env).contains("f\tblocked"));
    env.cmd()
        .args(["api@f", "--merge", "--from-session"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("t.txt"));
}

#[test]
fn a_conflicting_merge_leaves_the_base_and_its_timeline_edit_as_they_were() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    let f = done_feature_only(&env, "x.txt");
    std::fs::write(f.join("x.txt"), "feature side").unwrap();
    git(&f, &["commit", "-q", "-am", "feature x"]);
    std::fs::write(base.join("x.txt"), "base side").unwrap();
    git(&base, &["add", "x.txt"]);
    git(&base, &["commit", "-q", "-m", "base x"]);
    let edit = track_and_modify_timeline(&base);
    env.cmd().args(["api@f", "-done"]).assert().success();

    env.cmd()
        .args(["api@f", "--merge", "--from-session"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("was left untouched"));
    assert_eq!(std::fs::read_to_string(base.join("x.txt")).unwrap(), "base side");
    let after = std::fs::read_to_string(base.join(".ws/timeline.jsonl")).unwrap();
    assert!(after.contains(&edit), "the local timeline edit survived the abort: {after}");
    assert_eq!(git(&base, &["status", "--porcelain"]).trim_end(), " M .ws/timeline.jsonl");
}

// ---- fix wave: asked-set, cheap prechecks, the tracked-timeline configuration --

fn ws_at(env: &Env, name: &str, dir: &Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.current_dir(dir).env("WS_WORKSPACE", name).env("WS_DIR", dir);
    c
}

fn count_blocks(s: &str) -> usize {
    s.matches("\"decision\":\"block\"").count()
}

/// Base that already tracks its timeline, as in a real repo: worktrees are
/// created afterwards, so their own launch shows as a modification.
fn tracking_base(env: &Env, name: &str) -> PathBuf {
    let base = base_workspace(env, name);
    git(&base, &["add", "-f", ".ws/timeline.jsonl"]);
    git(&base, &["commit", "-q", "-m", "track the timeline"]);
    base
}

fn append_line(dir: &Path, line: &str) {
    let p = dir.join(".ws/timeline.jsonl");
    let mut s = std::fs::read_to_string(&p).unwrap_or_default();
    s.push_str(line);
    s.push('\n');
    std::fs::write(p, s).unwrap();
}

#[test]
fn merging_one_of_two_asked_worktrees_does_not_re_ask_about_the_other() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "one", "a.txt");
    done_feature(&env, "api", "two", "b.txt");
    assert_eq!(count_blocks(&stop(&env, "api", &base)), 1);
    env.cmd().args(["api@one", "--merge", "--from-session"]).assert().success();
    assert_eq!(stop(&env, "api", &base), "", "a smaller ready set is not news");
}

#[test]
fn a_worktree_that_flaps_out_of_ready_and_back_is_not_asked_again() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "one", "a.txt");
    done_feature(&env, "api", "two", "b.txt");
    assert_eq!(count_blocks(&stop(&env, "api", &base)), 1);
    let lock = env.root.join("api@one/.ws/local/lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, format!("pid = {}\n", std::process::id())).unwrap();
    assert_eq!(stop(&env, "api", &base), "", "one is blocked, two was already asked");
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(stop(&env, "api", &base), "", "one is back, and it was already asked");
}

#[test]
fn a_new_commit_on_an_asked_worktree_is_a_new_question() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "one", "a.txt");
    assert_eq!(count_blocks(&stop(&env, "api", &base)), 1);
    commit_in(&env.root.join("api@one"), "more.txt");
    env.cmd().args(["api@one", "-done"]).assert().success();
    assert_eq!(count_blocks(&stop(&env, "api", &base)), 1);
}

#[test]
fn a_stamp_that_cannot_be_written_means_no_prompt() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    done_feature(&env, "api", "one", "a.txt");
    std::fs::create_dir_all(base.join(".ws/local/done-prompt.stamp")).unwrap();
    assert_eq!(stop(&env, "api", &base), "", "an unrecordable question would repeat every turn");
}

#[test]
fn a_base_with_no_markers_is_silent_without_asking_git_anything() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@f").assert().success();
    commit_in(&env.root.join("api@f"), "a.txt");
    std::fs::remove_dir_all(env.root.join("api@f")).unwrap();
    assert_eq!(stop(&env, "api", &base), "");
}

#[test]
fn a_due_task_goes_first_and_the_done_question_follows_on_the_next_stop() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    ws_at(&env, "api", &base).args(["-task", "add", "water the plants"]).assert().success();
    // The queue file is a new untracked path in the base; commit it so it is not
    // taken for the user's uncommitted work and the worktree stays mergeable.
    git(&base, &["add", ".ws/queue"]);
    git(&base, &["commit", "-q", "-m", "queue"]);
    done_feature(&env, "api", "one", "a.txt");
    let first = stop(&env, "api", &base);
    assert_eq!(count_blocks(&first), 1, "{first}");
    assert!(first.contains("water the plants"), "{first}");
    let second = stop(&env, "api", &base);
    assert_eq!(count_blocks(&second), 1, "{second}");
    assert!(second.contains("ready to merge"), "{second}");
    assert_eq!(stop(&env, "api", &base), "");
}

#[test]
fn a_branch_that_renames_the_timeline_keeps_a_modified_base_timeline_as_dirt() {
    let env = Env::new();
    let base = tracking_base(&env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    git(&f, &["config", "user.email", "dev@example.com"]);
    git(&f, &["config", "user.name", "Dev"]);
    git(&f, &["checkout", "-q", "--", ".ws/timeline.jsonl"]);
    git(&f, &["mv", ".ws/timeline.jsonl", ".ws/moved.jsonl"]);
    git(&f, &["commit", "-q", "-m", "move the timeline"]);
    append_line(&base, "{\"local\":1}");
    env.cmd().args(["api@f", "-done"]).assert().success();
    assert!(sweep(&env).contains("f\tblocked"), "a rename must not hide the touched path");
    env.cmd()
        .args(["api@f", "--merge", "--from-session"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("uncommitted changes"));
}

/// The configuration the tool is actually used in: the timeline is tracked, so
/// every launch leaves a modification in the base *and* in each worktree.
fn real_config(env: &Env) -> (PathBuf, PathBuf) {
    let base = tracking_base(env, "api");
    env.cmd().arg("api@f").assert().success();
    let f = env.root.join("api@f");
    append_line(&base, "{\"launch\":\"base\"}");
    append_line(&f, "{\"launch\":\"feature\"}");
    commit_in(&f, "a.txt");
    assert!(git(&f, &["status", "--porcelain"]).contains(" M .ws/timeline.jsonl"));
    (base, f)
}

#[test]
fn a_worktree_with_a_tracked_modified_timeline_can_be_marked_done_and_merged_without_stranding() {
    let env = Env::new();
    let (base, f) = real_config(&env);
    env.cmd().args(["api@f", "-done"]).assert().success();
    assert!(sweep(&env).contains("f\tready"), "{}", sweep(&env));
    assert_eq!(count_blocks(&stop(&env, "api", &base)), 1, "the prompt can fire");

    env.cmd().args(["api@f", "--merge", "--from-session"]).assert().success();
    assert!(!f.exists(), "the worktree directory is gone");
    let reg = std::fs::read_to_string(env.home.path().join(".config/ws/registry.toml")).unwrap();
    assert!(!reg.contains("api@f"), "and it is unregistered: {reg}");
    assert!(base.join("a.txt").is_file());
    let tl = std::fs::read_to_string(base.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"launch\":\"base\""), "the base's local append survived: {tl}");
}

#[test]
fn a_staged_timeline_change_in_the_worktree_still_refuses() {
    let env = Env::new();
    let (_base, f) = real_config(&env);
    git(&f, &["add", ".ws/timeline.jsonl"]);
    env.cmd()
        .args(["api@f", "-done"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("uncommitted changes"));
}

#[test]
fn a_modified_timeline_plus_another_dirty_file_in_the_worktree_still_refuses() {
    let env = Env::new();
    let (_base, f) = real_config(&env);
    std::fs::write(f.join("a.txt"), "edited").unwrap();
    env.cmd()
        .args(["api@f", "-done"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("uncommitted changes"));
}
