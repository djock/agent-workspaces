mod common;
use common::Env;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

/// Build a fake cs session under `root`.
fn cs_session(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    let cs = dir.join(".cs");
    std::fs::create_dir_all(cs.join("memory")).unwrap();
    std::fs::create_dir_all(cs.join("local/log")).unwrap();
    std::fs::create_dir_all(cs.join("handoffs")).unwrap();
    std::fs::write(
        cs.join("README.md"),
        "---\nstatus: active\ntags: [\"rust\"]\n---\n# Session: x\n\n## Objective\n\nShip the parser\n",
    )
    .unwrap();
    std::fs::write(cs.join("memory/narrative.me.md"), "day 1: found the bug\n").unwrap();
    std::fs::write(cs.join("memory/MEMORY.md"), "- [note](n.md)\n").unwrap();
    std::fs::write(cs.join("handoffs/h.md"), "handoff\n").unwrap();
    std::fs::write(cs.join("timeline.jsonl"), "{\"event\":\"created\"}\n").unwrap();
    std::fs::write(cs.join("local/log/session.log"), "TOKEN=hunter2\n").unwrap();
    std::fs::write(dir.join("app.py"), "print('hi')\n").unwrap();
    dir
}

#[test]
fn migrates_a_real_session_directory() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "alpha");

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"])
        .assert().success()
        .stdout(predicate::str::contains("copied"));

    let dest = env.root.join("alpha");
    assert!(dest.join(".ws/workspace.toml").is_file());
    assert!(dest.join(".ws/README.md").is_file());
    assert!(dest.join(".ws/notebook/notebook.me.md").is_file(), "narrative → notebook");
    assert!(dest.join(".ws/memory/MEMORY.md").is_file());
    assert!(dest.join(".ws/handoffs/h.md").is_file());
    assert!(dest.join(".ws/timeline.jsonl").is_file());
    assert!(dest.join("app.py").is_file(), "project files come along");
    // .cs/local is NEVER copied — it holds the bash audit log.
    assert!(!dest.join(".ws/local/log/session.log").exists());
    assert!(!dest.join(".cs").exists(), ".cs is mapped, not copied verbatim");
    // the tag from the cs frontmatter landed
    let wt = std::fs::read_to_string(dest.join(".ws/workspace.toml")).unwrap();
    assert!(wt.contains("rust"), "{wt}");
    // cs is untouched
    assert!(src.join(".cs/README.md").is_file());
    assert!(!src.join(".ws").exists());

    // and it's registered
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("alpha"));
}

#[test]
fn symlinked_session_is_adopted_in_place_not_copied() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    // The real project lives elsewhere; cs only holds a symlink to it.
    let project = cs_session(env.home.path(), "real-project");
    std::os::unix::fs::symlink(&project, cs_root.join("proj")).unwrap();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "proj"])
        .assert().success();

    // .ws was created inside the real project, beside .cs — nothing was copied
    // into the sessions root.
    assert!(project.join(".ws/workspace.toml").is_file());
    assert!(project.join(".cs/README.md").is_file(), "cs stays usable");
    assert!(!env.root.join("proj").exists(), "a symlinked session must not be copied");
}

#[test]
fn migrate_all_and_dry_run() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "alpha");
    cs_session(&cs_root, "beta");
    std::fs::write(cs_root.join("index.md"), "# Sessions\n").unwrap();

    // dry run changes nothing
    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all", "--dry-run"])
        .assert().success()
        .stdout(predicate::str::contains("would migrate"));
    assert!(!env.root.join("alpha").exists());

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all"])
        .assert().success();
    assert!(env.root.join("alpha/.ws").is_dir());
    assert!(env.root.join("beta/.ws").is_dir());
}

#[test]
fn migrating_twice_refuses_the_second_time() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "alpha");

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"]).assert().success();
    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"])
        .assert().failure()
        .stderr(predicate::str::contains("already a ws workspace"));
}

#[test]
fn unknown_session_name_errors() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "ghost"])
        .assert().failure()
        .stderr(predicate::str::contains("no cs session named ghost"));
}

// C1: an existing, non-empty, non-workspace directory at the destination
// must never be silently merged into and overwritten.
#[test]
fn refuses_to_migrate_into_non_empty_non_workspace_directory() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "alpha");

    // Pre-existing debris at the destination — not a ws workspace.
    let dest = env.root.join("alpha");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("my-notes.txt"), "do not touch me\n").unwrap();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"])
        .assert().failure()
        .stderr(predicate::str::contains("non-empty directory"));

    // The pre-existing file is untouched, and nothing from the session landed.
    let contents = std::fs::read_to_string(dest.join("my-notes.txt")).unwrap();
    assert_eq!(contents, "do not touch me\n");
    assert!(!dest.join(".ws").exists());
    assert!(!dest.join("app.py").exists(), "session must not have been copied over the directory");
}

