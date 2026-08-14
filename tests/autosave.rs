//! Crash recovery, through the hooks that actually drive it.
//!
//! The unit tests in `src/autosave.rs` cover the snapshot mechanism. These cover
//! the wiring: that a turn ending takes a snapshot at all, that a session ending
//! cleanly leaves nothing behind, and that a launch after a crash says so.

mod common;
use common::Env;
use predicates::prelude::*;

/// A workspace on a real git repo, with one commit so HEAD exists.
fn workspace(env: &Env, name: &str) -> std::path::PathBuf {
    let proj = env.home.path().join(name);
    std::fs::create_dir_all(&proj).unwrap();
    for args in
        [vec!["init", "-q"], vec!["config", "user.email", "t@t"], vec!["config", "user.name", "t"]]
    {
        std::process::Command::new("git").arg("-C").arg(&proj).args(&args).output().unwrap();
    }
    std::fs::write(proj.join("tracked.txt"), "committed\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-q", "-m", "first"]] {
        std::process::Command::new("git").arg("-C").arg(&proj).args(&args).output().unwrap();
    }
    env.cmd().current_dir(&proj).args(["-adopt", name]).assert().success();
    proj
}

/// Drive one hook the way the agent does: payload on stdin, workspace in env.
fn hook(
    env: &Env,
    name: &str,
    root: &std::path::Path,
    handler: &str,
    session: &str,
) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_WORKSPACE", name)
        .env("WS_DIR", root)
        .env("WS_AGENT", "claude")
        .current_dir(root)
        .args(["internal", handler])
        .write_stdin(format!(r#"{{"session_id":"{session}"}}"#));
    c
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_turn_ending_saves_uncommitted_work_where_git_can_find_it() {
    let env = Env::new();
    let proj = workspace(&env, "proj");
    std::fs::write(proj.join("tracked.txt"), "an hour of edits\n").unwrap();
    std::fs::write(proj.join("untracked.txt"), "never added\n").unwrap();

    hook(&env, "proj", &proj, "stop", "conv-a").assert().success();

    let r = "refs/ws/session/conv-a";
    assert_eq!(git(&proj, &["show", &format!("{r}:tracked.txt")]), "an hour of edits");
    assert_eq!(git(&proj, &["show", &format!("{r}:untracked.txt")]), "never added");
}

/// The snapshot must be invisible to everything the user looks at. A save point
/// they have to notice is one they will trip over.
#[test]
fn snapshots_leave_the_users_git_state_untouched() {
    let env = Env::new();
    let proj = workspace(&env, "proj");
    std::fs::write(proj.join("staged.txt"), "staged on purpose\n").unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(&proj)
        .args(["add", "staged.txt"])
        .output()
        .unwrap();
    std::fs::write(proj.join("loose.txt"), "not staged\n").unwrap();
    let status_before = git(&proj, &["status", "--porcelain"]);
    let head_before = git(&proj, &["rev-parse", "HEAD"]);

    hook(&env, "proj", &proj, "stop", "conv-a").assert().success();

    assert_eq!(git(&proj, &["status", "--porcelain"]), status_before);
    assert_eq!(git(&proj, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(&proj, &["branch", "--list"]).matches('\n').count(), 0, "no new branch");
    assert_eq!(git(&proj, &["stash", "list"]), "", "the stash is not ws's to use");
}

#[test]
fn a_clean_session_end_leaves_nothing_to_recover() {
    let env = Env::new();
    let proj = workspace(&env, "proj");
    std::fs::write(proj.join("a.txt"), "work\n").unwrap();
    hook(&env, "proj", &proj, "stop", "conv-a").assert().success();
    assert!(!git(&proj, &["for-each-ref", "refs/ws/session"]).is_empty(), "snapshot taken");

    hook(&env, "proj", &proj, "session-end", "conv-a").assert().success();

    assert_eq!(
        git(&proj, &["for-each-ref", "refs/ws/session"]),
        "",
        "a session that ended on purpose is not a crash"
    );
}

/// The whole point: the next launch tells you the work is there and how to get
/// it. The snapshot here is left behind with no session-end, which is what a
/// killed process leaves, and its recorded pid is dead.
#[test]
fn a_launch_after_a_crash_reports_the_snapshot_and_how_to_restore_it() {
    let env = Env::new();
    let proj = workspace(&env, "proj");
    std::fs::write(proj.join("a.txt"), "an hour of unsaved work\n").unwrap();
    hook(&env, "proj", &proj, "stop", "conv-crashed").assert().success();

    // Re-point the ref at an identical tree whose recorded pid cannot be
    // running, standing in for the process that died.
    let tree = git(&proj, &["rev-parse", "refs/ws/session/conv-crashed^{tree}"]);
    let head = git(&proj, &["rev-parse", "HEAD"]);
    let msg = format!("ws autosave\n\nws-base: {head}\nws-pid: 0\n");
    let commit = git(&proj, &["commit-tree", &tree, "-m", &msg]);
    git(&proj, &["update-ref", "refs/ws/session/conv-crashed", &commit]);

    let claude = env.fake_claude();
    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_NO_EXEC", "1")
        .current_dir(&proj)
        .arg("proj")
        .assert()
        .success()
        .stdout(predicate::str::contains("ended without closing cleanly"))
        .stdout(predicate::str::contains("refs/ws/session/conv-crashed"))
        .stdout(predicate::str::contains("git checkout"));
}

/// A snapshot whose owner is still running belongs to a live session — a second
/// terminal, or a linked worktree sharing this ref namespace. Reporting it as a
/// crash would cry wolf on every launch for as long as that session lives.
#[test]
fn a_live_sessions_snapshot_is_not_reported_at_launch() {
    let env = Env::new();
    let proj = workspace(&env, "proj");
    std::fs::write(proj.join("a.txt"), "work\n").unwrap();
    // Written by the `ws` the hook spawns... which has exited by now, so record
    // this test process instead: alive by definition for as long as it runs.
    hook(&env, "proj", &proj, "stop", "conv-live").assert().success();
    let tree = git(&proj, &["rev-parse", "refs/ws/session/conv-live^{tree}"]);
    let head = git(&proj, &["rev-parse", "HEAD"]);
    let msg = format!("ws autosave\n\nws-base: {head}\nws-pid: {}\n", std::process::id());
    let commit = git(&proj, &["commit-tree", &tree, "-m", &msg]);
    git(&proj, &["update-ref", "refs/ws/session/conv-live", &commit]);

    let claude = env.fake_claude();
    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_NO_EXEC", "1")
        .current_dir(&proj)
        .arg("proj")
        .assert()
        .success()
        .stdout(predicate::str::contains("ended without closing cleanly").not());
}

/// Outside a git repo there is nothing to snapshot, and the hook must still
/// succeed — most of what ws manages is a git repo, but not all of it.
#[test]
fn a_workspace_that_is_not_a_repo_is_left_alone() {
    let env = Env::new();
    let proj = env.home.path().join("plain");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "plain"]).assert().success();
    std::fs::write(proj.join("a.txt"), "work\n").unwrap();

    hook(&env, "plain", &proj, "stop", "conv-a").assert().success();
}
