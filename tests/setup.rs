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

    // ~/.codex/hooks.json got the ws hooks — asserted on the *parsed* document.
    //
    // This used to be `body.contains("session-start.sh")`, which passes for any
    // schema and any matcher. It passed for two releases while Codex secret
    // redaction was dead, because the matcher was Claude's `Write|Edit` and Codex
    // reports `apply_patch`. Assert the structure and the matcher, or this class
    // of bug is invisible again.
    let hooks = env.home.path().join(".codex/hooks.json");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks).unwrap()).unwrap();

    let by_event = doc["hooks"].as_object().expect("hooks must be a JSON object");
    for event in ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop", "SessionEnd", "PostToolUse"] {
        assert!(by_event.contains_key(event), "{event} not registered: {doc}");
    }
    assert_eq!(
        doc["hooks"]["SessionStart"][0]["hooks"][0]["type"], "command",
        "per-event entries must be matcher-groups holding a command list"
    );
    assert!(doc["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str().unwrap().contains("session-start.sh"));

    // The matchers must be Codex's tool names, not Claude's. Verified against
    // Codex CLI 0.145.0: shell arrives as `Bash`, a file edit as `apply_patch`.
    assert_eq!(doc["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    let redact = doc["hooks"]["PostToolUse"][0]["matcher"].as_str().unwrap();
    assert!(
        redact.contains("apply_patch"),
        "Codex secret redaction must match apply_patch or it never fires; got {redact:?}"
    );
    // Events with no tool scope must carry no matcher at all.
    assert!(doc["hooks"]["SessionStart"][0].get("matcher").is_none());

    // namespaced codex prompt installed
    assert!(env.home.path().join(".codex/prompts/ws-summary.md").is_file());

    // Codex's native footer uses the same compact information as Claude's.
    let config = std::fs::read_to_string(env.home.path().join(".codex/config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let items = parsed["tui"]["status_line"].as_array().unwrap();
    let items = items.iter().map(|value| value.as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(
        items,
        vec![
            "model-with-reasoning",
            "git-branch",
            "context-used",
            "five-hour-limit",
            "weekly-limit",
        ]
    );
    assert_eq!(parsed["tui"]["status_line_use_colors"].as_bool(), Some(true));
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

    // Claude keeps Claude's tool names. The point of the per-agent matcher is
    // that fixing Codex did not silently change Claude's registration.
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(doc["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    assert_eq!(doc["hooks"]["PostToolUse"][0]["matcher"], "Write|Edit");
    assert!(
        !doc["hooks"]["PostToolUse"][0]["matcher"].as_str().unwrap().contains("apply_patch"),
        "Claude has no apply_patch tool; its matcher must not carry Codex's name"
    );

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
