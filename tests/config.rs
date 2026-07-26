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
        .stdout(predicates::str::contains("prompt_on_launch = false"));
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
    env.cmd().args(["config","set","default_agent","codex"]).assert().success();
    env.cmd().args(["config","set","theme","dark"]).assert().success();
    // first key survives the second write
    env.cmd().args(["config","get","default_agent"]).assert().success()
        .stdout(predicates::str::diff("codex\n"));
    env.cmd().args(["config","get","theme"]).assert().success()
        .stdout(predicates::str::diff("dark\n"));
}
