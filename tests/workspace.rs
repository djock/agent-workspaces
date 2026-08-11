mod common;
use common::Env;
use predicates::prelude::*;

/// I8. On an empty registry both `-list` and `-list --archived` must say the
/// registry is empty. The pre-fix code had these two messages swapped, so
/// plain `-list` pointed the user at `--archived` for workspaces that could
/// not exist.
#[test]
fn list_empty_says_none() {
    let env = Env::new();
    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("no workspaces yet"));
}

#[test]
fn list_archived_on_an_empty_registry_says_the_registry_is_empty() {
    let env = Env::new();
    env.cmd()
        .args(["-list", "--archived"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no workspaces yet"));
}

/// The other half of the distinction: workspaces exist, but every one of them
/// is archived. *This* is when "try: ws -list --archived" is useful advice.
#[test]
fn list_points_at_archived_only_when_archived_workspaces_actually_exist() {
    let env = Env::new();
    let proj = env.root.join("shelved");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "shelved"]).assert().success();
    env.cmd().args(["-archive", "shelved"]).assert().success();

    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("no active workspaces"));
    // ...and --archived then finds it.
    env.cmd()
        .args(["-list", "--archived"])
        .assert()
        .success()
        .stdout(predicates::str::contains("shelved"));
}

#[test]
fn adopt_current_dir() {
    let env = Env::new();
    let proj = env.home.path().join("myproj");
    std::fs::create_dir_all(&proj).unwrap();

    env.cmd()
        .current_dir(&proj)
        .arg("-adopt")
        .assert()
        .success()
        .stdout(predicates::str::contains("adopted"));

    assert!(proj.join(".ws/workspace.toml").is_file());

    // Now listed by name (dir basename).
    env.cmd().arg("-list").assert().success().stdout(predicates::str::contains("myproj"));
}

#[test]
fn rm_created_workspace_deletes_dir() {
    let env = Env::new();
    // create under sessions_root
    let root_ws = env.root.join("throwaway");
    // create via launch path is heavy; instead adopt a dir *inside* sessions_root
    std::fs::create_dir_all(&root_ws).unwrap();
    env.cmd().current_dir(&root_ws).args(["-adopt", "throwaway"]).assert().success();
    assert!(root_ws.join(".ws").is_dir());

    env.cmd()
        .args(["-rm", "throwaway", "--force"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed throwaway"));

    // dir under sessions_root is gone; no longer listed
    assert!(!root_ws.exists());
    env.cmd().arg("-list").assert().stdout(predicates::str::contains("throwaway").not());
}

#[test]
fn rm_adopted_external_keeps_project() {
    let env = Env::new();
    let proj = env.home.path().join("external");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("keepme.txt"), "data").unwrap();
    env.cmd().current_dir(&proj).arg("-adopt").assert().success();

    env.cmd().args(["-rm", "external", "--force"]).assert().success();

    // project dir and file survive; only .ws removed + unregistered
    assert!(proj.join("keepme.txt").is_file());
    assert!(!proj.join(".ws").exists());
}

#[test]
fn adopt_inside_existing_repo_does_not_nested_init() {
    let env = Env::new();
    let repo = env.home.path().join("outer");
    std::fs::create_dir_all(&repo).unwrap();
    std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    env.cmd().current_dir(&sub).args(["-adopt", "sub"]).assert().success();
    // no nested repo created in the subdirectory
    assert!(!sub.join(".git").exists(), "adopt must not init a nested repo inside an existing one");
}

#[test]
fn rm_prints_removed_only_on_success() {
    let env = Env::new();
    let proj = env.home.path().join("rmok");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "rmok"]).assert().success();
    env.cmd()
        .args(["-rm", "rmok", "--force"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed rmok"));
    // unregistered
    env.cmd().arg("-list").assert().stdout(predicates::str::contains("rmok").not());
}

#[test]
fn rm_cleans_up_missing_workspace_entry() {
    let env = Env::new();
    let proj = env.home.path().join("gone");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "gone"]).assert().success();

    // user manually deletes the directory out from under ws → stale (missing) registry entry
    std::fs::remove_dir_all(&proj).unwrap();

    // -rm must still succeed and unregister the stale entry
    env.cmd()
        .args(["-rm", "gone", "--force"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed gone"));
    env.cmd().arg("-list").assert().stdout(predicates::str::contains("gone").not());
}
