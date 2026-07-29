mod common;
use common::Env;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Raw git, for fixture setup and inspection only. Mirrors the `git()` helper
/// in `src/worktree.rs`'s own unit tests — the worktree/merge logic itself is
/// always driven through the `ws` binary in these tests, never called
/// directly.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Register `name` as a real git-backed workspace, through the binary
/// (`-adopt` needs no agent shim, unlike `ws <name>` launch). `-adopt` passes
/// `commit=false` to `contract::init`, so the `.ws/` bootstrap it writes is
/// still uncommitted afterward — give the repo an initial commit ourselves so
/// `git worktree add` has a HEAD to branch from, exactly as any real onboarded
/// workspace would.
fn base_workspace(env: &Env, name: &str) -> PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    git(&dir, &["config", "user.email", "dev@example.com"]);
    git(&dir, &["config", "user.name", "Dev"]);
    git(&dir, &["add", "-A"]);
    // `.ws/timeline.jsonl` is ws's own untracked, append-only bookkeeping — a
    // real `ws <name>` launch never commits it (`contract::init` writes it
    // only *after* its own commit step, so it never exists at commit time).
    // Unstage it so this fixture matches that: committing it here would make
    // the *worktree's* later timeline append look like a tracked-file
    // modification instead of untracked bookkeeping, which the dirty check
    // correctly does not wave through (see the I2 comment in
    // src/worktree.rs).
    git(&dir, &["reset", "--", ".ws/timeline.jsonl"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn create_makes_a_worktree_and_registers_it() {
    let env = Env::new();
    base_workspace(&env, "api");

    env.cmd()
        .arg("api@feat")
        .assert()
        .success()
        .stdout(predicates::str::contains("created api@feat"));

    let wt = env.root.join("api@feat");
    assert!(wt.join(".git").exists(), "the worktree checkout exists");
    assert!(wt.join(".ws/workspace.toml").is_file(), "the contract bootstrap ran");
    assert!(wt.join(".ws/base").is_file(), "it records which workspace it came from");

    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("api@feat"));
}

/// I1. `ws 'api@$(x)'` used to run `git worktree add` — a real branch and
/// checkout in the base repository — before the derived workspace name was
/// ever validated, leaving an orphan branch+worktree ws itself could no
/// longer see (no registry entry named it, so `-list`/`-rm` had nothing to
/// find). `$(x)` is a valid git branch name but not a valid workspace name
/// (shell metacharacters), so it exercises exactly that gap: the create must
/// fail AND leave git untouched.
#[test]
fn create_with_an_invalid_feature_name_leaves_no_branch_or_worktree_behind() {
    let env = Env::new();
    let base = base_workspace(&env, "api");

    env.cmd()
        .arg("api@$(x)")
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid workspace name"));

    assert!(!env.root.join("api@$(x)").exists(), "no worktree directory was created");
    let branches = git(&base, &["branch", "--list"]);
    assert!(!branches.contains("$(x)"), "no orphan branch left behind: {branches}");
    let worktrees = git(&base, &["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 1, "only the base checkout is listed: {worktrees}");

    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("$(x)").not());
}

/// Merge preflight parity: a dirty base must refuse before `git merge` ever
/// runs, naming the base as the offending side. Untracked-only dirt (no `git
/// add`) is deliberately what's used here — the same "either side has
/// untracked files" case the task brief calls out.
#[test]
fn merge_refuses_when_the_base_workspace_is_dirty() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@feat").assert().success();
    let wt = env.root.join("api@feat");

    std::fs::write(base.join("dirty.txt"), "uncommitted work\n").unwrap();

    env.cmd()
        .args(["api@feat", "--merge"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("uncommitted changes")
                .and(predicates::str::contains(base.display().to_string())),
        );

    // A refusal must not touch either side.
    assert!(wt.exists(), "a refused merge must not remove the worktree");
    assert!(base.join("dirty.txt").is_file(), "the base's untracked file survives");
    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("api@feat"));
}

/// The documented round trip, end to end through the binary: create, do real
/// work and commit it in the worktree, merge back. The feature's change must
/// land in the base and the worktree must be gone afterward.
#[test]
fn merge_lands_feature_changes_in_base_and_removes_the_worktree() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@feat").assert().success();
    let wt = env.root.join("api@feat");

    std::fs::write(wt.join("feature.txt"), "real work\n").unwrap();
    git(&wt, &["add", "feature.txt"]);
    git(&wt, &["commit", "-q", "-m", "feature work"]);

    env.cmd()
        .args(["api@feat", "--merge"])
        .assert()
        .success()
        .stdout(predicates::str::contains("merged feat into api"));

    assert_eq!(
        std::fs::read_to_string(base.join("feature.txt")).unwrap(),
        "real work\n",
        "the feature's work landed in the base"
    );
    let log = git(&base, &["log", "--oneline", "--merges"]);
    assert!(!log.trim().is_empty(), "--no-ff produced a merge commit: {log}");
    assert!(!wt.exists(), "the worktree is removed");

    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("api@feat").not());
}

/// The contract-version gate must cover worktree creation too: `create` writes
/// a branch and a checkout into the *base* repository, which is exactly the
/// kind of mutation the gate exists to refuse on a workspace some newer `ws`
/// owns. Without this, `ws base@feat` was the one mutating entry point that
/// walked straight past the version check.
#[test]
fn create_refuses_when_the_base_has_a_newer_contract_version() {
    let env = Env::new();
    let base = base_workspace(&env, "api");

    let wt_toml = base.join(".ws/workspace.toml");
    let body = std::fs::read_to_string(&wt_toml).unwrap();
    assert!(body.contains("contract_version = 1"), "sanity: {body}");
    std::fs::write(&wt_toml, body.replace("contract_version = 1", "contract_version = 999")).unwrap();

    env.cmd()
        .arg("api@feat")
        .assert()
        .failure()
        .stderr(predicate::str::contains("newer ws").and(predicate::str::contains("v999")));

    // And nothing was created before the refusal.
    assert!(!env.root.join("api@feat").exists(), "no checkout created");
    let branches = git(&base, &["branch", "--list"]);
    assert!(!branches.contains("feat"), "no branch created: {branches}");
}

/// `ws base@feature` has to mean both "make it" and "open it" — it is the only
/// spelling there is. It used to mean only the first, so every launch after the
/// creating one hit `worktree::create`'s "already exists" and the worktree was
/// unreachable by name for the rest of its life.
#[test]
fn an_existing_worktree_opens_instead_of_erroring() {
    let env = Env::new();
    let shim = env.fake_claude();
    let _base = base_workspace(&env, "api");

    env.cmd()
        .env("WS_CLAUDE_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .arg("api@retry")
        .assert()
        .success()
        .stdout(predicates::str::contains("created api@retry"));

    // Second time: launches, and specifically does not repeat the create.
    let out = env
        .cmd()
        .env("WS_CLAUDE_BIN", &shim)
        .env("WS_NO_EXEC", "1")
        .arg("api@retry")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("created api@retry"), "must not re-create: {stdout}");
    assert!(env.argv_log().contains("--session-id"), "must have launched an agent");
}
