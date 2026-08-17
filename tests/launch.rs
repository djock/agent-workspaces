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

/// `ws <name>` asks before resuming, but only when someone can answer. Without a
/// TTY it must resume silently rather than print a prompt into a pipe and block
/// on a read that never returns — that would hang every scripted launch.
#[test]
fn a_non_interactive_launch_resumes_without_asking() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();
    let out = launch_cmd(&env, &shim).arg("proj").assert().success().get_output().clone();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("Resume previous conversation"), "must not ask: {stderr}");
    assert!(env.argv_log().contains("--resume"), "and must still resume");
}

/// The prompt is a behavior change on every launch, so it has to be switchable
/// off — and with it off the launch resumes exactly as it always did.
#[test]
fn the_resume_prompt_can_be_turned_off() {
    let env = Env::new();
    let shim = env.fake_claude();
    env.cmd().args(["config", "set", "resume_prompt", "false"]).assert().success();

    launch_cmd(&env, &shim).arg("proj").assert().success();
    launch_cmd(&env, &shim).arg("proj").assert().success();

    assert!(env.argv_log().contains("--resume"), "resume_prompt=false still resumes");
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
    assert!(!env.root.join("proj/.ws").exists(), "and nothing was created on disk");
}

/// Task 2 item 1 (claude launch panic). A `state.toml` that has gone corrupt or
/// unreadable since the last launch — a concurrent writer, an external edit,
/// a disk hiccup, it doesn't matter which — used to panic on the second
/// launch: `has_prior_session` and the resume branch each re-read the file,
/// and the resume branch `.unwrap()`ed its read. It must instead degrade to
/// a fresh launch: the whole CLI, not just the command builder, must not
/// crash.
#[test]
fn corrupt_state_toml_degrades_to_fresh_launch_instead_of_crashing() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();

    let state = env.root.join("proj/.ws/local/state.toml");
    assert!(state.is_file(), "first launch recorded a session id");
    std::fs::write(&state, "not toml {{{").unwrap();

    // Must not panic (a non-zero/signal exit) and must fall back to fresh.
    launch_cmd(&env, &shim).arg("proj").assert().success();
    let log = env.argv_log();
    assert!(
        log.matches("--session-id").count() >= 2,
        "the second launch must be fresh (no id to resume from a corrupt file), got: {log}"
    );
    assert!(!log.contains("--resume"), "a corrupt state.toml has no id to resume: {log}");
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
    launch_cmd(&env, &shim).args(["proj", "--agent", "gemini"]).assert().failure().stderr(
        predicates::str::contains("unknown agent")
            .and(predicates::str::contains("claude and codex")),
    );
}

/// The refocused Codex identity path, end to end.
///
/// `resume --last` and its ownership marker are gone. Codex assigns its own
/// session id and reports it in the SessionStart hook payload, so ws records it
/// and resumes **by id**. The test drives the real recording path — `ws internal
/// session-start` with `WS_AGENT=codex`, which is exactly what the installed shim
/// runs — rather than planting state by hand.
#[test]
fn codex_resumes_by_recorded_session_id() {
    let env = Env::new();
    let shim = env.fake_codex();

    // First launch: nothing recorded, so it must be fresh.
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["cxproj", "--agent", "codex"])
        .assert()
        .success();
    let root = env.root.join("cxproj");
    assert!(root.join("AGENTS.md").is_file(), "codex context file generated");
    assert!(
        !env.codex_argv_log().contains("resume"),
        "a first launch has no session to resume: {}",
        env.codex_argv_log()
    );

    // The hook fires and reports the id Codex assigned.
    let id = "019fa430-273a-7fa2-a329-89fab081f383";
    env.cmd()
        .env("WS_WORKSPACE", "cxproj")
        .env("WS_DIR", &root)
        .env("WS_AGENT", "codex")
        .args(["internal", "session-start"])
        .write_stdin(format!(
            r#"{{"session_id":"{id}","source":"startup","cwd":"{}"}}"#,
            root.display()
        ))
        .assert()
        .success();

    let state = std::fs::read_to_string(root.join(".ws/local/state.toml")).unwrap();
    assert!(state.contains(id), "the hook must record the id: {state}");

    // Second launch resumes that exact session.
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["cxproj", "--agent", "codex"])
        .assert()
        .success();
    let log = env.codex_argv_log();
    assert!(log.contains(&format!("resume {id}")), "must resume by id, got: {log}");
    assert!(!log.contains("--last"), "--last is gone: {log}");
}

