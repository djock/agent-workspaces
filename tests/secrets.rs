mod common;
use common::Env;
use predicates::prelude::*;

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
