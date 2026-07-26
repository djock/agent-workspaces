mod common;
use common::Env;
use predicates::prelude::*;

fn launch_cmd(env: &Env, shim: &std::path::Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", shim).env("WS_NO_EXEC", "1");
    c
}

#[test]
fn first_launch_is_fresh_and_records_session_id() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();

    let log = env.argv_log();
    assert!(log.contains("--session-id"), "expected fresh launch, got: {log}");
    assert!(log.contains("WSW: proj"));
    assert!(log.contains(".ws/memory"));

    // state.toml recorded a session id
    let state = env.root.join("proj/.ws/local/state.toml");
    assert!(state.is_file());
    assert!(std::fs::read_to_string(state).unwrap().contains("session_id"));
}

#[test]
fn second_launch_resumes() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();
    launch_cmd(&env, &shim).arg("proj").assert().success();

    let log = env.argv_log();
    assert!(log.contains("--resume"), "second launch should resume, got: {log}");
}

#[test]
fn a_corrupt_registry_refuses_to_create_a_duplicate_workspace() {
    let env = Env::new();
    let shim = env.fake_claude();
    let reg = env.home.path().join(".config/ws/registry.toml");
    std::fs::create_dir_all(reg.parent().unwrap()).unwrap();
    std::fs::write(&reg, "not toml {{{").unwrap();

    let out = launch_cmd(&env, &shim).args(["proj"]).output().unwrap();

    assert!(!out.status.success(), "must not silently create a second workspace");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("registry.toml"), "stderr names the file: {err}");
    assert!(
        !env.root.join("proj/.ws").exists(),
        "and nothing was created on disk"
    );
}

#[test]
fn fresh_flag_forces_new_conversation() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();
    launch_cmd(&env, &shim).args(["proj", "--fresh"]).assert().success();

    // both launches used --session-id (fresh), never --resume
    let log = env.argv_log();
    assert!(!log.contains("--resume"), "got: {log}");
    assert_eq!(log.matches("--session-id").count(), 2);
}

#[test]
fn launch_exports_ws_dir() {
    let env = Env::new();
    let shim = env.fake_claude();
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", &shim).env("WS_NO_EXEC", "1");
    c.arg("wsdirtest").assert().success();
    // fake_claude logs WS_WORKSPACE; extend the shim to also log WS_DIR (see Step 3).
    assert!(env.argv_log().contains("WSDIR:"));
}

#[test]
fn launch_errors_clearly_when_claude_missing() {
    let env = Env::new();
    env.cmd()
        .env("WS_CLAUDE_BIN", "/nonexistent/definitely-not-claude")
        .env("WS_NO_EXEC", "1")
        .arg("missingclaude")
        .assert()
        .failure()
        .stderr(predicates::str::contains("claude").and(predicates::str::contains("not")));
}

#[test]
fn unknown_agent_errors_clearly() {
    let env = Env::new();
    let shim = env.fake_claude();
    // `gemini` is deliberately not an agent: ws supports claude and codex only,
    // so it must fall through to the same error as any other unknown name.
    launch_cmd(&env, &shim)
        .args(["proj", "--agent", "gemini"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("unknown agent")
                .and(predicates::str::contains("claude and codex")),
        );
}

#[test]
fn launch_with_agent_codex_uses_fake_codex() {
    let env = Env::new();
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim).env("WS_NO_EXEC", "1")
        .args(["cxproj", "--agent", "codex"])
        .assert().success();
    // fresh launch → no resume args logged; workspace scaffolded with AGENTS.md
    let root = env.root.join("cxproj");
    assert!(root.join("AGENTS.md").is_file(), "codex context file generated");
    // second launch resumes
    env.cmd()
        .env("WS_CODEX_BIN", &shim).env("WS_NO_EXEC", "1")
        .args(["cxproj", "--agent", "codex"])
        .assert().success();
    assert!(env.codex_argv_log().contains("resume --last"));
}

#[test]
fn switching_agents_clears_guard_and_records_default() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();

    // first launch with claude (default recorded = claude)
    env.cmd().env("WS_CLAUDE_BIN", &claude).env("WS_NO_EXEC","1")
        .arg("switchproj").assert().success();
    let root = env.root.join("switchproj");
    // plant a limit guard as if a threshold had been crossed
    std::fs::create_dir_all(root.join(".ws/local")).unwrap();
    std::fs::write(root.join(".ws/local/limit-guard"), "x").unwrap();

    // switch to codex
    env.cmd().env("WS_CODEX_BIN", &codex).env("WS_NO_EXEC","1")
        .args(["switchproj","--agent","codex"]).assert().success();

    // guard cleared on switch; default_agent now codex; AGENTS.md generated
    assert!(!root.join(".ws/local/limit-guard").exists(), "switch clears the limit guard");
    let wt = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
    assert!(wt.contains("default_agent = \"codex\""));
    assert!(root.join("AGENTS.md").is_file());
}