// I2: migrating a name already registered to a different, unrelated path
// must refuse rather than silently repoint the registry entry.
#[test]
fn refuses_when_name_already_registered_to_a_different_path() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "milo");

    // An unrelated existing workspace already claims the name "milo".
    let other = env.home.path().join("elsewhere").join("milo-project");
    std::fs::create_dir_all(&other).unwrap();
    env.cmd().current_dir(&other).args(["-adopt", "milo"]).assert().success();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "milo"])
        .assert().failure()
        .stderr(predicate::str::contains("already registered"));

    // The original registry entry must still resolve to the original path.
    let list_out = env.cmd().args(["-list"]).assert().success();
    let stdout = String::from_utf8_lossy(&list_out.get_output().stdout).to_string();
    assert!(stdout.contains(&other.display().to_string()), "{stdout}");

    // Nothing was migrated into the sessions root either.
    assert!(!env.root.join("milo").exists());
}

// I4: a dangling symlink entry is real but broken; both the named and --all
// paths must call it out rather than reporting "not found" or staying silent.
#[test]
fn dangling_symlink_entry_is_reported_as_broken_not_missing() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let missing_target = env.home.path().join("no-such-project-dir");
    std::os::unix::fs::symlink(&missing_target, cs_root.join("coach")).unwrap();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "coach"])
        .assert().failure()
        .stderr(predicate::str::contains("broken symlink"))
        .stderr(predicate::str::contains("coach"));

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all"])
        .assert().success()
        .stdout(predicate::str::contains("broken symlink"))
        .stdout(predicate::str::contains("coach"));
}

// I5: re-running --all must be idempotent — an already-migrated session is a
// skip, not a failure, so the run still exits 0. But asking for that same
// session by name explicitly still errors.
#[test]
fn migrate_all_is_idempotent_but_named_rerun_still_errors() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "alpha");

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all"]).assert().success();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all"])
        .assert().success()
        .stdout(predicate::str::contains("already migrated"));

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"])
        .assert().failure()
        .stderr(predicate::str::contains("already a ws workspace"));
}

// F1: dest-nested-in-source guard must catch the case even when dest doesn't
// exist yet (the normal case for a fresh migration) — canonicalize() fails on
// a non-existent path, so the guard must resolve the nearest existing
// ancestor instead of silently comparing an unresolved path. WS_ROOT here is
// configured nested inside the session's own directory, so dest ends up
// literally inside src; on macOS (tmp dirs under /var -> /private/var) the
// old code missed this because src was resolved and dest was not.
#[test]
fn refuses_when_dest_is_nested_inside_source() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "alpha");

    let nested_root = src.join("nested-ws-root");
    std::fs::create_dir_all(&nested_root).unwrap();

    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .env("WS_ROOT", &nested_root)
        .args(["migrate-cs", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nested inside"));

    // Nothing was created at the (nested) destination.
    assert!(!nested_root.join("alpha/.ws").exists());
}

// F5: a symlink inside a migrated session tree must be reported, not chased
// or silently dropped, and must not appear at the destination. A skipped
// symlink is dropped data, so it's a WARNING: line on stderr, and the run
// exits non-zero.
#[test]
fn symlink_inside_session_tree_is_reported_and_not_copied() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "alpha");
    std::os::unix::fs::symlink(src.join("app.py"), src.join("link.py")).unwrap();

    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipped symlink"))
        .stderr(predicate::str::contains("link.py"));

    let dest = env.root.join("alpha");
    assert!(!dest.join("link.py").exists(), "symlink must not be copied");
}

// F5: a subdirectory under .cs/memory/ must be copied whole, not dropped.
// Regression lock for the fix that routes a real directory under memory/
// through copy_tree instead of copy_file (which returns Ok(false) for
// non-regular files and would silently vanish it).
#[test]
fn subdirectory_under_memory_is_copied() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "alpha");
    std::fs::create_dir_all(src.join(".cs/memory/sub")).unwrap();
    std::fs::write(src.join(".cs/memory/sub/note.md"), "a note\n").unwrap();

    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "alpha"])
        .assert()
        .success();

    let dest = env.root.join("alpha");
    let copied = std::fs::read_to_string(dest.join(".ws/memory/sub/note.md")).unwrap();
    assert_eq!(copied, "a note\n");
}

// F2: a symlink to a directory under .cs/memory/ must be reported and
// skipped, not silently dropped. symlink_metadata reports the link itself
// (not the target), so is_dir() on it is false — before the fix this fell
// through to the regular-file-only copy path and vanished with no log line.
// A skipped symlink is dropped data, so it's a WARNING: line on stderr and
// the run exits non-zero.
#[test]
fn symlinked_directory_under_memory_is_skipped_and_reported() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "alpha");

    // A real directory living outside .cs/memory/, linked into memory/.
    let real_dir = src.join("linked-target");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(real_dir.join("note.md"), "a note\n").unwrap();
    std::os::unix::fs::symlink(&real_dir, src.join(".cs/memory/linked-dir")).unwrap();

    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("skipped symlink"))
        .stderr(predicate::str::contains("memory/linked-dir"));

    let dest = env.root.join("alpha");
    assert!(!dest.join(".ws/memory/linked-dir").exists());
}