/// With no recorded id, launch must start fresh and *say so* — this is the state
/// when Codex's hooks have not been trusted via `/hooks`, and it is the honest
/// alternative to `resume --last` guessing at which session was meant.
#[test]
fn codex_without_a_recorded_session_launches_fresh_and_explains() {
    let env = Env::new();
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["loopproj", "--agent", "codex"])
        .assert()
        .success();

    let out = env
        .cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["loopproj", "--agent", "codex"])
        .output()
        .unwrap();
    assert!(out.status.success(), "must degrade to fresh, not crash or hang: {out:?}");
    assert!(
        !env.codex_argv_log().contains("resume"),
        "nothing recorded: must not resume into nothing, got: {}",
        env.codex_argv_log()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no recorded codex session"),
        "the fallback must be visible, not silent: {stderr}"
    );
}

/// `--fresh` must win over a recorded id, or there is no way to deliberately
/// start over.
#[test]
fn fresh_overrides_a_recorded_codex_session() {
    let env = Env::new();
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["fproj", "--agent", "codex"])
        .assert()
        .success();
    let root = env.root.join("fproj");

    env.cmd()
        .env("WS_WORKSPACE", "fproj")
        .env("WS_DIR", &root)
        .env("WS_AGENT", "codex")
        .args(["internal", "session-start"])
        .write_stdin(r#"{"session_id":"aaaa-bbbb","source":"startup"}"#)
        .assert()
        .success();

    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["fproj", "--agent", "codex", "--fresh"])
        .assert()
        .success();
    assert!(
        !env.codex_argv_log().contains("resume"),
        "--fresh must not resume: {}",
        env.codex_argv_log()
    );
}

/// The lineage half: a *different* session id replacing a recorded one is a
/// rotation, and `ws -conversations` must show it. `record_rotation` had zero
/// callers before this, so every `rotated` row described a shape production never
/// wrote.
#[test]
fn a_new_session_id_records_a_rotation_visible_in_conversations() {
    let env = Env::new();
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["rotproj", "--agent", "codex"])
        .assert()
        .success();
    let root = env.root.join("rotproj");

    for (id, source) in [("id-one", "startup"), ("id-two", "startup")] {
        env.cmd()
            .env("WS_WORKSPACE", "rotproj")
            .env("WS_DIR", &root)
            .env("WS_AGENT", "codex")
            .args(["internal", "session-start"])
            .write_stdin(format!(r#"{{"session_id":"{id}","source":"{source}"}}"#))
            .assert()
            .success();
    }

    let tl = std::fs::read_to_string(root.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"kind\":\"rotated\""), "a rotation must be recorded: {tl}");
    assert!(tl.contains("id-one") && tl.contains("id-two"), "both ends of the link: {tl}");

    env.cmd()
        .args(["-conversations", "rotproj"])
        .assert()
        .success()
        .stdout(predicates::str::contains("id-two"));
}

