mod common;
use common::Env;
use predicates::prelude::*;

#[test]
fn doctor_reports_agents_and_hook_state() {
    let env = Env::new();
    let claude = env.fake_claude();
    // run setup so there's something to check
    env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();

    env.cmd().env("WS_CLAUDE_BIN", &claude)
        .arg("-doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("claude"))
        .stdout(predicates::str::contains("hooks"));
}

/// Set up a git repo at `dir`, adopt it as a workspace, and optionally have the
/// repo ignore `.ws/`.
fn workspace_repo(env: &Env, name: &str, ignore_ws: bool) -> std::path::PathBuf {
    let proj = env.home.path().join(name);
    std::fs::create_dir_all(&proj).unwrap();
    for args in [vec!["init", "-q"], vec!["config", "user.email", "t@t"], vec!["config", "user.name", "t"]] {
        std::process::Command::new("git").arg("-C").arg(&proj).args(&args).output().unwrap();
    }
    if ignore_ws {
        std::fs::write(proj.join(".gitignore"), "/.ws/\n").unwrap();
    }
    env.cmd().current_dir(&proj).args(["-adopt", name]).assert().success();
    proj
}

/// A gitignored `.ws/` silently costs three things ws otherwise provides: the
/// init commit is skipped, notebooks and handoffs are never shared with a
/// co-developer, and the `merge=union` driver in `.ws/.gitattributes` cannot
/// run because a merge driver only applies to *tracked* files. `contract::init`
/// anticipates the ignored case and skips the commit without a word, so today
/// nothing tells you. Ignoring `.ws/` is a legitimate choice for a public repo
/// full of raw notes — so this is a note, not a failure.
#[test]
fn doctor_warns_when_the_workspace_ws_dir_is_gitignored() {
    let env = Env::new();
    let claude = env.fake_claude();
    let proj = workspace_repo(&env, "ignored", true);

    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .current_dir(&proj)
        .arg("-doctor")
        .assert()
        .success() // a deliberate choice, not a broken installation
        .stdout(predicates::str::contains(".ws/ is gitignored"))
        .stdout(predicates::str::contains("merge=union"));
}

#[test]
fn doctor_is_quiet_about_a_tracked_ws_dir() {
    let env = Env::new();
    let claude = env.fake_claude();
    let proj = workspace_repo(&env, "tracked", false);

    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .current_dir(&proj)
        .arg("-doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains(".ws/ is gitignored").not());
}

/// `-doctor` is the command people run when something is already wrong, so it
/// must not acquire a dependency on being inside a workspace or a git repo.
#[test]
fn doctor_says_nothing_about_ws_tracking_outside_a_workspace() {
    let env = Env::new();
    let claude = env.fake_claude();

    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .current_dir(env.home.path())
        .arg("-doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains(".ws/ is gitignored").not());
}

#[test]
fn doctor_flags_no_agents_installed() {
    let env = Env::new();
    // point both agent bins at a nonexistent path → neither installed
    env.cmd()
        .env("WS_CLAUDE_BIN", "/nope/claude")
        .env("WS_CODEX_BIN", "/nope/codex")
        .arg("-doctor")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no agent").or(predicates::str::contains("not installed")));
}
