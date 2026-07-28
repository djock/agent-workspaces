mod common;
use common::Env;
use predicates::prelude::*;

/// Write an executable no-op hook and return its path.
fn hook_script(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn write_hooks_toml(env: &Env, body: &str) -> std::path::PathBuf {
    let p = env.home.path().join(".config/ws/hooks.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

/// The headline promise: one declaration, both agents, each with its own tool
/// vocabulary resolved for it. A hand-written hook cannot do this — the user would
/// have to know that Claude says `Write|Edit|MultiEdit|NotebookEdit` and Codex says
/// `Write|Edit|apply_patch`, and keep both in step forever.
#[test]
fn one_user_hook_registers_for_both_agents_with_each_ones_matcher() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();
    let cmd = hook_script(&env, "my-hook.sh");
    write_hooks_toml(
        &env,
        &format!(
            "[[hook]]\nevent = \"PostToolUse\"\ntool = \"file-write\"\ncommand = {:?}\ntimeout = 30\n",
            cmd.to_str().unwrap()
        ),
    );

    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_CODEX_BIN", &codex)
        .arg("setup")
        .assert()
        .success();

    let claude_cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    let codex_cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home.path().join(".codex/hooks.json")).unwrap(),
    )
    .unwrap();

    let groups = |v: &serde_json::Value| v["hooks"]["PostToolUse"].as_array().unwrap().clone();
    let user_group = |v: &serde_json::Value| {
        groups(v)
            .into_iter()
            .find(|g| {
                g["hooks"][0]["command"].as_str().unwrap_or_default().contains("user-")
            })
            .expect("a user hook group must be registered")
    };

    let c = user_group(&claude_cfg);
    assert_eq!(c["matcher"], "Write|Edit|MultiEdit|NotebookEdit", "Claude's vocabulary");
    assert_eq!(c["hooks"][0]["timeout"], 30, "the declared timeout is honoured");

    let x = user_group(&codex_cfg);
    assert_eq!(x["matcher"], "Write|Edit|apply_patch", "Codex's vocabulary");
}

/// The shim, not the raw command, is what gets registered — deliberately. It lives
/// under ws's hooks dir, so re-running `setup` replaces it instead of appending a
/// second copy, and the user's command inherits ws's env and stdin payload.
#[test]
fn a_user_hook_is_registered_through_a_ws_owned_shim() {
    let env = Env::new();
    let claude = env.fake_claude();
    let cmd = hook_script(&env, "audit.sh");
    write_hooks_toml(
        &env,
        &format!("[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n", cmd.to_str().unwrap()),
    );

    env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();

    let shim = env.home.path().join(".config/ws/hooks/user-stop-audit.sh");
    assert!(shim.is_file(), "a shim is materialised for the user hook");
    let body = std::fs::read_to_string(&shim).unwrap();
    assert!(body.contains("audit.sh"), "the shim execs the user's command: {body}");
    assert!(body.starts_with("#!/bin/sh"), "{body}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&shim).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the agent has to be able to execute it");
    }
}

/// Re-running setup must not accumulate duplicate groups — the reason user hooks
/// go through a ws-owned shim rather than being registered as bare commands.
#[test]
fn setup_is_idempotent_for_user_hooks() {
    let env = Env::new();
    let claude = env.fake_claude();
    let cmd = hook_script(&env, "h.sh");
    write_hooks_toml(
        &env,
        &format!("[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n", cmd.to_str().unwrap()),
    );

    for _ in 0..3 {
        env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();
    }

    let cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    let stop = cfg["hooks"]["Stop"].as_array().unwrap();
    let user_groups = stop
        .iter()
        .filter(|g| g["hooks"][0]["command"].as_str().unwrap_or_default().contains("user-"))
        .count();
    assert_eq!(user_groups, 1, "exactly one user group after three setups: {stop:?}");
}

/// Codex has no `PostToolUseFailure`. Registering it there would write an entry
/// that looks installed and can never fire — the same silent no-op that once
/// disabled secret redaction on Codex. It must be skipped *and said out loud*.
#[test]
fn an_event_codex_cannot_fire_is_skipped_with_a_note() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();
    let cmd = hook_script(&env, "fail-log.sh");
    write_hooks_toml(
        &env,
        &format!(
            "[[hook]]\nevent = \"PostToolUseFailure\"\ncommand = {:?}\n",
            cmd.to_str().unwrap()
        ),
    );

    let out = env
        .cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_CODEX_BIN", &codex)
        .arg("setup")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("codex"), "the skip must be reported: {stderr}");

    let codex_cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home.path().join(".codex/hooks.json")).unwrap(),
    )
    .unwrap();
    assert!(
        codex_cfg["hooks"]["PostToolUseFailure"].is_null(),
        "nothing may be written for an event codex never fires: {codex_cfg}"
    );

    let claude_cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        claude_cfg["hooks"]["PostToolUseFailure"].is_array(),
        "Claude does fire it, so Claude gets it"
    );
}

