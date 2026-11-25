mod common;
use common::Env;

#[test]
fn setup_installs_codex_hooks_and_prompts_when_codex_present() {
    let env = Env::new();
    // Make codex "installed" by pointing WS_CODEX_BIN at a shim that exits 0 on --version.
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .arg("setup")
        .assert()
        .success()
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
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd", "PostToolUse"] {
        assert!(by_event.contains_key(event), "{event} not registered: {doc}");
    }
    assert_eq!(
        doc["hooks"]["SessionStart"][0]["hooks"][0]["type"], "command",
        "per-event entries must be matcher-groups holding a command list"
    );
    assert!(doc["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("session-start.sh"));

    // The matchers must be Codex's tool names, not Claude's. Verified against
    // Codex CLI 0.145.0: shell arrives as `Bash`, a file edit as `apply_patch`.
    // PreToolUse has no built-in hook any more: the only one was a bash audit
    // that wrote a log nothing in ws ever read. `ToolKind::Shell` survives for
    // user-defined hooks, which is where a shell matcher now comes from.
    assert!(doc["hooks"]["PreToolUse"].is_null(), "no built-in PreToolUse hook: {}", doc["hooks"]);
    let redact = doc["hooks"]["PostToolUse"][0]["matcher"].as_str().unwrap();
    assert!(
        redact.contains("apply_patch"),
        "Codex secret redaction must match apply_patch or it never fires; got {redact:?}"
    );
    // Events with no tool scope must carry no matcher at all.
    assert!(doc["hooks"]["SessionStart"][0].get("matcher").is_none());

    // namespaced codex prompt installed
    assert!(env.home.path().join(".codex/prompts/ws-summary.md").is_file());

    // Codex's native footer carries the closest available native counterpart to
    // Claude's ws status line: identity, work, model, and both usage windows.
    let config = std::fs::read_to_string(env.home.path().join(".codex/config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let items = parsed["tui"]["status_line"].as_array().unwrap();
    let items = items.iter().map(|value| value.as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(
        items,
        vec![
            "project-name",
            "git-branch",
            "model-with-reasoning",
            "task-progress",
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
    // PreToolUse has no built-in hook any more: the only one was a bash audit
    // that wrote a log nothing in ws ever read. `ToolKind::Shell` survives for
    // user-defined hooks, which is where a shell matcher now comes from.
    assert!(doc["hooks"]["PreToolUse"].is_null(), "no built-in PreToolUse hook: {}", doc["hooks"]);
    // Every write-side Claude tool, not just Write/Edit: MultiEdit and
    // NotebookEdit put content on disk too, and redaction never fired for them.
    let redact = doc["hooks"]["PostToolUse"][0]["matcher"].as_str().unwrap();
    for tool in ["Write", "Edit", "MultiEdit", "NotebookEdit"] {
        assert!(redact.contains(tool), "redaction must match {tool}; got {redact:?}");
    }
    assert!(
        !redact.contains("apply_patch"),
        "Claude has no apply_patch tool; its matcher must not carry Codex's name"
    );

    // namespaced prompts installed
    assert!(env.home.path().join(".claude/commands/ws/summary.md").is_file());
    assert!(env.home.path().join(".claude/commands/ws/rotate.md").is_file());
}

#[test]
fn setup_registers_statuslines_and_backs_up_prior() {
    let env = Env::new();
    // a pre-existing foreign statusline must be backed up, not lost
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(
        &sp,
        r#"{"statusLine":{"type":"command","command":"/opt/other/other-statusline"}}"#,
    )
    .unwrap();

    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    let cmd = settings["statusLine"]["command"].as_str().unwrap();
    assert!(cmd.ends_with(" statusline"), "statusLine should be ws, got {cmd}");
    assert!(
        settings.get("subagentStatusLine").is_none(),
        "the subagent status line was a second pipeline for one Claude-only pane; \
         setup must not register it any more"
    );

    // the prior command was recorded to the backup file
    let backup = std::fs::read_to_string(env.home.path().join(".config/ws/statusline-backup.json"))
        .unwrap_or_default();
    assert!(backup.contains("other-statusline"), "prior statusline must be backed up");
}

#[test]
fn setup_backs_up_arbitrary_prior_statusline() {
    let env = Env::new();
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(
        &sp,
        r#"{"statusLine":{"type":"command","command":"/usr/local/bin/my-custom-line --flag"}}"#,
    )
    .unwrap();

    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    let backup = std::fs::read_to_string(env.home.path().join(".config/ws/statusline-backup.json"))
        .unwrap_or_default();
    assert!(
        backup.contains("/usr/local/bin/my-custom-line --flag"),
        "prior custom statusline must be backed up verbatim, got: {backup}"
    );
    // and settings.json now points at ws
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    assert!(settings["statusLine"]["command"].as_str().unwrap().ends_with(" statusline"));
}

#[test]
fn setup_backup_merges_and_never_drops_a_prior_original() {
    let env = Env::new();
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();

    // Run 1: only a foreign statusLine present.
    std::fs::write(
        &sp,
        r#"{"statusLine":{"type":"command","command":"/opt/other/other-statusline"}}"#,
    )
    .unwrap();
    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    // Between runs the user points statusLine at something else by hand. ws must
    // keep the *first* original it ever saw rather than overwriting the backup
    // with whatever it happens to find on a later run — losing the only record of
    // what the user had before ws touched anything.
    let mut s: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    s["statusLine"] = serde_json::json!({"type":"command","command":"/opt/other/thing"});
    std::fs::write(&sp, serde_json::to_string(&s).unwrap()).unwrap();

    // Run 2.
    env.cmd().env("WS_CLAUDE_BIN", &shim).arg("setup").assert().success();

    let backup = std::fs::read_to_string(env.home.path().join(".config/ws/statusline-backup.json"))
        .unwrap_or_default();
    assert!(backup.contains("other-statusline"), "run-1 original must survive: {backup}");
}

/// `CODEX_HOME` selects the profile Codex reads hooks from. Setup used to write
/// `~/.codex` regardless; each profile also keeps its own footer backup, and
/// uninstall restores every profile setup touched.
#[test]
fn setup_follows_codex_home_and_uninstall_restores_each_profile() {
    let env = Env::new();
    let shim = env.fake_codex();
    let a = env.home.path().join("profile-a");
    let b = env.home.path().join("profile-b");
    for (dir, footer) in [(&a, "model"), (&b, "current-dir")] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.toml"), format!("[tui]\nstatus_line = [\"{footer}\"]\n"))
            .unwrap();
        std::fs::write(
            dir.join("hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/usr/local/bin/mine"}]}]}}"#,
        )
        .unwrap();
    }
    for dir in [&a, &b] {
        env.cmd()
            .env("WS_CODEX_BIN", &shim)
            .env("CODEX_HOME", dir)
            .arg("setup")
            .assert()
            .success()
            .stdout(predicates::str::contains("profile-"));
    }
    assert!(!env.home.path().join(".codex/hooks.json").exists(), "default profile left alone");
    for dir in [&a, &b] {
        let hooks = std::fs::read_to_string(dir.join("hooks.json")).unwrap();
        assert!(hooks.contains("session-start.sh"), "{}: {hooks}", dir.display());
        assert!(hooks.contains("/usr/local/bin/mine"), "unrelated hook kept: {hooks}");
    }

    // Uninstall from a third, unrelated selection still cleans both.
    let _ = env
        .cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("CODEX_HOME", env.home.path().join("profile-c"))
        .args(["-uninstall", "--force"])
        .assert();
    for (dir, footer) in [(&a, "model"), (&b, "current-dir")] {
        let cfg = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(
            cfg.contains(&format!("[\"{footer}\"]")),
            "{} restored its own: {cfg}",
            dir.display()
        );
        let hooks = std::fs::read_to_string(dir.join("hooks.json")).unwrap();
        assert!(!hooks.contains("session-start.sh"), "ws hooks removed: {hooks}");
        assert!(hooks.contains("/usr/local/bin/mine"), "unrelated hook kept: {hooks}");
    }
}
