//! `ws -msg` end to end: two workspaces, one message, and the agent on the
//! other end being told about it.

mod common;
use common::Env;
use predicates::prelude::*;

fn adopt(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    env.cmd().current_dir(&p).args(["-adopt", name]).assert().success();
    p
}

/// The `UserPromptSubmit` hook, as the agent runs it.
fn prompt_hook(env: &Env, name: &str, root: &std::path::Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_WORKSPACE", name)
        .env("WS_DIR", root)
        .env("WS_AGENT", "claude")
        .current_dir(root)
        .args(["internal", "user-prompt"])
        .write_stdin(r#"{"prompt":"carry on"}"#);
    c
}

#[test]
fn a_message_reaches_the_other_workspace_and_clears_when_read() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    let b = adopt(&env, "beta");

    env.cmd()
        .current_dir(&a)
        .args(["-msg", "beta", "the parser is ready for you"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sent to beta"));

    // Unread until read.
    env.cmd()
        .current_dir(&b)
        .arg("-msg")
        .assert()
        .success()
        .stdout(predicate::str::contains("the parser is ready for you"))
        .stdout(predicate::str::contains("alpha"));

    // Reading clears it, and the history keeps it.
    env.cmd()
        .current_dir(&b)
        .arg("-msg")
        .assert()
        .success()
        .stdout(predicate::str::contains("no unread mail"));
    env.cmd()
        .current_dir(&b)
        .args(["-msg", "log"])
        .assert()
        .success()
        .stdout(predicate::str::contains("the parser is ready for you"));
}

/// The point of delivering at all: the receiving agent is told, on its next
/// prompt, without anyone having to run a command.
#[test]
fn the_receiving_agent_is_told_on_its_next_prompt_until_it_reads() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    let b = adopt(&env, "beta");
    env.cmd().current_dir(&a).args(["-msg", "beta", "look at the 429 retries"]).assert().success();

    // Every prompt, not once: a message that arrives mid-turn would otherwise be
    // announced at a moment nobody is looking and never again.
    for _ in 0..2 {
        prompt_hook(&env, "beta", &b)
            .assert()
            .success()
            .stdout(predicate::str::contains("look at the 429 retries"))
            .stdout(predicate::str::contains("unread message"));
    }

    env.cmd().current_dir(&b).arg("-msg").assert().success();

    prompt_hook(&env, "beta", &b)
        .assert()
        .success()
        .stdout(predicate::str::contains("unread message").not());
}

/// A body that size does not belong in argv, where every `ps` on the machine can
/// read it and the platform caps its length.
#[test]
fn a_body_can_be_read_from_stdin() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    let b = adopt(&env, "beta");
    let big = format!("handoff:\n{}", "context ".repeat(500));

    env.cmd()
        .current_dir(&a)
        .args(["-msg", "beta", "-"])
        .write_stdin(big.clone())
        .assert()
        .success();

    env.cmd()
        .current_dir(&b)
        .arg("-msg")
        .assert()
        .success()
        .stdout(predicate::str::contains("handoff:"));
}

/// `--kind task` lands in the recipient's queue as well as its mailbox: reading
/// a message consumes it, and work that survives being read is what a queue is.
#[test]
fn a_task_message_is_queued_where_the_work_is() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    let b = adopt(&env, "beta");

    env.cmd()
        .current_dir(&a)
        .args(["-msg", "beta", "regenerate the fixtures", "--kind", "task"])
        .assert()
        .success();

    env.cmd()
        .current_dir(&b)
        .args(["-task", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("regenerate the fixtures"));
    env.cmd()
        .current_dir(&b)
        .arg("-msg")
        .assert()
        .success()
        .stdout(predicate::str::contains("[task]"));
}

#[test]
fn a_reply_carries_the_thread_it_answers() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    let b = adopt(&env, "beta");

    let out = env.cmd().current_dir(&a).args(["-msg", "beta", "question?"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let thread = stdout.split("thread ").nth(1).unwrap().trim().trim_end_matches(')').to_string();

    env.cmd()
        .current_dir(&b)
        .args(["-msg", "alpha", "answer!", "--reply", &thread])
        .assert()
        .success()
        .stdout(predicate::str::contains(&thread));
}

#[test]
fn sending_to_a_workspace_that_does_not_exist_is_an_error() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    env.cmd()
        .current_dir(&a)
        .args(["-msg", "nowhere", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no such workspace"));
}

#[test]
fn a_workspace_cannot_send_to_itself() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    env.cmd()
        .current_dir(&a)
        .args(["-msg", "alpha", "hello me"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("this workspace"));
}

#[test]
fn an_empty_body_is_refused() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    adopt(&env, "beta");
    env.cmd()
        .current_dir(&a)
        .args(["-msg", "beta", "   "])
        .assert()
        .failure()
        .stderr(predicate::str::contains("empty"));
}

/// Mail is machine-local by design: a message is addressed to a running agent on
/// this machine, not to whoever clones the repository next month.
#[test]
fn mail_lands_under_the_gitignored_local_directory() {
    let env = Env::new();
    let a = adopt(&env, "alpha");
    let b = adopt(&env, "beta");
    env.cmd().current_dir(&a).args(["-msg", "beta", "hello"]).assert().success();

    let mailbox = b.join(".ws/local/mail/new");
    assert!(mailbox.is_dir(), "mail belongs under .ws/local/");
    assert_eq!(std::fs::read_dir(&mailbox).unwrap().count(), 1);
}