/// An invalid entry must refuse the whole install rather than half-register. A
/// hook the user believes is running but is not is the failure this file exists
/// to prevent.
#[test]
fn an_invalid_hooks_toml_refuses_setup_and_names_the_problem() {
    let env = Env::new();
    let claude = env.fake_claude();
    write_hooks_toml(&env, "[[hook]]\nevent = \"Stop\"\ncommand = \"/no/such/hook.sh\"\n");

    let out = env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").output().unwrap();
    assert!(!out.status.success(), "setup must refuse");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not exist"), "{err}");
    assert!(err.contains("entry #1"), "the error must locate the entry: {err}");
}

#[test]
fn hooks_check_validates_and_writes_nothing() {
    let env = Env::new();
    let cmd = hook_script(&env, "h.sh");
    let toml = write_hooks_toml(
        &env,
        &format!("[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n", cmd.to_str().unwrap()),
    );
    let before = std::fs::metadata(&toml).unwrap().modified().unwrap();

    env.cmd()
        .args(["hooks", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would register"))
        .stdout(predicate::str::contains("Nothing was written"));

    // No config was created, and hooks.toml itself is untouched.
    assert!(
        !env.home.path().join(".claude/settings.json").exists(),
        "check must not register anything"
    );
    assert_eq!(before, std::fs::metadata(&toml).unwrap().modified().unwrap());
}

#[test]
fn hooks_check_reports_an_invalid_file_without_writing() {
    let env = Env::new();
    write_hooks_toml(&env, "[[hook]]\nevent = \"Nope\"\ncommand = \"/bin/sh\"\n");
    let out = env.cmd().args(["hooks", "check"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Nope"));
}

#[test]
fn hooks_list_shows_builtins_and_user_hooks_per_agent() {
    let env = Env::new();
    let cmd = hook_script(&env, "h.sh");
    write_hooks_toml(
        &env,
        &format!(
            "[[hook]]\nevent = \"PreToolUse\"\ntool = \"shell\"\ncommand = {:?}\n",
            cmd.to_str().unwrap()
        ),
    );

    env.cmd()
        .args(["hooks", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("built-in"))
        .stdout(predicate::str::contains("secret-redact"))
        .stdout(predicate::str::contains("user"))
        // The shell matcher is the same word for both agents, but it must be
        // *resolved* rather than printed as the abstract kind.
        .stdout(predicate::str::contains("Bash"));
}

#[test]
fn hooks_list_works_with_no_user_hooks_at_all() {
    let env = Env::new();
    env.cmd()
        .args(["hooks", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("built-in"));
}

/// A hook the user wired in themselves, outside ws's hooks directory, must survive
/// `ws setup` — ws only replaces its own entries.
#[test]
fn a_foreign_hook_registered_by_hand_survives_setup() {
    let env = Env::new();
    let claude = env.fake_claude();
    let settings = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/opt/mine/own-hook.sh"}]}]}}"#,
    )
    .unwrap();

    env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();

    let body = std::fs::read_to_string(&settings).unwrap();
    assert!(body.contains("own-hook.sh"), "a foreign hook must not be dropped: {body}");
    assert!(body.contains("stop.sh"), "and ws's own is added alongside it");
}

/// `-uninstall` must take the user shims with it, not leave executables in a
/// directory it reports as cleaned.
///
/// Driven through the binary, which also pins the guard that makes this safe to
/// test at all: `-uninstall` refuses to delete a `ws` living under `target/`,
/// because that is a build artifact rather than an installation. Without it this
/// test deleted the executable the rest of the suite runs.
#[test]
fn uninstall_removes_user_hook_shims_and_refuses_to_delete_a_build_artifact() {
    let env = Env::new();
    let claude = env.fake_claude();
    let cmd = hook_script(&env, "h.sh");
    write_hooks_toml(
        &env,
        &format!("[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n", cmd.to_str().unwrap()),
    );
    env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();

    let shim = env.home.path().join(".config/ws/hooks/user-stop-h.sh");
    assert!(shim.is_file(), "precondition: the shim exists");

    let out = env
        .cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .args(["-uninstall", "--force"])
        .output()
        .unwrap();

    assert!(!shim.exists(), "the user shim must be removed too");
    assert!(!out.status.success(), "and the build artifact must not be deleted");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("build artifact"), "the refusal must say why: {err}");
    assert!(
        std::path::Path::new(env!("CARGO_BIN_EXE_ws")).exists(),
        "the test binary itself must survive"
    );
}
