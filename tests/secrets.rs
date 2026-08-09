mod common;
use common::Env;
use predicates::prelude::*;

/// A command against the *keyring* backend.
///
/// `Env::cmd` points HOME at a temp dir, which on macOS means there is no
/// `login.keychain-db` and every vault call fails with "a default keychain
/// could not be found" — so a keyring test run that way proves nothing about
/// the real vault. These tests put the real HOME back while keeping
/// XDG_CONFIG_HOME isolated: the name index (the part `ws` owns) still lands in
/// the temp dir, and only the credential itself touches the login keychain,
/// under a service name unique to this process and removed before the test ends.
fn kc(env: &Env, ws: &str) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_SECRETS_BACKEND", "keyring").env("WS_WORKSPACE", ws);
    if let Ok(home) = std::env::var("HOME") {
        c.env("HOME", home);
    }
    c
}

fn sc(env: &Env) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "testpw")
        .env("WS_WORKSPACE", "sw");
    c
}

#[test]
fn set_from_stdin_get_list_rm() {
    let env = Env::new();
    // set prints ONLY the confirmation — never the value (security invariant).
    sc(&env).args(["-secrets", "set", "API_KEY"]).write_stdin("s3cr3t\n").assert().success()
        .stdout(predicates::str::diff("stored API_KEY\n"));
    sc(&env).args(["-secrets", "get", "API_KEY"]).assert().success()
        .stdout(predicates::str::diff("s3cr3t\n"));
    sc(&env).args(["-secrets", "list"]).assert().success()
        .stdout(predicates::str::contains("API_KEY"))
        .stdout(predicates::str::contains("s3cr3t").not());  // list never shows values
    sc(&env).args(["-secrets", "rm", "API_KEY"]).assert().success();
    sc(&env).args(["-secrets", "get", "API_KEY"]).assert().failure(); // absent
}

#[test]
fn export_and_backend() {
    let env = Env::new();
    sc(&env).args(["-secrets", "set", "TOKEN"]).write_stdin("abc").assert().success();
    sc(&env).args(["-secrets", "export"]).assert().success()
        .stdout(predicates::str::contains("export TOKEN='abc'"));
    sc(&env).args(["-secrets", "backend"]).assert().success()
        .stdout(predicates::str::contains("file"));
}

#[test]
fn purge_refuses_without_tty() {
    let env = Env::new();
    sc(&env).args(["-secrets", "set", "K"]).write_stdin("v").assert().success();
    // assert_cmd runs with non-TTY stdin → purge must refuse (never silently wipe).
    sc(&env).args(["-secrets", "purge"]).assert().failure()
        .stderr(predicates::str::contains("without a TTY"));
    // the secret survived the refused purge
    sc(&env).args(["-secrets", "get", "K"]).assert().success()
        .stdout(predicates::str::diff("v\n"));
}

/// The keyring backend must survive process exit.
///
/// Every other test in this file pins `WS_SECRETS_BACKEND=file`, which is why
/// the suite was green while `-secrets get` was broken for every real user:
/// `keyring` was declared with no platform feature, so it fell back to an
/// in-memory mock. `set` returned Ok, the name landed in the on-disk index so
/// `list` kept showing it, and the value evaporated when the process exited.
///
/// Catching that requires *two* processes — in-process the mock is perfectly
/// convincing. Each `assert_cmd` call is its own process, so the `get` below is
/// the real assertion.
///
/// Environments with no OS vault at all (headless CI) are skipped rather than
/// failed, and the two cases are distinguishable: with no vault `set` fails,
/// whereas the mock makes `set` *succeed*. So a successful `set` followed by a
/// failing `get` is exactly the regression, and is never a skip.
#[test]
fn keyring_secrets_survive_process_exit() {
    let env = Env::new();
    // The OS vault is machine-global (unlike the temp HOME), so the service
    // name must not collide with a real workspace or a concurrent test run.
    let ws = format!("wskrt{}", std::process::id());
    let stored = kc(&env, &ws).args(["-secrets", "set", "VAULTED"]).write_stdin("v4ult").ok();
    if stored.is_err() {
        eprintln!("skipping {ws}: no OS credential vault available here");
        return;
    }

    // Separate process: a mock store has already forgotten the value.
    kc(&env, &ws)
        .args(["-secrets", "get", "VAULTED"])
        .assert()
        .success()
        .stdout(predicates::str::diff("v4ult\n"));

    kc(&env, &ws).args(["-secrets", "rm", "VAULTED"]).assert().success();
    kc(&env, &ws).args(["-secrets", "get", "VAULTED"]).assert().failure();
}

/// `help` used to be rejected as an unknown subcommand, which left the list of
/// subcommands discoverable only by reading the source. It must also work with
/// no workspace and no master password — hence the bare `env.cmd()` here, with
/// neither `WS_WORKSPACE` nor `WS_SECRETS_PASSWORD` set: if `help` ever starts
/// opening the store again, the file backend blocks on a password prompt.
#[test]
fn help_lists_subcommands_without_a_workspace_or_password() {
    let env = Env::new();
    for args in [vec!["-secrets", "help"], vec!["-secrets"]] {
        env.cmd()
            .env("WS_SECRETS_BACKEND", "file")
            .current_dir(env.home.path())
            .args(&args)
            .assert()
            .success()
            .stdout(predicates::str::contains("usage: ws -secrets"))
            .stdout(predicates::str::contains("restore <file>"));
    }
    // An unknown subcommand still fails, but now says what the options are.
    sc(&env).args(["-secrets", "frobnicate"]).assert().failure()
        .stderr(predicates::str::contains("unknown -secrets subcommand: frobnicate"))
        .stderr(predicates::str::contains("usage: ws -secrets"));
}

/// A name the store lists but cannot resolve is the fingerprint of the pre-0.6.1
/// mock-keyring data loss. It must not be reported as a plain "no such secret",
/// which reads as a typo and sends people looking for a secret that is gone.
#[test]
fn a_listed_but_unresolvable_name_reports_data_loss_not_a_typo() {
    let env = Env::new();
    // Forge the keyring backend's on-disk name index with a name the OS vault
    // has never heard of — exactly the state the mock store left behind.
    let dir = env.home.path().join(".config/ws/secrets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sw.keyring-index"), "GHOST\n").unwrap();

    kc(&env, "sw").args(["-secrets", "get", "GHOST"]).assert().failure()
        .stderr(predicates::str::contains("value is missing"))
        .stderr(predicates::str::contains("cannot be recovered"))
        .stderr(predicates::str::contains("ws -secrets set GHOST"));

    // A name that was never listed keeps the plain message.
    kc(&env, "sw").args(["-secrets", "get", "NEVERSTORED"]).assert().failure()
        .stderr(predicates::str::contains("no such secret: NEVERSTORED"));
}

#[test]
fn secrets_outside_workspace_errors() {
    let env = Env::new();
    // no WS_WORKSPACE and cwd isn't a workspace (force cwd to a dir with no
    // `.ws`, since the dev repo checkout itself may have a stray `.ws/` from
    // manual testing).
    env.cmd().env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","x")
        .current_dir(env.home.path())
        .args(["-secrets","list"]).assert().failure()
        .stderr(predicates::str::contains("not in a workspace"));
}
