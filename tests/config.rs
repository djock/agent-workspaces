mod common;
use common::Env;

#[test]
fn defaults_listed() {
    let env = Env::new();
    env.cmd()
        .args(["config", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("default_agent = claude"))
        .stdout(predicates::str::contains("statusline = true"));
}

/// `prompt_on_launch` and `nerd_fonts` were settable, listed, and read nowhere,
/// so setting them reported success and did nothing. They are gone: neither may
/// reappear in `config list`, and setting one must fail rather than pretend.
#[test]
fn removed_placebo_keys_are_neither_listed_nor_settable() {
    let env = Env::new();
    let listed = env.cmd().args(["config", "list"]).assert().success();
    let out = String::from_utf8(listed.get_output().stdout.clone()).unwrap();
    for key in ["prompt_on_launch", "nerd_fonts"] {
        assert!(!out.contains(key), "{key} must no longer be listed:\n{out}");
        env.cmd().args(["config", "set", key, "true"]).assert().failure();
    }
}

/// The inverse: `statusline` was in the same placebo set but expresses real
/// intent, so it was implemented rather than deleted. Setting it false must
/// actually stop `ws setup` claiming the status bar.
#[test]
fn statusline_false_stops_setup_registering_a_status_line() {
    let env = Env::new();
    let shim = env.fake_claude();
    env.cmd().args(["config", "set", "statusline", "false"]).assert().success();
    env.cmd()
        .env("WS_CLAUDE_BIN", &shim)
        .arg("setup")
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped status line registration"));

    let settings = env.home.path().join(".claude/settings.json");
    let body = std::fs::read_to_string(&settings).unwrap_or_default();
    assert!(
        !body.contains("statusLine"),
        "no status line may be registered when the key is false: {body}"
    );
    // Hooks are a separate concern and must still have been installed.
    assert!(body.contains("session-start.sh"), "hooks must still install: {body}");
}

#[test]
fn set_then_get_roundtrips() {
    let env = Env::new();
    env.cmd().args(["config", "set", "default_agent", "codex"]).assert().success();
    env.cmd()
        .args(["config", "get", "default_agent"])
        .assert()
        .success()
        .stdout(predicates::str::diff("codex\n"));
}

#[test]
fn unknown_key_errors() {
    let env = Env::new();
    env.cmd().args(["config", "get", "bogus"]).assert().failure();
}

#[test]
fn config_set_preserves_other_keys() {
    let env = Env::new();
    env.cmd().args(["config", "set", "default_agent", "codex"]).assert().success();
    env.cmd().args(["config", "set", "theme", "dark"]).assert().success();
    // first key survives the second write
    env.cmd()
        .args(["config", "get", "default_agent"])
        .assert()
        .success()
        .stdout(predicates::str::diff("codex\n"));
    env.cmd()
        .args(["config", "get", "theme"])
        .assert()
        .success()
        .stdout(predicates::str::diff("dark\n"));
}