/// Recording the same id twice is a resume, not a rotation — otherwise every
/// `/clear` and compact would manufacture a fake lineage entry.
#[test]
fn re_reporting_the_same_session_id_is_not_a_rotation() {
    let env = Env::new();
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .args(["sameproj", "--agent", "codex"])
        .assert()
        .success();
    let root = env.root.join("sameproj");

    for _ in 0..2 {
        env.cmd()
            .env("WS_WORKSPACE", "sameproj")
            .env("WS_DIR", &root)
            .env("WS_AGENT", "codex")
            .args(["internal", "session-start"])
            .write_stdin(r#"{"session_id":"stable-id","source":"resume"}"#)
            .assert()
            .success();
    }

    let tl = std::fs::read_to_string(root.join(".ws/timeline.jsonl")).unwrap_or_default();
    let rotations = tl.matches("\"kind\":\"rotated\"").count();
    assert_eq!(rotations, 1, "first record is a rotation from nothing; the repeat is not: {tl}");
}

/// Without `WS_AGENT` there is nothing to file an id under, so the hook must do
/// nothing rather than guess an agent.
#[test]
fn the_hook_records_nothing_without_an_agent() {
    let env = Env::new();
    let shim = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &shim).env("WS_NO_EXEC", "1").arg("noagent").assert().success();
    let root = env.root.join("noagent");
    let before = std::fs::read_to_string(root.join(".ws/local/state.toml")).unwrap_or_default();

    env.cmd()
        .env("WS_WORKSPACE", "noagent")
        .env("WS_DIR", &root)
        .args(["internal", "session-start"])
        .write_stdin(r#"{"session_id":"orphan-id","source":"startup"}"#)
        .assert()
        .success();

    let after = std::fs::read_to_string(root.join(".ws/local/state.toml")).unwrap_or_default();
    assert_eq!(before, after, "no WS_AGENT: nothing may be recorded");
    assert!(!after.contains("orphan-id"));
}

#[test]
fn switching_agents_clears_guard_and_records_default() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();

    // first launch with claude (default recorded = claude)
    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_NO_EXEC", "1")
        .arg("switchproj")
        .assert()
        .success();
    let root = env.root.join("switchproj");
    // plant a limit guard as if a threshold had been crossed
    std::fs::create_dir_all(root.join(".ws/local")).unwrap();
    std::fs::write(root.join(".ws/local/limit-guard"), "x").unwrap();

    // switch to codex
    env.cmd()
        .env("WS_CODEX_BIN", &codex)
        .env("WS_NO_EXEC", "1")
        .args(["switchproj", "--agent", "codex"])
        .assert()
        .success();

    // guard cleared on switch; default_agent now codex; AGENTS.md generated
    assert!(!root.join(".ws/local/limit-guard").exists(), "switch clears the limit guard");
    let wt = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
    assert!(wt.contains("default_agent = \"codex\""));
    assert!(root.join("AGENTS.md").is_file());
}

/// The whole point of recording the agent: `-codex` once, then plain `ws <name>`
/// forever. This is the end-to-end version of the two tests below — it asserts
/// on which binary actually ran, not on what a file says.
#[test]
fn a_bare_launch_returns_to_the_last_agent_used() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();
    let shims = |c: &mut assert_cmd::Command| {
        c.env("WS_CLAUDE_BIN", &claude).env("WS_CODEX_BIN", &codex).env("WS_NO_EXEC", "1");
    };

    let mut c = env.cmd();
    shims(&mut c);
    c.args(["stickyproj", "-codex"]).assert().success();

    // No flag this time. Both shims are on offer; the recorded default decides.
    let mut c = env.cmd();
    shims(&mut c);
    c.arg("stickyproj").assert().success();

    assert!(
        env.codex_argv_log().lines().filter(|l| !l.contains("--version")).count() >= 2,
        "the bare launch must reopen codex, got: {}",
        env.codex_argv_log()
    );
    assert!(
        env.argv_log().lines().all(|l| l.contains("--version")),
        "claude must never have been launched, got: {}",
        env.argv_log()
    );
}

/// A workspace whose `workspace.toml` has no `default_agent` — written before the
/// key existed, hand-edited, or restored without it. `switching` is false there
/// (nothing recorded to differ from), so the flag-chosen agent used to be run and
/// then forgotten, and the next bare launch fell back to the global default.
#[test]
fn an_agent_chosen_by_flag_is_recorded_when_nothing_was() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();

    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_NO_EXEC", "1")
        .arg("backfillproj")
        .assert()
        .success();

    // Rewind to the pre-`default_agent` state.
    let wt_path = env.root.join("backfillproj/.ws/workspace.toml");
    let stripped = std::fs::read_to_string(&wt_path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("default_agent"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&wt_path, format!("{stripped}\n")).unwrap();

    env.cmd()
        .env("WS_CODEX_BIN", &codex)
        .env("WS_NO_EXEC", "1")
        .args(["backfillproj", "-codex"])
        .assert()
        .success();

    let wt = std::fs::read_to_string(&wt_path).unwrap();
    assert!(wt.contains("default_agent = \"codex\""), "the backfill must record codex: {wt}");
    // The backfill is a read-modify-write of the identity file; nothing else in
    // it may be lost.
    for key in ["name", "created", "contract_version", "color", "tags"] {
        assert!(wt.contains(key), "{key} must survive the backfill: {wt}");
    }
    // A backfill is not a switch: no `agent-switch` event, and no handoff seeding.
    let timeline = std::fs::read_to_string(env.root.join("backfillproj/.ws/timeline.jsonl"))
        .unwrap_or_default();
    assert!(!timeline.contains("agent-switch"), "a backfill is not a switch: {timeline}");
}

