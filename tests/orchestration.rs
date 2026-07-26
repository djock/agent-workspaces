mod common;
use common::Env;

/// Safety rule 4: a drain refuses to start while the circuit breaker is open,
/// and a refusal must not consume the queued work — the pending task must
/// still be there afterwards, waiting for `--reset`.
#[test]
fn drain_refuses_while_the_circuit_breaker_is_open() {
    let env = Env::new();
    let proj = env.root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "proj"]).assert().success();
    env.cmd().args(["-queue", "add", "proj", "do", "the", "thing"]).assert().success();

    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(proj.join(".ws/local/queue-circuit-open"), "2026-07-26T00:00:00Z\n").unwrap();

    env.cmd()
        .args(["-queue", "drain", "proj"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--reset"));

    // Refusing to start must not consume the queued work.
    env.cmd()
        .args(["-queue", "list", "proj"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pending"))
        .stdout(predicates::str::contains("do the thing"));
}

/// Safety rule 2: a drain refuses to start if the workspace lock is held by a
/// live process. Refusing must not consume the queued work either.
#[test]
fn drain_refuses_when_a_live_process_holds_the_lock() {
    let env = Env::new();
    let proj = env.root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "proj"]).assert().success();
    env.cmd().args(["-queue", "add", "proj", "do", "the", "thing"]).assert().success();

    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    // This test process's own pid is, by definition, alive.
    let me = std::process::id();
    std::fs::write(
        proj.join(".ws/local/lock"),
        format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n"),
    )
    .unwrap();

    env.cmd()
        .args(["-queue", "drain", "proj"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(me.to_string()));

    env.cmd()
        .args(["-queue", "list", "proj"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pending"))
        .stdout(predicates::str::contains("do the thing"));
}
