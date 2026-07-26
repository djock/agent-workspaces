mod common;
use common::Env;
use predicates::prelude::*;

/// Create a workspace directory the way `ws -adopt` would, without launching an
/// agent: make the dir, run `-adopt` inside it.
fn make_ws(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    dir
}

#[test]
fn tag_add_list_rm() {
    let env = Env::new();
    let dir = make_ws(&env, "proj");
    env.cmd().current_dir(&dir).args(["-tag", "add", "rust", "cli"]).assert().success();
    env.cmd().current_dir(&dir).args(["-tag", "list"]).assert().success()
        .stdout(predicate::str::contains("cli"))
        .stdout(predicate::str::contains("rust"));
    env.cmd().current_dir(&dir).args(["-tag", "rm", "cli"]).assert().success();
    env.cmd().current_dir(&dir).args(["-tag", "list"]).assert().success()
        .stdout(predicate::str::contains("rust"))
        .stdout(predicate::str::contains("cli").not());
}

#[test]
fn tag_by_name_from_anywhere() {
    let env = Env::new();
    make_ws(&env, "proj");
    // No cwd inside the workspace — address it by name instead.
    env.cmd().current_dir(env.home.path())
        .args(["-tag", "add", "--workspace", "proj", "rust"]).assert().success();
    env.cmd().current_dir(env.home.path())
        .args(["-tag", "list", "--workspace", "proj"]).assert().success()
        .stdout(predicate::str::contains("rust"));
}

#[test]
fn status_set_shows_in_list_then_clears() {
    let env = Env::new();
    let dir = make_ws(&env, "proj");
    env.cmd().current_dir(&dir).args(["-status", "waiting on review"]).assert().success();
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("waiting on review"));
    env.cmd().current_dir(&dir).args(["-status", "--clear"]).assert().success();
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("waiting on review").not());
}

#[test]
fn archive_hides_from_list_until_flagged() {
    let env = Env::new();
    make_ws(&env, "keep");
    make_ws(&env, "old");
    env.cmd().args(["-archive", "old"]).assert().success();

    // default listing hides it. Match "old\t" (the name column, tab-terminated)
    // rather than a bare "old" — on macOS the default TMPDIR contains the path
    // segment "folders", which itself contains "old" as a substring and would
    // make a bare `contains("old")` pass even when the workspace isn't listed.
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("keep"))
        .stdout(predicate::str::contains("old\t").not());
    // --archived shows it, marked
    env.cmd().args(["-list", "--archived"]).assert().success()
        .stdout(predicate::str::contains("old\t"))
        .stdout(predicate::str::contains("archived"));
    // unarchive brings it back
    env.cmd().args(["-unarchive", "old"]).assert().success();
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("old\t"));
}

#[test]
fn list_filters_by_tag() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    make_ws(&env, "beta");
    env.cmd().current_dir(&a).args(["-tag", "add", "rust"]).assert().success();
    env.cmd().args(["-list", "--tag", "rust"]).assert().success()
        .stdout(predicate::str::contains("alpha"))
        .stdout(predicate::str::contains("beta").not());
}

#[test]
fn archive_unknown_workspace_errors() {
    let env = Env::new();
    env.cmd().args(["-archive", "ghost"]).assert().failure()
        .stderr(predicate::str::contains("no such workspace"));
}

#[test]
fn tag_outside_workspace_errors() {
    let env = Env::new();
    env.cmd().current_dir(env.home.path())
        .args(["-tag", "add", "rust"]).assert().failure()
        .stderr(predicate::str::contains("not in a workspace"));
}

#[test]
fn tag_with_unknown_workspace_name_errors() {
    let env = Env::new();
    env.cmd().args(["-tag", "add", "--workspace", "ghost", "rust"])
        .assert().failure()
        .stderr(predicate::str::contains("no such workspace"));
}
