mod common;
use common::Env;
use predicates::prelude::*;

#[test]
fn limits_lists_captured_windows() {
    let env = Env::new();
    let proj = env.home.path().join("lw");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "lw"]).assert().success();

    // Feed the statusline once to capture limits.
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":88.0,"resets_at":9999999999},"seven_day":{"used_percentage":40.0,"resets_at":9999999999}},"workspace":{"current_dir":"x"}}"#;
    env.cmd()
        .env("WS_WORKSPACE", "lw")
        .env("WS_DIR", &proj)
        .env("NO_COLOR", "1")
        .arg("statusline")
        .write_stdin(sample)
        .assert()
        .success();

    env.cmd()
        .arg("-limits")
        .assert()
        .success()
        .stdout(predicates::str::contains("lw"))
        .stdout(predicates::str::contains("5h 88%"))
        .stdout(predicates::str::contains("wk 40%"));
}

#[test]
fn limits_hides_archived_workspaces() {
    let env = Env::new();
    let proj = env.home.path().join("lw2");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "lw2"]).assert().success();

    // Feed the statusline once to capture limits for the workspace.
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":77.0,"resets_at":9999999999},"seven_day":{"used_percentage":33.0,"resets_at":9999999999}},"workspace":{"current_dir":"x"}}"#;
    env.cmd()
        .env("WS_WORKSPACE", "lw2")
        .env("WS_DIR", &proj)
        .env("NO_COLOR", "1")
        .arg("statusline")
        .write_stdin(sample)
        .assert()
        .success();

    // Sanity check: before archiving, the row shows up.
    env.cmd().arg("-limits").assert().success().stdout(predicates::str::contains("lw2"));

    env.cmd().args(["-archive", "lw2"]).assert().success();

    // After archiving, the stale limits.json must not surface -limits.
    env.cmd().arg("-limits").assert().success().stdout(predicates::str::contains("lw2").not());
}

#[test]
fn limits_empty_message() {
    let env = Env::new();
    env.cmd().arg("-limits").assert().success().stdout(predicates::str::contains("no limit data"));
}

#[test]
fn stop_blocks_with_handoff_directive_when_over_threshold() {
    let env = Env::new();
    let proj = env.home.path().join("hs");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "hs"]).assert().success();

    // Capture a 5h at 90% (over default 85).
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":90.0,"resets_at":9999999999},"seven_day":{"used_percentage":10.0,"resets_at":9999999999}}}"#;
    env.cmd()
        .env("WS_WORKSPACE", "hs")
        .env("WS_DIR", &proj)
        .env("NO_COLOR", "1")
        .arg("statusline")
        .write_stdin(sample)
        .assert()
        .success();

    // Stop now blocks with a handoff directive + sets the guard.
    env.cmd()
        .env("WS_WORKSPACE", "hs")
        .env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"decision\":\"block\""))
        .stdout(predicates::str::contains("handoff"));
    assert!(proj.join(".ws/local/limit-guard").exists());
}

#[test]
fn warn_mode_does_not_block_but_sets_guard() {
    let env = Env::new();
    let proj = env.home.path().join("warn");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "warn"]).assert().success();
    env.cmd().args(["config", "set", "limit_action", "warn"]).assert().success();

    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":95.0,"resets_at":9999999999},"seven_day":{"used_percentage":10.0,"resets_at":9999999999}}}"#;
    env.cmd()
        .env("WS_WORKSPACE", "warn")
        .env("WS_DIR", &proj)
        .arg("statusline")
        .write_stdin(sample)
        .assert()
        .success();

    // warn mode: Stop must never emit a limit BLOCK — but the guard is still set.
    // Assert the decision itself isn't "block" (airtight), not merely the absence
    // of the word "handoff" (which a notebook-reminder block would also lack).
    env.cmd()
        .env("WS_WORKSPACE", "warn")
        .env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"decision\":\"block\"").not());
    assert!(proj.join(".ws/local/limit-guard").exists());
}

#[test]
fn reset_clears_guard() {
    let env = Env::new();
    let proj = env.home.path().join("reset");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "reset"]).assert().success();
    // plant a guard, and capture an UNDER-threshold snapshot
    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(proj.join(".ws/local/limit-guard"), "x").unwrap();
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":5.0,"resets_at":9999999999},"seven_day":{"used_percentage":5.0,"resets_at":9999999999}}}"#;
    env.cmd()
        .env("WS_WORKSPACE", "reset")
        .env("WS_DIR", &proj)
        .arg("statusline")
        .write_stdin(sample)
        .assert()
        .success();

    // a Stop now sees under-threshold → clears the guard
    env.cmd()
        .env("WS_WORKSPACE", "reset")
        .env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success();
    assert!(!proj.join(".ws/local/limit-guard").exists(), "guard should clear on reset");
}

#[test]
fn user_prompt_notes_active_limit_guard() {
    let env = Env::new();
    let proj = env.home.path().join("gd");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "gd"]).assert().success();

    // Manually set the guard marker.
    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(proj.join(".ws/local/limit-guard"), "x").unwrap();

    env.cmd()
        .env("WS_WORKSPACE", "gd")
        .env("WS_DIR", &proj)
        .args(["internal", "user-prompt"])
        .write_stdin(r#"{"prompt":"keep going"}"#)
        .assert()
        .success()
        .stdout(predicates::str::contains("limit guard"))
        .stdout(predicates::str::contains("hookSpecificOutput"));
}
