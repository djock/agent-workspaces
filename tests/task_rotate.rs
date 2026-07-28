mod common;
use common::Env;
use predicates::prelude::*;

/// A workspace to work in, created without needing an agent shim.
fn adopt(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.root.join(name);
    std::fs::create_dir_all(&p).unwrap();
    env.cmd().current_dir(&p).args(["-adopt", name]).assert().success();
    p
}

// ---------------------------------------------------------------- -task

/// The `/btw` shape: capture from inside a session without naming the workspace.
/// `$WS_WORKSPACE` is what an agent has, so that is what has to work.
#[test]
fn task_add_from_inside_a_session_needs_no_workspace_name() {
    let env = Env::new();
    let p = adopt(&env, "proj");

    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .args(["-task", "add", "rename the misleading flag"])
        .assert()
        .success()
        .stdout(predicate::str::contains("noted for proj"));

    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .args(["-task", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rename the misleading flag"));
}

#[test]
fn task_add_accepts_an_explicit_workspace_name() {
    let env = Env::new();
    adopt(&env, "other");
    let here = adopt(&env, "here");

    env.cmd()
        .env("WS_WORKSPACE", "here")
        .current_dir(&here)
        .args(["-task", "add", "other", "a task for the other one"])
        .assert()
        .success()
        .stdout(predicate::str::contains("noted for other"));

    // It landed in `other`, not in the workspace we ran from.
    env.cmd()
        .args(["-task", "list", "here"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no tasks"));
    env.cmd()
        .args(["-task", "list", "other"])
        .assert()
        .success()
        .stdout(predicate::str::contains("a task for the other one"));
}

/// An unregistered first word is task text, not a mistyped workspace. Otherwise
/// `/ws:task` capturing "other things to check" would silently target a workspace.
#[test]
fn a_first_word_that_is_not_a_workspace_is_part_of_the_task() {
    let env = Env::new();
    let p = adopt(&env, "proj");

    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .args(["-task", "add", "other", "things", "to", "check"])
        .assert()
        .success();

    env.cmd()
        .args(["-task", "list", "proj"])
        .assert()
        .success()
        .stdout(predicate::str::contains("other things to check"));
}

#[test]
fn task_list_numbers_tasks_and_rm_drops_the_named_one() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    for t in ["first", "second", "third"] {
        env.cmd()
            .env("WS_WORKSPACE", "proj")
            .current_dir(&p)
            .args(["-task", "add", t])
            .assert()
            .success();
    }

    env.cmd()
        .args(["-task", "rm", "proj", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dropped task 2"));

    let out = env.cmd().args(["-task", "list", "proj"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("first") && text.contains("third"), "{text}");
    assert!(!text.contains("second"), "the dropped task is gone: {text}");
}

/// An index the user cannot see in the listing must be refused rather than
/// silently dropping the wrong task.
#[test]
fn task_rm_refuses_an_index_that_is_not_listed() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .args(["-task", "add", "only one"])
        .assert()
        .success();

    let out = env.cmd().args(["-task", "rm", "proj", "7"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no task 7"), "{err}");
    assert!(err.contains("1 task"), "the error says how many there are: {err}");
}

#[test]
fn task_rm_rejects_index_zero_rather_than_wrapping() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .args(["-task", "add", "a task"])
        .assert()
        .success();
    // The listing is 1-based; 0 is not a row anyone can see.
    assert!(!env.cmd().args(["-task", "rm", "proj", "0"]).output().unwrap().status.success());
}

#[test]
fn task_list_says_so_when_there_is_nothing() {
    let env = Env::new();
    adopt(&env, "empty");
    env.cmd()
        .args(["-task", "list", "empty"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no tasks"));
}

/// The cap is on the serialized line, and it must refuse rather than append a
/// record that could be torn by a concurrent write.
#[test]
fn task_add_refuses_an_oversized_task() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    let big = "x".repeat(9000);
    let out = env
        .cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .args(["-task", "add", &big])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cap"));
}

#[test]
fn the_task_prompt_is_installed_for_both_agents() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();
    env.cmd()
        .env("WS_CLAUDE_BIN", &claude)
        .env("WS_CODEX_BIN", &codex)
        .arg("setup")
        .assert()
        .success();

    let claude_prompt = env.home.path().join(".claude/commands/ws/task.md");
    let codex_prompt = env.home.path().join(".codex/prompts/ws-task.md");
    assert!(claude_prompt.is_file(), "/ws:task for Claude");
    assert!(codex_prompt.is_file(), "ws-task for Codex");

    let body = std::fs::read_to_string(&claude_prompt).unwrap();
    assert!(body.contains("ws -task add"), "it must tell the agent the command: {body}");
    assert!(
        body.contains("do not start") || body.contains("Do not start"),
        "and that it must not switch to the task: {body}"
    );
}

// ---------------------------------------------------------------- -rotate

#[test]
fn rotate_writes_a_handoff_skeleton_that_handoff_then_finds() {
    let env = Env::new();
    let p = adopt(&env, "proj");

    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .current_dir(&p)
        .arg("-rotate")
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote"))
        .stdout(predicate::str::contains("--handoff"));

    let dir = p.join(".ws/handoffs");
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md"))
        .collect();
    assert_eq!(files.len(), 1, "one handoff: {files:?}");

    let body = std::fs::read_to_string(dir.join(&files[0])).unwrap();
    for heading in ["# Handoff", "What is done", "What is next", "Watch out for"] {
        assert!(body.contains(heading), "missing {heading:?} in:\n{body}");
    }
    assert!(body.contains("**Agent:**"), "the agent is recorded: {body}");
    assert!(body.contains("**By:**"), "and the actor: {body}");
}

/// The filename must sort lexically by time, so `latest_handoff`'s mtime pick and
/// a human's `ls` agree.
#[test]
fn rotate_names_handoffs_so_they_sort_by_time() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    env.cmd().env("WS_WORKSPACE", "proj").current_dir(&p).arg("-rotate").assert().success();

    let name = std::fs::read_dir(p.join(".ws/handoffs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|n| n.ends_with(".md"))
        .unwrap();
    assert!(name.starts_with("20"), "starts with the year: {name}");
    assert!(!name.contains(':'), "no colons — awkward in a shell: {name}");
}

#[test]
fn rotate_records_a_timeline_event() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    env.cmd().env("WS_WORKSPACE", "proj").current_dir(&p).arg("-rotate").assert().success();

    let tl = std::fs::read_to_string(p.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("handoff-written"), "{tl}");
}

#[test]
fn rotate_needs_a_workspace() {
    let env = Env::new();
    let out = env.cmd().current_dir(env.home.path()).arg("-rotate").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not in a workspace"));
}

// ---------------------------------------------------------------- -who

/// `-who` answers "who did what", which is why it reads the timeline rather than
/// ranking `git log` authors — the git version could not say what anyone did.
#[test]
fn who_summarises_the_timeline_per_actor() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    let tl = p.join(".ws/timeline.jsonl");
    std::fs::write(
        &tl,
        "{\"ts\":\"2026-07-01T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"alice\"}\n\
         {\"ts\":\"2026-07-01T01:00:00Z\",\"kind\":\"rotated\",\"actor\":\"alice\"}\n\
         {\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"bob\"}\n",
    )
    .unwrap();

    let out = env.cmd().args(["-who", "proj"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("alice"), "{text}");
    assert!(text.contains("bob"), "{text}");
    assert!(text.contains("opened"), "the kinds are shown: {text}");
    assert!(text.contains("rotated"), "{text}");
    // Busiest first.
    assert!(
        text.find("alice").unwrap() < text.find("bob").unwrap(),
        "alice has more events: {text}"
    );
}

#[test]
fn who_falls_back_when_there_is_no_timeline_yet() {
    let env = Env::new();
    let p = adopt(&env, "fresh");
    let tl = p.join(".ws/timeline.jsonl");
    let _ = std::fs::remove_file(&tl);

    let out = env.cmd().args(["-who", "fresh"]).output().unwrap();
    assert!(out.status.success(), "must not fail just because nothing happened yet");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("no timeline yet") || text.contains("no recorded activity"),
        "{text}"
    );
}