/// The backfill reads the identity file back *after* `open_or_create`, so a
/// workspace `contract::init` just wrote is already covered and must not be
/// rewritten — and, more importantly, must not be mistaken for a switch.
#[test]
fn creating_a_workspace_with_a_flag_records_that_agent_without_a_switch() {
    let env = Env::new();
    let codex = env.fake_codex();

    env.cmd()
        .env("WS_CODEX_BIN", &codex)
        .env("WS_NO_EXEC", "1")
        .args(["newcodexproj", "-codex"])
        .assert()
        .success();

    let root = env.root.join("newcodexproj");
    let wt = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
    assert!(wt.contains("default_agent = \"codex\""));
    let timeline = std::fs::read_to_string(root.join(".ws/timeline.jsonl")).unwrap_or_default();
    assert!(!timeline.contains("agent-switch"), "creation is not a switch: {timeline}");
}

/// The collision menu needs a terminal to answer it. Without one, launching a
/// held workspace must still fail with the old error rather than render a menu
/// into a pipe and block forever on a keypress that cannot arrive.
#[test]
fn a_held_workspace_still_errors_without_a_terminal() {
    let env = Env::new();
    let shim = env.fake_claude();
    launch_cmd(&env, &shim).arg("held").assert().success();

    // Forge a live lock: this test process is definitely alive.
    let lock = env.root.join("held/.ws/local/lock");
    std::fs::write(&lock, format!("pid = {}\n", std::process::id())).unwrap();

    launch_cmd(&env, &shim)
        .arg("held")
        .assert()
        .failure()
        .stderr(predicates::str::contains("in use by pid"));
}

/// `--force` is already an answer, so it must take the lock without stopping to
/// ask — the menu would be a regression for every scripted `--force` launch.
#[test]
fn force_takes_a_held_workspace_without_offering_the_menu() {
    let env = Env::new();
    let shim = env.fake_claude();
    launch_cmd(&env, &shim).arg("held").assert().success();
    let lock = env.root.join("held/.ws/local/lock");
    std::fs::write(&lock, format!("pid = {}\n", std::process::id())).unwrap();

    let out =
        launch_cmd(&env, &shim).args(["held", "--force"]).assert().success().get_output().clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("already open"), "--force must not ask: {stderr}");
}

/// A changelog shaped like the real one: an `[Unreleased]` heading first, then
/// releases newest-first with wrapped bullets.
const CHANGELOG: &str = "\
# Changelog

## [Unreleased]

## [9.9.9] - 2026-08-05

### Added

- **The newest thing.** Body text that belongs to the same
  wrapped bullet.

## [9.9.8] - 2026-08-01

### Fixed

- An older fix.
";

/// Opening a workspace says so when a newer ws has been released — the same
/// nudge `cs` prints on session open — and lists what each newer release
/// brought, read from the published changelog.
#[test]
fn a_launch_reports_a_newer_release_and_its_changelog() {
    let env = Env::new();
    let shim = env.fake_claude();
    let gh = env.fake_gh("v9.9.9", CHANGELOG);

    let out = launch_cmd(&env, &shim)
        .env_remove("WS_NO_UPDATE_CHECK")
        .env("WS_GH_BIN", &gh)
        .arg("proj")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Update available"), "expected the notice, got: {stdout}");
    assert!(stdout.contains("9.9.9"), "expected the new version, got: {stdout}");
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "and the current one: {stdout}");
    assert!(stdout.contains("ws -update"), "and the fix: {stdout}");
    assert!(stdout.contains("The newest thing."), "and the headline: {stdout}");
    assert!(stdout.contains("9.9.8"), "every newer release, not just the latest: {stdout}");
    assert!(stdout.contains("An older fix."), "with its headline too: {stdout}");
    assert!(!stdout.contains("Unreleased"), "but never the unreleased section: {stdout}");
    assert!(!stdout.contains('*'), "markdown must be rendered away: {stdout}");
}

