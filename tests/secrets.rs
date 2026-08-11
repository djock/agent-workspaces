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

/// Is there a *usable* OS vault here?
///
/// Not every environment has one, and the two ways of lacking it look nothing
/// alike: macOS with a temp HOME reports "a default keychain could not be
/// found", and a headless Linux runner reports
/// `org.freedesktop.secrets was not provided by any .service files`. Rather
/// than match on either message, ask the binary to store something and see.
///
/// The mock store this whole file exists to guard against would pass this
/// probe — `set` always succeeds there. That is deliberate: the probe must not
/// be what decides the mock is acceptable, so callers still assert the
/// cross-process behaviour that only a real vault can satisfy.
fn vault_works(env: &Env, ws: &str) -> bool {
    let ok = kc(env, ws).args(["-secrets", "set", "__PROBE__"]).write_stdin("p").ok().is_ok();
    if ok {
        kc(env, ws).args(["-secrets", "rm", "__PROBE__"]).ok().ok();
    } else {
        eprintln!("skipping: no usable OS credential vault in this environment");
    }
    ok
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
    sc(&env)
        .args(["-secrets", "set", "API_KEY"])
        .write_stdin("s3cr3t\n")
        .assert()
        .success()
        .stdout(predicates::str::diff("stored API_KEY\n"));
    sc(&env)
        .args(["-secrets", "get", "API_KEY"])
        .assert()
        .success()
        .stdout(predicates::str::diff("s3cr3t\n"));
    sc(&env)
        .args(["-secrets", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("API_KEY"))
        .stdout(predicates::str::contains("s3cr3t").not()); // list never shows values
    sc(&env).args(["-secrets", "rm", "API_KEY"]).assert().success();
    sc(&env).args(["-secrets", "get", "API_KEY"]).assert().failure(); // absent
}

#[test]
fn export_and_backend() {
    let env = Env::new();
    sc(&env).args(["-secrets", "set", "TOKEN"]).write_stdin("abc").assert().success();
    sc(&env)
        .args(["-secrets", "export"])
        .assert()
        .success()
        .stdout(predicates::str::contains("export TOKEN='abc'"));
    sc(&env)
        .args(["-secrets", "backend"])
        .assert()
        .success()
        .stdout(predicates::str::contains("file"));
}

/// `backend` reports configuration; it decrypts nothing, so it must never
/// authenticate.
///
/// It used to, because `secrets()` opened the store before dispatching and
/// `open` builds the `FileStore` password eagerly — so asking *which* backend
/// was configured prompted for the file backend's master password, and with no
/// terminal to prompt on it died with a raw `Device not configured (os error
/// 6)` from rpassword's `/dev/tty`.
#[test]
fn backend_reports_the_file_backend_without_a_password() {
    let env = Env::new();
    env.cmd()
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_WORKSPACE", "sw")
        .env_remove("WS_SECRETS_PASSWORD")
        .args(["-secrets", "backend"])
        .assert()
        .success()
        .stdout(predicates::str::diff("file\n"));
}

/// A subcommand that genuinely needs the password, with no password and no
/// terminal, must name the way out.
///
/// The actionable message already existed for the redaction hook; the CLI
/// reached rpassword instead and surfaced ENXIO, from which
/// `$WS_SECRETS_PASSWORD` is undiscoverable. Note this test only stays
/// hang-free because the refusal happens *before* rpassword: `cargo test` from
/// a terminal gives its children a controlling terminal, so a version that
/// still called `prompt_password` would block here rather than fail.
#[test]
fn a_password_needing_subcommand_names_the_env_var_when_it_cannot_prompt() {
    let env = Env::new();
    env.cmd()
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_WORKSPACE", "sw")
        .env_remove("WS_SECRETS_PASSWORD")
        .args(["-secrets", "list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("$WS_SECRETS_PASSWORD"))
        .stderr(predicates::str::contains("Device not configured").not());
}

#[test]
fn purge_refuses_without_tty() {
    let env = Env::new();
    sc(&env).args(["-secrets", "set", "K"]).write_stdin("v").assert().success();
    // assert_cmd runs with non-TTY stdin → purge must refuse (never silently wipe).
    sc(&env)
        .args(["-secrets", "purge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("without a TTY"));
    // the secret survived the refused purge
    sc(&env).args(["-secrets", "get", "K"]).assert().success().stdout(predicates::str::diff("v\n"));
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
    sc(&env)
        .args(["-secrets", "frobnicate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown -secrets subcommand: frobnicate"))
        .stderr(predicates::str::contains("usage: ws -secrets"));
}

/// A name the store lists but cannot resolve is the fingerprint of the pre-0.6.3
/// mock-keyring data loss. It must not be reported as a plain "no such secret",
/// which reads as a typo and sends people looking for a secret that is gone.
#[test]
fn a_listed_but_unresolvable_name_reports_data_loss_not_a_typo() {
    let env = Env::new();
    // The message under test distinguishes "listed but unresolvable" from
    // "never stored", which needs a vault that can answer *not found*. Where
    // there is no vault at all, `get` fails with a transport error before
    // reaching either branch — so probe first rather than assert on an error
    // that says nothing about this code.
    if !vault_works(&env, "sw") {
        return;
    }
    // Forge the keyring backend's on-disk name index with a name the OS vault
    // has never heard of — exactly the state the mock store left behind.
    let dir = env.home.path().join(".config/ws/secrets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sw.keyring-index"), "GHOST\n").unwrap();

    kc(&env, "sw")
        .args(["-secrets", "get", "GHOST"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("value is missing"))
        .stderr(predicates::str::contains("cannot be recovered"))
        .stderr(predicates::str::contains("ws -secrets set GHOST"));

    // A name that was never listed keeps the plain message.
    kc(&env, "sw")
        .args(["-secrets", "get", "NEVERSTORED"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no such secret: NEVERSTORED"));
}

#[test]
fn secrets_outside_workspace_errors() {
    let env = Env::new();
    // no WS_WORKSPACE and cwd isn't a workspace (force cwd to a dir with no
    // `.ws`, since the dev repo checkout itself may have a stray `.ws/` from
    // manual testing).
    env.cmd()
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "x")
        .current_dir(env.home.path())
        .args(["-secrets", "list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not in a workspace"));
}
