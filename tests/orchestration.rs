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
///
/// I2: a mutation test showed the previous version of this assertion
/// (stderr contains the pid) passes even with the entire `live_pid_checked`
/// check deleted from `src/drain.rs::run` — `lock::acquire`'s own liveness
/// check bails a few lines later with a message that *also* contains the
/// pid ("workspace is in use by pid {pid} (another terminal)..."), so the
/// test proved "something refuses", not that the drain's own explicit
/// pre-check ran. Asserting on `"not starting a drain"` — wording that only
/// `drain::run`'s `live_pid_checked` branch produces — closes that hole.
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
        .stderr(predicates::str::contains(me.to_string()))
        .stderr(predicates::str::contains("not starting a drain"));

    env.cmd()
        .args(["-queue", "list", "proj"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pending"))
        .stdout(predicates::str::contains("do the thing"));
}

/// I2 continued: `live_pid_checked` differs from the display-only `live_pid`
/// in exactly one case — an *unreadable* lock file. `live_pid` folds a read
/// error into `None` (not live); `live_pid_checked` surfaces the error and
/// `drain::run` propagates it, refusing to start. A readable-lock test can
/// never exercise this branch, which is why it's added as its own case
/// rather than folded into the test above.
#[test]
#[cfg(unix)]
fn drain_refuses_when_the_lock_file_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    // Running as root defeats file permissions — the read would succeed
    // regardless, same caveat as lock.rs's own unreadable-lock-file test.
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if uid == "0" {
        return;
    }

    let env = Env::new();
    let proj = env.root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "proj"]).assert().success();
    env.cmd().args(["-queue", "add", "proj", "do", "the", "thing"]).assert().success();

    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    let lock = proj.join(".ws/local/lock");
    let me = std::process::id();
    let original = format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n");
    std::fs::write(&lock, &original).unwrap();

    // Write-only, no read — mirrors lock.rs's
    // `an_unreadable_lock_file_is_never_reclaimed` fixture.
    let mut perms = std::fs::metadata(&lock).unwrap().permissions();
    perms.set_mode(0o200);
    std::fs::set_permissions(&lock, perms).unwrap();

    let result = env.cmd().args(["-queue", "drain", "proj"]).assert().failure();

    // Restore permissions before any further assertions/teardown touch the file.
    let mut perms = std::fs::metadata(&lock).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&lock, perms).unwrap();

    let _ = result; // failure() already asserted the exit code

    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        original,
        "an unreadable lock must not be reclaimed or overwritten"
    );
    env.cmd()
        .args(["-queue", "list", "proj"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pending"))
        .stdout(predicates::str::contains("do the thing"));
}