// C1: a session whose `.git` is a *file* containing a `gitdir:` line is a
// linked git worktree of a repo living elsewhere. Migrating it (copying that
// pointer) would make the destination and the original worktree share one
// admin dir in a third repo — refuse it, name it, and don't create a
// destination. It's a per-session refusal so --all still proceeds otherwise.
#[test]
fn refuses_to_migrate_a_linked_git_worktree() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "milo@meals");
    // Simulate a linked worktree: .git is a file pointing at another repo's
    // worktree admin dir, not a real .git directory.
    std::fs::write(
        src.join(".git"),
        "gitdir: /Users/someone/Native/milo/Milo/.git/worktrees/milo@meals\n",
    )
    .unwrap();

    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "milo@meals"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("milo@meals"))
        .stderr(predicate::str::contains("linked git worktree"));

    assert!(!env.root.join("milo@meals").exists(), "nothing must be created at the destination");

    // Under --all, the refusal must not block other sessions.
    cs_session(&cs_root, "beta");
    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "--all"])
        .assert()
        .failure() // one session failed, so the overall run is non-zero...
        .stderr(predicate::str::contains("linked git worktree"));
    assert!(env.root.join("beta/.ws").is_dir(), "...but the other session still migrated");

    // --dry-run must show the refusal too.
    let src2 = cs_session(&cs_root, "gamma-worktree");
    std::fs::write(
        src2.join(".git"),
        "gitdir: /Users/someone/other/.git/worktrees/gamma-worktree\n",
    )
    .unwrap();
    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "gamma-worktree", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("linked git worktree"));
}

// I7: --dry-run on a symlinked (in-place) session is the one path whose
// failure mode is "wrote into the user's real project" — it must leave the
// live target completely untouched: no .ws created, no files added.
#[test]
fn dry_run_on_symlinked_session_touches_nothing_in_the_live_project() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let project = cs_session(env.home.path(), "real-project");
    std::os::unix::fs::symlink(&project, cs_root.join("proj")).unwrap();

    // Snapshot the project tree before the dry run.
    let before: Vec<PathBuf> = walk(&project);

    env.cmd()
        .env("WS_CS_ROOT", &cs_root)
        .args(["migrate-cs", "proj", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would migrate"))
        .stdout(predicate::str::contains("in place"));

    assert!(!project.join(".ws").exists(), "dry-run must not create .ws in the live project");
    let after: Vec<PathBuf> = walk(&project);
    assert_eq!(before, after, "dry-run must not add, remove, or touch any file in the live project");
}

/// Sorted list of every path under `root` (relative to `root`).
fn walk(root: &Path) -> Vec<PathBuf> {
    fn rec(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(root).unwrap().to_path_buf());
            if p.is_dir() {
                rec(&p, root, out);
            }
        }
    }
    let mut out = Vec::new();
    rec(root, root, &mut out);
    out.sort();
    out
}

// I7: in-place migration into a target that already has a CLAUDE.local.md
// (the default agent's context file) must preserve the existing content —
// context regeneration splices into the managed block (ws:begin/ws:end)
// rather than overwriting the whole file.
#[test]
fn in_place_migration_preserves_existing_claude_local_md_content() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let project = cs_session(env.home.path(), "real-project");
    std::fs::write(
        project.join("CLAUDE.local.md"),
        "# my hand-written notes\n\nDo not lose this.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&project, cs_root.join("proj")).unwrap();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "proj"])
        .assert().success();

    let claude_md = std::fs::read_to_string(project.join("CLAUDE.local.md")).unwrap();
    assert!(
        claude_md.contains("Do not lose this."),
        "existing CLAUDE.local.md content must survive migration: {claude_md}"
    );
    assert!(project.join(".ws/workspace.toml").is_file());
}

// Archived end-to-end: a cs README whose frontmatter says archived must land
// as an archived ws workspace — hidden from the default -list, present with
// --archived.
#[test]
fn archived_cs_session_migrates_to_an_archived_workspace() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let dir = cs_session(&cs_root, "old-project");
    std::fs::write(
        dir.join(".cs/README.md"),
        "---\nstatus: archived\ntags: [\"rust\"]\n---\n# Session: x\n\n## Objective\n\nShip the parser\n",
    )
    .unwrap();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "old-project"])
        .assert().success();

    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("old-project").not());
    env.cmd().args(["-list", "--archived"]).assert().success()
        .stdout(predicate::str::contains("old-project"));
}
