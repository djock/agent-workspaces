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
