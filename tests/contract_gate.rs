mod common;
use common::Env;
use predicates::prelude::*;

fn launch_cmd(env: &Env, shim: &std::path::Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", shim).env("WS_NO_EXEC", "1");
    c
}

/// Task 3 item 2: `contract_version` is written into every `workspace.toml`
/// but, until this fix, was never read for a decision. A workspace whose
/// recorded version is greater than this binary's must refuse to launch —
/// mimicking a workspace created by a `ws` from the future, which a rollback
/// or a mixed-version team can produce for real.
#[test]
fn launch_refuses_a_workspace_created_by_a_newer_ws() {
    let env = Env::new();
    let shim = env.fake_claude();

    // First launch creates the workspace at the current CONTRACT_VERSION.
    launch_cmd(&env, &shim).arg("proj").assert().success();

    let wt = env.root.join("proj/.ws/workspace.toml");
    let body = std::fs::read_to_string(&wt).unwrap();
    assert!(body.contains("contract_version = 1"), "sanity: {body}");
    let bumped = body.replace("contract_version = 1", "contract_version = 999");
    std::fs::write(&wt, bumped).unwrap();

    launch_cmd(&env, &shim)
        .arg("proj")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("newer ws")
                .and(predicate::str::contains("v999"))
                .and(predicate::str::contains("update ws")),
        );
}

/// The equal, older and absent cases must all pass — only strictly greater
/// refuses.
#[test]
fn launch_passes_equal_older_and_absent_contract_versions() {
    let env = Env::new();
    let shim = env.fake_claude();
    launch_cmd(&env, &shim).arg("proj").assert().success();
    let wt = env.root.join("proj/.ws/workspace.toml");
    let body = std::fs::read_to_string(&wt).unwrap();
    assert!(body.contains("contract_version = 1"));

    // Equal (the value `-adopt`/launch itself wrote): already covered by the
    // very launch above succeeding, but assert the *second* (resume) launch
    // too, since that is the path this gate actually guards.
    launch_cmd(&env, &shim).arg("proj").assert().success();

    // Older: an explicit v0.
    std::fs::write(&wt, body.replace("contract_version = 1", "contract_version = 0")).unwrap();
    launch_cmd(&env, &shim).arg("proj").assert().success();

    // Absent: the field missing entirely (every workspace.toml before the
    // field existed at all).
    let no_field: String = std::fs::read_to_string(&wt)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("contract_version"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&wt, no_field).unwrap();
    launch_cmd(&env, &shim).arg("proj").assert().success();
}

/// The other half of the requirement: read-only, multi-workspace commands
/// must never refuse just because one workspace on disk was created by a
/// newer `ws`. `-list` walks every registered workspace to display it, not
/// to mutate it.
#[test]
fn list_does_not_refuse_for_a_workspace_with_a_newer_contract_version() {
    let env = Env::new();
    let proj = env.root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "proj"]).assert().success();

    let wt = proj.join(".ws/workspace.toml");
    let body = std::fs::read_to_string(&wt).unwrap();
    assert!(body.contains("contract_version = 1"), "sanity: {body}");
    let bumped = body.replace("contract_version = 1", "contract_version = 999");
    std::fs::write(&wt, bumped).unwrap();

    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicate::str::contains("proj"));
}

/// Same, for `-search`.
#[test]
fn search_does_not_refuse_for_a_workspace_with_a_newer_contract_version() {
    let env = Env::new();
    let proj = env.root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "proj"]).assert().success();
    std::fs::write(proj.join("needle.txt"), "unique-search-token").unwrap();

    let wt = proj.join(".ws/workspace.toml");
    let body = std::fs::read_to_string(&wt).unwrap();
    assert!(body.contains("contract_version = 1"), "sanity: {body}");
    let bumped = body.replace("contract_version = 1", "contract_version = 999");
    std::fs::write(&wt, bumped).unwrap();

    env.cmd()
        .args(["-search", "unique-search-token"])
        .assert()
        .success();
}

/// Mutating single-workspace commands (tag/status/archive/msg/queue) are also
/// gated — the gate is not launch-only. One representative case, `-tag`,
/// stands in for the class; `check_gate`'s own unit tests in `contract.rs`
/// cover the version-comparison logic itself.
#[test]
fn tag_add_refuses_a_workspace_with_a_newer_contract_version() {
    let env = Env::new();
    let proj = env.root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "proj"]).assert().success();

    let wt = proj.join(".ws/workspace.toml");
    let body = std::fs::read_to_string(&wt).unwrap();
    assert!(body.contains("contract_version = 1"), "sanity: {body}");
    let bumped = body.replace("contract_version = 1", "contract_version = 999");
    std::fs::write(&wt, bumped).unwrap();

    env.cmd()
        .current_dir(&proj)
        .args(["-tag", "add", "rust"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("newer ws"));
}