/// Both lookups are cached, so a second launch inside the hour prints the same
/// thing without going near GitHub again.
#[test]
fn a_second_launch_reuses_the_cached_answer() {
    let env = Env::new();
    let shim = env.fake_claude();
    let gh = env.fake_gh("v9.9.9", CHANGELOG);
    let launch = || {
        launch_cmd(&env, &shim)
            .env_remove("WS_NO_UPDATE_CHECK")
            .env("WS_GH_BIN", &gh)
            .arg("proj")
            .assert()
            .success()
            .get_output()
            .clone()
    };

    launch();
    let calls = env.gh_log().lines().count();
    assert_eq!(calls, 2, "first launch asks for the tag and the changelog: {}", env.gh_log());

    let out = launch();
    assert_eq!(env.gh_log().lines().count(), calls, "second launch must ask nothing");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("The newest thing."), "and still print the notes: {stdout}");
}

/// The common case is being current, and a launch that is current must say
/// nothing at all — a line on every open is noise, not information.
#[test]
fn a_launch_on_the_latest_release_is_silent() {
    let env = Env::new();
    let shim = env.fake_claude();
    env.write_update_cache(env!("CARGO_PKG_VERSION"));

    let out = launch_cmd(&env, &shim)
        .env_remove("WS_NO_UPDATE_CHECK")
        .arg("proj")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("Update available"), "must stay quiet: {stdout}");
}

/// No `gh`, no network, no GitHub: the launch must proceed in silence, and must
/// record the failure so the next launch inside the hour does not retry it.
#[test]
fn a_failed_release_lookup_neither_fails_nor_repeats() {
    let env = Env::new();
    let shim = env.fake_claude();

    let out = launch_cmd(&env, &shim)
        .env_remove("WS_NO_UPDATE_CHECK")
        .env("WS_GH_BIN", "/nonexistent/gh")
        .arg("proj")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("Update available"), "must stay quiet: {stdout}");
    assert!(
        env.update_cache().contains('-'),
        "the failure must be recorded: {}",
        env.update_cache()
    );
}

/// A changelog that cannot be fetched must not cost a fetch attempt on every
/// launch, and must not suppress the notice itself — the version nudge is the
/// part that matters.
#[test]
fn an_unreachable_changelog_still_leaves_the_notice() {
    let env = Env::new();
    let shim = env.fake_claude();
    env.write_update_cache("9.9.9");

    let launch = || {
        launch_cmd(&env, &shim)
            .env_remove("WS_NO_UPDATE_CHECK")
            .env("WS_GH_BIN", "/nonexistent/gh")
            .arg("proj")
            .assert()
            .success()
            .get_output()
            .clone()
    };
    let out = launch();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Update available"), "the notice survives: {stdout}");

    let notes = env.home.path().join(".cache/ws/update-notes-9.9.9");
    assert!(notes.is_file(), "the failed fetch is recorded so it is not retried");
    launch();
}

/// Notes are keyed by the version they describe, and only the pending one is
/// worth keeping — otherwise the cache grows a file per release forever.
#[test]
fn notes_for_superseded_versions_are_pruned() {
    let env = Env::new();
    let shim = env.fake_claude();
    let gh = env.fake_gh("v9.9.9", CHANGELOG);
    let stale = env.home.path().join(".cache/ws/update-notes-9.9.0");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "9.9.0\tstale\n").unwrap();

    launch_cmd(&env, &shim)
        .env_remove("WS_NO_UPDATE_CHECK")
        .env("WS_GH_BIN", &gh)
        .arg("proj")
        .assert()
        .success();

    assert!(!stale.exists(), "the superseded notes must be gone");
    assert!(env.home.path().join(".cache/ws/update-notes-9.9.9").is_file());
}

