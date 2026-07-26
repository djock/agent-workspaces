mod common;
use common::Env;
use predicates::prelude::*;

fn adopt_ws(env: &Env, name: &str) -> std::path::PathBuf {
    let proj = env.home.path().join(name);
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", name]).assert().success();
    proj
}

#[test]
fn session_start_injects_context_and_logs_opened() {
    let env = Env::new();
    let proj = adopt_ws(&env, "proj");

    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .env("WS_DIR", &proj)
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup","cwd":"x"}"#)
        .assert()
        .success()
        .stdout(predicates::str::contains("hookSpecificOutput"))
        .stdout(predicates::str::contains("proj"))
        .stdout(predicates::str::contains("captured from the first prompt").not());

    // timeline recorded an "opened" event (plus the "created" from adopt)
    let tl = std::fs::read_to_string(proj.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"opened\""));
}

#[test]
fn session_start_noop_outside_workspace() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup"}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn session_start_noop_in_subagent() {
    let env = Env::new();
    let proj = adopt_ws(&env, "sub");
    env.cmd()
        .env("WS_WORKSPACE", "sub")
        .env("WS_DIR", &proj)
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup","agent_id":"abc"}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn hook_payload_extracts_field() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "hook-payload", "source"])
        .write_stdin(r#"{"source":"startup"}"#)
        .assert()
        .success()
        .stdout(predicates::str::diff("startup\n"));
}

#[test]
fn user_prompt_captures_objective() {
    let env = Env::new();
    let proj = adopt_ws(&env, "obj");

    // README starts with the placeholder
    let readme = proj.join(".ws/README.md");
    assert!(std::fs::read_to_string(&readme).unwrap().contains("_(captured from the first prompt)_"));

    env.cmd()
        .env("WS_WORKSPACE", "obj")
        .env("WS_DIR", &proj)
        .args(["internal", "user-prompt"])
        .write_stdin(r#"{"prompt":"Implement the widget parser"}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    let after = std::fs::read_to_string(&readme).unwrap();
    assert!(after.contains("Implement the widget parser"));
    assert!(!after.contains("_(captured from the first prompt)_"));
}

#[test]
fn session_start_injects_captured_objective() {
    let env = Env::new();
    let proj = adopt_ws(&env, "cap");
    env.cmd()
        .env("WS_WORKSPACE", "cap").env("WS_DIR", &proj)
        .args(["internal", "user-prompt"])
        .write_stdin(r#"{"prompt":"Ship the invoice exporter"}"#)
        .assert().success();
    env.cmd()
        .env("WS_WORKSPACE", "cap").env("WS_DIR", &proj)
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup"}"#)
        .assert()
        .success()
        .stdout(predicates::str::contains("Ship the invoice exporter"))
        .stdout(predicates::str::contains("captured from the first prompt").not());
}

#[test]
fn unknown_internal_handler_is_silent_success() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "does-not-exist"])
        .write_stdin("{}")
        .assert()
        .success();
}

#[test]
fn stop_reminds_then_cools_down() {
    let env = Env::new();
    let proj = adopt_ws(&env, "nb");

    // Age the notebook file well past the cooldown so the reminder fires.
    // (Set mtime to the epoch via `touch -t`.)
    let nb = std::fs::read_dir(proj.join(".ws/notebook"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("notebook."))
        .unwrap();
    std::process::Command::new("touch").args(["-t", "200001010000"]).arg(&nb).status().unwrap();

    // First stop → block with a reminder
    env.cmd()
        .env("WS_WORKSPACE", "nb").env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"decision\":\"block\""))
        .stdout(predicates::str::contains("notebook"));

    // Second stop immediately after → cooldown holds → allow with no output.
    // Stop's only valid decision is "block"; "approve" is invalid JSON for
    // this event.
    env.cmd()
        .env("WS_WORKSPACE", "nb").env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn stop_allows_silently_outside_workspace() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn bash_audit_logs_command() {
    let env = Env::new();
    let proj = adopt_ws(&env, "aud");
    env.cmd()
        .env("WS_WORKSPACE", "aud").env("WS_DIR", &proj)
        .args(["internal", "bash-audit"])
        .write_stdin(r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    let log = std::fs::read_to_string(proj.join(".ws/local/log/session.log")).unwrap();
    assert!(log.contains("BASH: echo hi"));
}

#[test]
fn bash_audit_ignores_non_bash() {
    let env = Env::new();
    let proj = adopt_ws(&env, "aud2");
    let log_path = proj.join(".ws/local/log/session.log");

    // Establish a baseline: one real Bash command logged.
    env.cmd()
        .env("WS_WORKSPACE", "aud2").env("WS_DIR", &proj)
        .args(["internal", "bash-audit"])
        .write_stdin(r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#)
        .assert()
        .success();
    let log = std::fs::read_to_string(&log_path).unwrap();
    let bash_lines_before = log.lines().filter(|l| l.contains("BASH")).count();
    assert_eq!(bash_lines_before, 1);

    // A non-Bash tool must not add another BASH line.
    env.cmd()
        .env("WS_WORKSPACE", "aud2").env("WS_DIR", &proj)
        .args(["internal", "bash-audit"])
        .write_stdin(r#"{"tool_name":"Edit","tool_input":{"command":""}}"#)
        .assert()
        .success();
    let log = std::fs::read_to_string(&log_path).unwrap();
    let bash_lines_after = log.lines().filter(|l| l.contains("BASH")).count();
    assert_eq!(bash_lines_after, 1, "non-Bash tool must not add a BASH log line");
}

#[test]
fn session_end_records_closed() {
    let env = Env::new();
    let proj = adopt_ws(&env, "end");
    env.cmd()
        .env("WS_WORKSPACE", "end").env("WS_DIR", &proj)
        .args(["internal", "session-end"])
        .write_stdin(r#"{"reason":"exit"}"#)
        .assert()
        .success();
    let tl = std::fs::read_to_string(proj.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"closed\""));
}
