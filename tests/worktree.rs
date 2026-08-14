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

    env.cmd().arg("-list").assert().success().stdout(predicates::str::contains("api@feat"));
}

/// A feature worktree is the same project on a branch, so it must start on the
/// agent that project is already on. Creation used to stamp the *config*
/// default, which put every worktree of a Codex workspace on Claude.
#[test]
fn a_worktree_inherits_its_bases_agent() {
    let env = Env::new();
    let base = base_workspace(&env, "api");

    let base_toml = base.join(".ws/workspace.toml");
    let toml = std::fs::read_to_string(&base_toml)
        .unwrap()
        .replace("default_agent = \"claude\"", "default_agent = \"codex\"");
    assert!(toml.contains("codex"), "fixture must actually be on codex");
    std::fs::write(&base_toml, toml).unwrap();

    env.cmd().arg("api@feat").assert().success();

    let child = std::fs::read_to_string(env.root.join("api@feat/.ws/workspace.toml")).unwrap();
    assert!(
        child.contains("default_agent = \"codex\""),
        "the worktree must inherit codex from its base: {child}"
    );
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

    env.cmd().arg("-list").assert().success().stdout(predicates::str::contains("$(x)").not());
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

    env.cmd().args(["api@feat", "--merge"]).assert().failure().stderr(
        predicates::str::contains("uncommitted changes")
            .and(predicates::str::contains(base.display().to_string())),
    );

    // A refusal must not touch either side.
    assert!(wt.exists(), "a refused merge must not remove the worktree");
    assert!(base.join("dirty.txt").is_file(), "the base's untracked file survives");
    env.cmd().arg("-list").assert().success().stdout(predicates::str::contains("api@feat"));
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

    env.cmd().arg("-list").assert().success().stdout(predicates::str::contains("api@feat").not());
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
    std::fs::write(&wt_toml, body.replace("contract_version = 1", "contract_version = 999"))
        .unwrap();

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

/// The property the whole `-features` screen exists for: what it says about a
/// worktree and what `--merge` actually does are the same computation, so the
/// screen cannot promise a merge that then refuses, nor report a blocker that
/// would have gone straight through.
///
/// Checked by driving both against the same fixtures rather than by reading the
/// code: a shared function that one caller subtly bypasses is exactly how these
/// two drift apart.
#[test]
fn the_features_screen_agrees_with_what_merging_does() {
    let env = Env::new();
    let base = base_workspace(&env, "api");

    // Ready: a committed change on the branch.
    env.cmd().arg("api@ready").assert().success();
    let ready = env.root.join("api@ready");
    std::fs::write(ready.join("done.txt"), "finished\n").unwrap();
    // The user's file only: `add -A` would also commit ws's own untracked
    // bookkeeping onto the branch, which then collides with the base's copy of
    // it — the I2 case `WS_BOOKKEEPING` exists for.
    git(&ready, &["add", "done.txt"]);
    git(&ready, &["commit", "-q", "-m", "work"]);

    // Blocked: uncommitted work in the worktree.
    env.cmd().arg("api@dirty").assert().success();
    std::fs::write(env.root.join("api@dirty").join("wip.txt"), "half done\n").unwrap();

    let out = env.cmd().current_dir(&base).args(["api", "-features"]).output().unwrap();
    let screen = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(screen.contains("ready"), "{screen}");
    assert!(screen.contains("dirty"), "{screen}");

    // The screen said `dirty` is blocked, so the merge must refuse it...
    let dirty_line = screen.lines().find(|l| l.contains("dirty")).unwrap();
    assert!(dirty_line.starts_with('✗'), "the screen must call it blocked: {dirty_line}");
    env.cmd()
        .args(["api@dirty", "--merge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("uncommitted"));

    // ...and said `ready` merges, so it must.
    let ready_line = screen.lines().find(|l| l.contains("ready")).unwrap();
    assert!(ready_line.starts_with('✓'), "the screen must call it ready: {ready_line}");
    assert!(ready_line.contains("1 commit"), "and say what it will do: {ready_line}");
    env.cmd().args(["api@ready", "--merge"]).assert().success();
}

/// A refusal names every blocker. Fixing one only to be refused for the next is
/// the slow way to find out there were three.
#[test]
fn a_refused_merge_reports_every_blocker_at_once() {
    let env = Env::new();
    let base = base_workspace(&env, "api");
    env.cmd().arg("api@feat").assert().success();

    std::fs::write(env.root.join("api@feat").join("wip.txt"), "half done\n").unwrap();
    std::fs::write(base.join("also-dirty.txt"), "and here\n").unwrap();

    env.cmd()
        .args(["api@feat", "--merge"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("wip.txt"))
        .stderr(predicates::str::contains("also-dirty.txt"));
}

#[test]
fn features_is_empty_for_a_base_with_no_worktrees() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd()
        .args(["api", "-features"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no feature worktrees"));
}

/// A `--porcelain` record per feature, for a script.
#[test]
fn porcelain_emits_one_tab_separated_record_per_feature() {
    let env = Env::new();
    base_workspace(&env, "api");
    env.cmd().arg("api@one").assert().success();
    env.cmd().arg("api@two").assert().success();

    let out = env.cmd().args(["api", "-features", "--porcelain"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for l in lines {
        assert_eq!(l.matches('\t').count(), 3, "name, state, ahead, plan: {l:?}");
    }
}

/// `myproj@wip` must not be found by a listing of a base whose name merely
/// prefixes it — the collision a `starts_with` on the base name produces.
#[test]
fn a_bases_features_never_include_another_bases() {
    let env = Env::new();
    base_workspace(&env, "api");
    base_workspace(&env, "api2");
    env.cmd().arg("api2@feat").assert().success();

    env.cmd()
        .args(["api", "-features"])
        .assert()
        .success()
        .stdout(predicates::str::contains("no feature worktrees"));
    env.cmd()
        .args(["api2", "-features"])
        .assert()
        .success()
        .stdout(predicates::str::contains("feat"));
}
