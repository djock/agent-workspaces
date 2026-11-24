//! agy (Antigravity CLI) as a third agent: launch and resume by id, its hooks
//! file, and the hook handlers answering in agy's own dialect. The payloads are
//! trimmed from live agy 1.5.16 captures.

mod common;
use common::Env;

fn json(out: &[u8]) -> serde_json::Value {
    serde_json::from_slice(out)
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {}", String::from_utf8_lossy(out)))
}

#[test]
fn agy_launches_records_its_conversation_and_resumes_it_by_id() {
    let env = Env::new();
    let shim = env.fake_codex();
    // `gemini` is accepted as a name for agy.
    env.cmd()
        .env("WS_AGY_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["gproj", "--agent", "gemini"])
        .assert()
        .success();
    let root = env.root.join("gproj");
    assert!(root.join("AGENTS.md").is_file(), "agy reads AGENTS.md");
    assert!(!env.codex_argv_log().contains("--conversation"), "{}", env.codex_argv_log());
    let toml = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
    assert!(toml.contains("agy"), "recorded as agy, not gemini: {toml}");

    let id = "291f5260-1f99-4e1a-a73a-fd20342d1179";
    let out = env
        .cmd()
        .env("WS_WORKSPACE", "gproj")
        .env("WS_DIR", &root)
        .env("WS_AGENT", "agy")
        .args(["internal", "session-start"])
        .write_stdin(format!(
            r#"{{"conversationId":"{id}","cwd":"{}","hookEventName":"SessionStart","lastUserInput":"hi"}}"#,
            root.display()
        ))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let ctx = v["injectSteps"][0]["systemMessage"]["systemMessage"].as_str().unwrap();
    assert!(ctx.contains("gproj"), "the workspace context is injected: {ctx}");
    assert!(v.get("hookSpecificOutput").is_none(), "no Claude shape for agy: {v}");

    let state = std::fs::read_to_string(root.join(".ws/local/state.toml")).unwrap();
    assert!(state.contains(id), "{state}");

    env.cmd()
        .env("WS_AGY_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["gproj", "-agy"])
        .assert()
        .success();
    assert!(
        env.codex_argv_log().contains(&format!("--conversation {id}")),
        "{}",
        env.codex_argv_log()
    );
}

/// agy reads each hook's stdout as JSON, so a handler with nothing to say still
/// answers `{}` instead of printing nothing.
#[test]
fn agy_hooks_with_nothing_to_say_answer_an_empty_object() {
    let env = Env::new();
    for handler in ["stop", "secret-redact", "user-prompt"] {
        let out = env
            .cmd()
            .args(["internal", handler])
            .write_stdin(r#"{"conversationId":"c","hookEventName":"Stop","cwd":"/nowhere"}"#)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(json(&out), serde_json::json!({}), "{handler}");
    }
}

#[test]
fn setup_writes_a_named_ws_entry_into_agys_hooks_file_and_uninstall_removes_it() {
    let env = Env::new();
    let shim = env.fake_codex();
    let path = env.home.path().join(".gemini/config/hooks.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"mine":{"Stop":[{"command":"./mine.sh"}]}}"#).unwrap();

    env.cmd().env("WS_AGY_BIN", &shim).arg("setup").assert().success();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("hooks").is_none(), "agy rejects a `hooks` wrapper: {v}");
    assert_eq!(v["mine"]["Stop"][0]["command"], "./mine.sh", "the user's hook survives");
    let ws = &v["ws"];
    assert!(ws["SessionStart"][0]["command"].as_str().unwrap().contains("session-start.sh"));
    assert!(ws["PreInvocation"][0]["command"].as_str().unwrap().contains("user-prompt.sh"));
    assert!(ws["Stop"][0]["command"].as_str().unwrap().contains("stop.sh"));
    assert!(ws.get("UserPromptSubmit").is_none() && ws.get("SessionEnd").is_none(), "{ws}");
    assert!(ws.get("PreToolUse").is_none(), "a PreToolUse hook fails closed on agy: {ws}");
    let post = &ws["PostToolUse"][0];
    assert!(post["matcher"].as_str().unwrap().contains("write_file"), "{post}");
    assert!(post["hooks"][0]["command"].as_str().unwrap().contains("secret-redact.sh"));

    let doctor = env.cmd().env("WS_AGY_BIN", &shim).arg("-doctor").output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert!(text.contains("agy") && text.contains("ws hooks registered"), "{text}");

    let _ = env.cmd().env("WS_AGY_BIN", &shim).args(["-uninstall", "--force"]).assert();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(v.get("ws").is_none(), "{v}");
    assert_eq!(v["mine"]["Stop"][0]["command"], "./mine.sh");
}
