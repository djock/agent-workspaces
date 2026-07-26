mod common;
use common::Env;

#[test]
fn setup_installs_codex_hooks_and_prompts_when_codex_present() {
    let env = Env::new();
    // Make codex "installed" by pointing WS_CODEX_BIN at a shim that exits 0 on --version.
    let shim = env.fake_codex();
    env.cmd().env("WS_CODEX_BIN", &shim).arg("setup").assert().success()
        .stdout(predicates::str::contains("codex"))
        .stdout(predicates::str::contains("/hooks")); // trust note surfaced

    // ~/.codex/hooks.json got the ws SessionStart hook
    let hooks = env.home.path().join(".codex/hooks.json");
    let body = std::fs::read_to_string(&hooks).unwrap();
    assert!(body.contains("session-start.sh"));
    // namespaced codex prompt installed
    assert!(env.home.path().join(".codex/prompts/ws-summary.md").is_file());
}

#[test]
fn setup_installs_hooks_and_prompts() {
    let env = Env::new();
    let shim = env.fake_claude();
    env.cmd()
        .env("WS_CLAUDE_BIN", &shim)
        .arg("setup")
        .assert()
        .success()
        .stdout(predicates::str::contains("hook"))
        .stdout(predicates::str::contains("prompt"));

    // settings.json registered a ws SessionStart hook
    let settings = env.home.path().join(".claude/settings.json");
    let body = std::fs::read_to_string(&settings).unwrap();
    assert!(body.contains("session-start.sh"));

    // namespaced prompts installed
    assert!(env.home.path().join(".claude/commands/ws/summary.md").is_file());
    assert!(env.home.path().join(".claude/commands/ws/rotate.md").is_file());
}

#[test]
fn setup_registers_statuslines_and_backs_up_prior() {
    let env = Env::new();
    // pre-existing foreign statusline (cs) must be backed up, not lost
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(&sp, r#"{"statusLine":{"type":"command","command":"/opt/cs/cs-statusline"}}"#).unwrap();

    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    let cmd = settings["statusLine"]["command"].as_str().unwrap();
    assert!(cmd.ends_with(" statusline"), "statusLine should be ws, got {cmd}");
    assert!(settings["subagentStatusLine"]["command"].as_str().unwrap().ends_with(" subagent-statusline"));

    // the prior cs command was recorded to the backup file
    let backup = std::fs::read_to_string(
        env.home.path().join(".config/ws/statusline-backup.json")
    ).unwrap_or_default();
    assert!(backup.contains("cs-statusline"), "prior statusline must be backed up");
}

#[test]
fn setup_backs_up_arbitrary_prior_statusline() {
    let env = Env::new();
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(&sp, r#"{"statusLine":{"type":"command","command":"/usr/local/bin/my-custom-line --flag"}}"#).unwrap();

    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    let backup = std::fs::read_to_string(env.home.path().join(".config/ws/statusline-backup.json")).unwrap_or_default();
    assert!(backup.contains("/usr/local/bin/my-custom-line --flag"), "prior custom statusline must be backed up verbatim, got: {backup}");
    // and settings.json now points at ws
    let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    assert!(settings["statusLine"]["command"].as_str().unwrap().ends_with(" statusline"));
}

#[test]
fn setup_backup_merges_and_never_drops_a_prior_original() {
    let env = Env::new();
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();

    // Run 1: only a foreign statusLine present.
    std::fs::write(&sp, r#"{"statusLine":{"type":"command","command":"/opt/cs/cs-statusline"}}"#).unwrap();
    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    // Between runs the user sets a foreign subagentStatusLine by hand (statusLine now ws).
    let mut s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    s["subagentStatusLine"] = serde_json::json!({"type":"command","command":"/opt/cs/cs-sub"});
    std::fs::write(&sp, serde_json::to_string(&s).unwrap()).unwrap();

    // Run 2.
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    // Both foreign originals must still be in the backup.
    let backup = std::fs::read_to_string(env.home.path().join(".config/ws/statusline-backup.json")).unwrap_or_default();
    assert!(backup.contains("cs-statusline"), "run-1 backup lost: {backup}");
    assert!(backup.contains("cs-sub"), "run-2 backup missing: {backup}");
}