/// The point of the modes is that they are *remembered*: the flag is typed
/// once, and every later plain `ws proj` launches in the same posture. This is
/// the whole feature end to end — flag → `workspace.toml` → the next launch's
/// argv — and none of it is visible from the unit tests, which never write the
/// file or read it back.
#[test]
fn a_chosen_posture_is_remembered_by_later_launches() {
    let env = Env::new();
    let shim = env.fake_claude();
    let ws_toml = env.root.join("proj/.ws/workspace.toml");

    launch_cmd(&env, &shim).args(["proj", "-loco"]).assert().success();
    assert!(
        std::fs::read_to_string(&ws_toml).unwrap().contains("mode = \"loco\""),
        "the posture must be recorded where the next launch will find it"
    );

    // No flag at all: the recorded posture still applies.
    launch_cmd(&env, &shim).arg("proj").assert().success();
    let log = env.argv_log();
    assert_eq!(
        log.matches("--dangerously-skip-permissions").count(),
        2,
        "both launches must be loco, the second without being told: {log}"
    );

    // And the opposite flag replaces it, rather than adding a second posture.
    launch_cmd(&env, &shim).args(["proj", "-sane"]).assert().success();
    let toml = std::fs::read_to_string(&ws_toml).unwrap();
    assert!(toml.contains("mode = \"sane\""), "{toml}");
    assert!(!toml.contains("loco"), "the old posture must be gone: {toml}");

    launch_cmd(&env, &shim).arg("proj").assert().success();
    let log = env.argv_log();
    assert_eq!(
        log.matches("--permission-mode acceptEdits").count(),
        2,
        "sane must stick the same way loco did: {log}"
    );
    assert_eq!(
        log.matches("--dangerously-skip-permissions").count(),
        2,
        "and no later launch may still be bypassing permissions: {log}"
    );
}

/// A workspace nobody has told must launch exactly as it did before the modes
/// existed: no flags, and no `mode` key written into a committed file.
#[test]
fn a_workspace_with_no_recorded_posture_is_untouched() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();

    let log = env.argv_log();
    assert!(!log.contains("--permission-mode"), "{log}");
    assert!(!log.contains("--dangerously-skip-permissions"), "{log}");
    assert!(
        !std::fs::read_to_string(env.root.join("proj/.ws/workspace.toml"))
            .unwrap()
            .contains("mode"),
        "a plain launch must not write a posture into workspace.toml"
    );
}

/// Loco says so on every launch, not only the one that typed the flag: the
/// posture rides along in a committed file, so the launch running without
/// permission checks is usually one that never mentioned loco.
#[test]
fn every_loco_launch_says_so() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).args(["proj", "-loco"]).assert().success();
    let out = launch_cmd(&env, &shim).arg("proj").assert().success().get_output().clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("loco"), "the remembered launch must announce itself: {stderr}");
    assert!(stderr.contains("-sane"), "and say how to undo it: {stderr}");

    // A sane launch is silent about it.
    let out =
        launch_cmd(&env, &shim).args(["proj", "-sane"]).assert().success().get_output().clone();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("loco"));
}

/// `workspace.toml` is hand-editable and git-synced, so it can arrive carrying
/// a `mode` this ws does not know. It must be reported and ignored — never
/// resolved to a posture, since the only guess that could be made is the
/// permissive one.
#[test]
fn an_unrecognized_recorded_posture_is_reported_and_ignored() {
    let env = Env::new();
    let shim = env.fake_claude();
    launch_cmd(&env, &shim).arg("proj").assert().success();

    let ws_toml = env.root.join("proj/.ws/workspace.toml");
    let body = std::fs::read_to_string(&ws_toml).unwrap();
    std::fs::write(&ws_toml, format!("{body}mode = \"yolo\"\n")).unwrap();

    let out = launch_cmd(&env, &shim).arg("proj").assert().success().get_output().clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unrecognized mode"), "{stderr}");
    assert!(!env.argv_log().contains("--dangerously-skip-permissions"), "{}", env.argv_log());
    assert!(!env.argv_log().contains("--permission-mode"), "{}", env.argv_log());
}

/// Codex gets the same two words, spelled its own way — and on a resume, where
/// the flags have to follow `resume <id>`.
#[test]
fn codex_launches_in_the_remembered_posture_too() {
    let env = Env::new();
    let codex = env.fake_codex();

    env.cmd()
        .env("WS_CODEX_BIN", &codex)
        .env("WS_NO_EXEC", "1")
        .args(["proj", "-codex", "-sane"])
        .assert()
        .success();
    env.cmd().env("WS_CODEX_BIN", &codex).env("WS_NO_EXEC", "1").arg("proj").assert().success();

    let log = env.codex_argv_log();
    assert_eq!(
        log.matches("--sandbox workspace-write --ask-for-approval on-request").count(),
        2,
        "codex must get its own spelling, on the remembered launch too: {log}"
    );
}
