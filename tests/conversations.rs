mod common;
use common::Env;

fn adopt(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    env.cmd().current_dir(&p).args(["-adopt", name]).assert().success();
    p
}

/// `ws -conversations` end to end through the real binary.
///
/// Driven by a hand-written timeline so the assertion is about *reading* lineage,
/// independent of how many launches it takes to produce one. The launch-side
/// recording is covered by `src/agents/claude.rs`'s own tests.
#[test]
fn conversations_renders_the_lineage_chain_and_marks_the_live_session() {
    let env = Env::new();
    let proj = adopt(&env, "lin");

    let timeline = proj.join(".ws/timeline.jsonl");
    let lines = [
        r#"{"ts":"2026-07-27T10:00:00Z","kind":"created","actor":"a","agent":"claude"}"#,
        r#"{"ts":"2026-07-27T10:01:00Z","kind":"rotated","actor":"a","agent":"claude","from":null,"to":"1111111111111111","reason":"first"}"#,
        r#"{"ts":"2026-07-27T12:00:00Z","kind":"rotated","actor":"a","agent":"claude","from":"1111111111111111","to":"2222222222222222","reason":"fresh"}"#,
        r#"{"ts":"2026-07-27T13:00:00Z","kind":"agent-switch","actor":"a","from":"claude","to":"codex","handoff":"rotate-1300.md"}"#,
    ];
    // Append, since -adopt may already have written a `created` event.
    let mut body = std::fs::read_to_string(&timeline).unwrap_or_default();
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    std::fs::write(&timeline, body).unwrap();

    // Make the second conversation the live one.
    let state = proj.join(".ws/local/state.toml");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, "[claude]\nsession_id = \"2222222222222222\"\n").unwrap();

    let out =
        env.cmd().args(["-conversations", "lin"]).assert().success().get_output().stdout.clone();
    let out = String::from_utf8(out).unwrap();

    assert!(out.contains("(first)"), "the first conversation is labelled:\n{out}");
    assert!(out.contains("(fresh)"), "and the rotation reason is shown:\n{out}");
    assert!(out.contains("claude → codex"), "the agent switch is a link:\n{out}");
    assert!(out.contains("via rotate-1300.md"), "naming what crossed:\n{out}");

    let current: Vec<&str> = out.lines().filter(|l| l.contains("← current")).collect();
    assert_eq!(current.len(), 1, "exactly one line is the live conversation:\n{out}");
    assert!(current[0].contains("222222222222"), "and it is the live id: {}", current[0]);

    // Non-lineage events must not be rendered as links.
    assert!(!out.contains("created"), "`created` is not a lineage link:\n{out}");
}

#[test]
fn conversations_on_a_workspace_with_no_history_says_so() {
    let env = Env::new();
    adopt(&env, "empty");
    env.cmd()
        .args(["-conversations", "empty"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no conversation history"));
}

/// The timeline is union-merged across checkouts and appended by several writers,
/// so one unreadable line must not cost the whole history.
#[test]
fn conversations_survives_a_corrupt_timeline_line() {
    let env = Env::new();
    let proj = adopt(&env, "corrupt");
    let timeline = proj.join(".ws/timeline.jsonl");
    let mut body = std::fs::read_to_string(&timeline).unwrap_or_default();
    body.push_str("this is not json\n");
    body.push_str(r#"{"ts":"2026-07-27T10:01:00Z","kind":"rotated","agent":"claude","from":null,"to":"abc123def456","reason":"first"}"#);
    body.push('\n');
    std::fs::write(&timeline, body).unwrap();

    env.cmd()
        .args(["-conversations", "corrupt"])
        .assert()
        .success()
        .stdout(predicates::str::contains("(first)"));
}

#[test]
fn conversations_rejects_extra_arguments() {
    let env = Env::new();
    adopt(&env, "x");
    env.cmd().args(["-conversations", "x", "y"]).assert().failure();
}
