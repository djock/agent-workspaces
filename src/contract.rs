use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::actors;
use crate::registry;

pub const CONTRACT_VERSION: u32 = 1;

pub fn init(name: &str, root: &Path, agent: &str, commit: bool) -> Result<()> {
    // Validate here, not only in `open_or_create`: `-adopt`, `migrate-cs` and
    // `worktree::create` all reach this function directly, and `-adopt` used to
    // register an arbitrary name — which `ws -spawn` then executed.
    crate::workspace::validate_name(name)?;
    let ws = root.join(".ws");
    for sub in ["notebook", "memory", "handoffs", "plans", "local"] {
        std::fs::create_dir_all(ws.join(sub))?;
    }

    let now = crate::now_iso();
    let actor = actors::actor_slug();

    // workspace.toml (identity)
    let workspace_toml = format!(
        "name = \"{name}\"\n\
         created = \"{now}\"\n\
         contract_version = {CONTRACT_VERSION}\n\
         default_agent = \"{agent}\"\n\
         archived = false\n\
         tags = []\n"
    );
    write_if_absent(&ws.join("workspace.toml"), &workspace_toml)?;

    // README.md
    write_if_absent(
        &ws.join("README.md"),
        &format!("# {name}\n\n## Objective\n\n_(captured from the first prompt)_\n\n## Outcome\n\n"),
    )?;

    // notebook
    write_if_absent(
        &ws.join("notebook/NOTES.md"),
        "# Notebook index\n\nPer-actor lab notebooks live beside this file.\n",
    )?;
    write_if_absent(
        &ws.join(format!("notebook/notebook.{actor}.md")),
        &format!("# Notebook ({actor})\n\n"),
    )?;

    // memory keep-file (agent memory redirect target; keep dir under git)
    write_if_absent(&ws.join("memory/.gitkeep"), "")?;
    write_if_absent(&ws.join("handoffs/.gitkeep"), "")?;
    write_if_absent(&ws.join("plans/.gitkeep"), "")?;

    // .ws/.gitignore — never commit local/, secrets, or transaction sidecars.
    //
    // `*.lock` covers the `txn` module's sidecar lock files (`workspace.toml.lock`
    // and friends). They are per-machine coordination state with no meaning in
    // another checkout, and committing one would put a file in the repo that a
    // co-developer's ws then locks against for no reason.
    write_if_absent(
        &ws.join(".gitignore"),
        "local/\n*.enc\n*.lock\n",
    )?;

    // .ws/.gitattributes — union-merge every append-only file.
    //
    // `queue/*.jsonl` is the same shape as the notebooks and the timeline:
    // append-only, one record per line, written independently in a base and in
    // its worktree. Without union merge a `base@feature` merge conflicts on
    // it, and a conflicted merge is the C1 path — the one that leaves the
    // user's repository mid-merge.
    write_if_absent(
        &ws.join(".gitattributes"),
        "notebook/*.md merge=union\ntimeline.jsonl merge=union\nqueue/*.jsonl merge=union\n",
    )?;

    // git init only if this dir is not already inside a repo (direct or ancestor)
    let inside = Command::new("git")
        .arg("-C").arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if !inside {
        run_git(root, &["init", "-q"])?;
    }
    if commit {
        // `git commit -- <path>` is a partial commit, and git refuses those
        // outright whenever a sequencer operation is in progress: `fatal:
        // cannot do a partial commit during a merge / a cherry-pick`, exit
        // 128. Enumerating those states is the wrong shape — an earlier fix
        // probed MERGE_HEAD only and left cherry-pick, revert and conflicted
        // `rebase --merge` (which sets CHERRY_PICK_HEAD) failing exactly as
        // before. So correctness does not depend on the probe below at all:
        // the commit itself is non-fatal, like the `add`, and any git state
        // that refuses it simply costs the convenience commit rather than
        // failing workspace creation before `registry::register` runs.
        //
        // The probe is kept purely to avoid git's noisy `fatal:` on stderr in
        // the common mid-merge case. It detects via `rev-parse --git-path`,
        // not by assuming `.git` is a directory and joining `.git/MERGE_HEAD`
        // ourselves: a linked git worktree's `.git` is a *file* containing a
        // `gitdir:` pointer, and conflating the two produced a Critical in
        // this project's Phase 6. `--git-path` resolves correctly for plain
        // repos, linked worktrees, and `GIT_DIR` overrides alike.
        let merging = run_git_stdout(root, &["rev-parse", "--git-path", "MERGE_HEAD"])
            .map(|p| root.join(p.trim()).exists())
            .unwrap_or(false);
        if !merging {
            // C1: stage and commit *only* `.ws`, never `git add -A`.
            //
            // `init` runs with commit=true from `workspace::open_or_create`, and
            // `root` is not always a directory ws created: a registry entry whose
            // `.ws/` was deleted resolves to the user's own project (a "(missing)"
            // row — Enter on it in the TUI lands here). `git add -A` there swept
            // the user's uncommitted work into a commit authored `ws`, rewriting
            // the history of a repository ws does not own. `commands::adopt`
            // passes commit=false for exactly this reason.
            //
            // The pathspec on `commit` matters as much as the one on `add`: a
            // bare `git commit` writes whatever is already in the index, which in
            // a repository ws does not own includes the user's own staged work.
            // `git commit -- .ws` commits those paths only and leaves the rest of
            // the index exactly as the user left it.
            //
            // The `add` is deliberately not fatal: if `.ws/` is excluded by an
            // ancestor or global gitignore, `git add -- .ws` fails where
            // `git add -A` silently skipped, and failing to record a convenience
            // commit must not fail workspace creation. The staged-diff check
            // below then finds nothing and the commit is skipped.
            let _ = Command::new("git").arg("-C").arg(root).args(["add", "--", ".ws"]).status();
            let staged = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["diff", "--cached", "--name-only", "--", ".ws"])
                .output()?;
            if !staged.stdout.is_empty() {
                // Non-fatal, exactly as the `add` above is: recording a
                // convenience commit must never fail workspace creation. This
                // is what closes every partial-commit-refusing git state at
                // once instead of the ones we happened to think of.
                let _ = run_git(
                    root,
                    &[
                        "-c",
                        "user.name=ws",
                        "-c",
                        "user.email=ws@localhost",
                        "commit",
                        "-q",
                        "-m",
                        "chore: initialize ws workspace",
                        "--",
                        ".ws",
                    ],
                );
            }
        }
    }

    registry::register(name, root)?;

    // Best-effort: record workspace creation on the timeline.
    let _ = crate::timeline::record(
        &root.join(".ws").join("timeline.jsonl"),
        "created",
        &actor,
        serde_json::json!({ "agent": agent }),
    );

    Ok(())
}

fn write_if_absent(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").arg("-C").arg(root).args(args).status()?;
    if !status.success() {
        anyhow::bail!("git {:?} failed", args);
    }
    Ok(())
}

/// Like `run_git`, but returns captured stdout instead of inheriting the
/// child's. Used for `rev-parse --git-path`, where the point is the path it
/// prints, not just success/failure.
fn run_git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed", args);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn read_session_id(state_toml: &Path, agent: &str) -> Option<String> {
    let s = std::fs::read_to_string(state_toml).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get(agent)?.get("session_id")?.as_str().map(String::from)
}

/// Read `.ws/local/state.toml` as a table for a read-modify-write.
///
/// Absent → an empty table. Present but unparseable → `Err`: this file is
/// never replaced wholesale on a parse failure. Both writers went through
/// `.ok().and_then(...).unwrap_or_default()` before, which silently threw the
/// file away — including whatever the *other* writer had put there.
pub fn read_state_table(state_toml: &Path) -> Result<toml::Table> {
    match std::fs::read_to_string(state_toml) {
        Ok(s) => toml::from_str(&s).with_context(|| {
            format!("{} is corrupt (refusing to overwrite)", state_toml.display())
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", state_toml.display())),
    }
}

/// Write `.ws/local/state.toml` atomically and clobber-safely.
///
/// `state.toml` has two independent writers (`write_session_id` from hook
/// handlers, `agents::codex::record_marker` from launch), so the safe
/// temp+rename in `atomic::atomic_write` matters here in particular.
pub fn write_state_table(state_toml: &Path, t: &toml::Table) -> Result<()> {
    crate::atomic::atomic_write(state_toml, toml::to_string_pretty(t)?)?;
    Ok(())
}

/// Read-modify-write `state.toml` under an interprocess lock.
///
/// This is the file the C2 finding is about: `write_session_id` (from hook
/// handlers) and `agents::codex::record_owner` (from launch) write it from
/// *different processes*. Merging keys instead of replacing them stopped either
/// from dropping the other's fields, but merging alone cannot stop a lost update —
/// both writers can still read the same starting table and rename their own
/// result into place, and the second silently wins. Every mutation goes through
/// here so the read and the write are one transaction.
///
/// Callers must not nest: `f` may not call another `update_state` on the same
/// path (see `txn`'s module docs on reentrancy).
pub fn update_state(state_toml: &Path, f: impl FnOnce(&mut toml::Table)) -> Result<()> {
    crate::txn::transaction(state_toml, || {
        let mut t = read_state_table(state_toml)?;
        f(&mut t);
        write_state_table(state_toml, &t)
    })
}

/// Merge `key`/`value` into the sub-table for `agent`, preserving its other keys.
pub fn set_agent_field(
    state_toml: &Path,
    agent: &str,
    key: &str,
    value: toml::Value,
) -> Result<()> {
    update_state(state_toml, |t| {
        // Merge into the agent's existing table rather than replacing it, so a
        // `session_id` write does not drop the `launched` marker (or vice versa).
        let mut entry = match t.get(agent).and_then(|v| v.as_table()) {
            Some(existing) => existing.clone(),
            None => toml::Table::new(),
        };
        entry.insert(key.to_string(), value);
        t.insert(agent.to_string(), toml::Value::Table(entry));
    })
}

pub fn write_session_id(state_toml: &Path, agent: &str, id: &str) -> Result<()> {
    set_agent_field(state_toml, agent, "session_id", toml::Value::String(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_creates_layout() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        let root = d.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        init("proj", &root, "claude", true).unwrap();

        assert!(root.join(".ws/workspace.toml").is_file());
        assert!(root.join(".ws/README.md").is_file());
        assert!(root.join(".ws/notebook/NOTES.md").is_file());
        assert!(root.join(".ws/memory").is_dir());
        assert!(root.join(".ws/handoffs").is_dir());
        assert!(root.join(".ws/local").is_dir());
        assert!(root.join(".ws/.gitignore").is_file());
        assert!(root.join(".git").is_dir());

        let toml = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
        assert!(toml.contains("name = \"proj\""));
        assert!(toml.contains("contract_version = 1"));
    }

    /// C1 regression. `init(commit=true)` inside a repository ws did not
    /// create must never sweep the user's work into its commit — not from the
    /// working tree, and not out of an index the user staged themselves.
    #[test]
    fn init_never_commits_anything_outside_ws() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        let root = d.path().join("myproj");
        std::fs::create_dir_all(&root).unwrap();

        // A real repository with history, exactly what `-adopt` targets.
        run_git(&root, &["init", "-q"]).unwrap();
        std::fs::write(root.join("src.rs"), "fn main() {}\n").unwrap();
        run_git(&root, &["add", "src.rs"]).unwrap();
        run_git(
            &root,
            &["-c", "user.name=u", "-c", "user.email=u@e", "commit", "-q", "-m", "base"],
        )
        .unwrap();

        // Work in flight: one file only in the working tree, one the user
        // staged themselves. Neither belongs in a ws-authored commit.
        std::fs::write(root.join("wip.txt"), "secret-wip\n").unwrap();
        std::fs::write(root.join("staged.txt"), "mine\n").unwrap();
        run_git(&root, &["add", "staged.txt"]).unwrap();

        init("myproj", &root, "claude", true).unwrap();

        let files = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["show", "--pretty=format:", "--name-only", "HEAD"])
            .output()
            .unwrap();
        let files = String::from_utf8_lossy(&files.stdout).to_string();
        assert!(files.contains(".ws/workspace.toml"), "the workspace itself is committed: {files}");
        for outsider in ["wip.txt", "staged.txt", "src.rs"] {
            assert!(
                !files.contains(outsider),
                "{outsider} must not be in a ws-authored commit: {files}"
            );
        }

        // And the user's own staging area survives untouched.
        let status = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&status.stdout).to_string();
        assert!(status.contains("A  staged.txt"), "the user's staged file stays staged: {status}");
        assert!(status.contains("?? wip.txt"), "and their untracked file stays untracked: {status}");
    }

    /// A partial commit (`git commit -- <path>`) is what C1 scoped the
    /// convenience commit to, and git refuses it outright while a merge is
    /// in progress. Confirm the fixture actually reproduces that git
    /// behavior before relying on it to exercise the `init` fix below.
    #[test]
    fn a_partial_commit_really_does_fail_mid_merge() {
        let d = TempDir::new().unwrap();
        let root = d.path();
        run_git(root, &["init", "-q"]).unwrap();
        run_git(root, &["config", "user.email", "t@t"]).unwrap();
        run_git(root, &["config", "user.name", "t"]).unwrap();
        std::fs::write(root.join("base.txt"), "base").unwrap();
        run_git(root, &["add", "base.txt"]).unwrap();
        run_git(root, &["commit", "-qm", "base"]).unwrap();
        std::fs::write(root.join(".git/MERGE_HEAD"), "0".repeat(40) + "\n").unwrap();
        std::fs::write(root.join("base.txt"), "changed").unwrap();
        run_git(root, &["add", "--", "base.txt"]).unwrap();

        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-qm", "partial", "--", "base.txt"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "a partial commit must fail mid-merge");
        assert_eq!(out.status.code(), Some(128));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("cannot do a partial commit during a merge"), "unexpected stderr: {stderr}");
    }

    /// The merge case. C1's fix scoped the convenience commit to `-- .ws`,
    /// but `git commit -- <path>` is a partial commit and git refuses those
    /// outright mid-merge. `init` must skip the commit rather than fail —
    /// otherwise the workspace exists on disk but is never registered.
    #[test]
    fn init_skips_the_commit_during_an_in_progress_merge() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        let root = d.path();
        run_git(root, &["init", "-q"]).unwrap();
        run_git(root, &["config", "user.email", "t@t"]).unwrap();
        run_git(root, &["config", "user.name", "t"]).unwrap();
        std::fs::write(root.join("base.txt"), "base").unwrap();
        run_git(root, &["add", "base.txt"]).unwrap();
        run_git(root, &["commit", "-qm", "base"]).unwrap();
        // Simulate a merge in progress; git refuses partial commits in this state.
        std::fs::write(root.join(".git/MERGE_HEAD"), "0".repeat(40) + "\n").unwrap();

        init("proj", root, "claude", true).expect("init must not fail mid-merge");
        assert!(root.join(".ws/workspace.toml").is_file(), "the workspace is still created");
    }

    /// Build a repo sitting in a *real* conflicted cherry-pick. git refuses a
    /// partial commit whenever `whence != FROM_COMMIT`, which covers
    /// CHERRY_PICK_HEAD and REVERT_HEAD as well as MERGE_HEAD — so a
    /// MERGE_HEAD-only probe leaves this state failing exactly as before.
    /// Nothing is simulated here: git itself writes CHERRY_PICK_HEAD.
    fn repo_mid_cherry_pick(root: &Path) {
        run_git(root, &["init", "-q", "-b", "main"]).unwrap();
        run_git(root, &["config", "user.email", "t@t"]).unwrap();
        run_git(root, &["config", "user.name", "t"]).unwrap();
        std::fs::write(root.join("f.txt"), "base\n").unwrap();
        run_git(root, &["add", "f.txt"]).unwrap();
        run_git(root, &["commit", "-qm", "base"]).unwrap();

        run_git(root, &["checkout", "-q", "-b", "side"]).unwrap();
        std::fs::write(root.join("f.txt"), "side\n").unwrap();
        run_git(root, &["commit", "-qam", "side change"]).unwrap();

        run_git(root, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(root.join("f.txt"), "main\n").unwrap();
        run_git(root, &["commit", "-qam", "main change"]).unwrap();

        // Conflicts, and leaves the cherry-pick in progress.
        let out = Command::new("git")
            .arg("-C").arg(root)
            .args(["cherry-pick", "side"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "the cherry-pick must conflict for this fixture to mean anything");
        let cherry_head = root.join(".git/CHERRY_PICK_HEAD");
        assert!(cherry_head.exists(), "git must have left CHERRY_PICK_HEAD behind");
    }

    /// I2. The MERGE_HEAD probe was an enumeration of git states, and it was
    /// one short in three directions. Rather than enumerate, the convenience
    /// commit is now non-fatal — the same standard the `add` two lines above
    /// it already held itself to — which closes cherry-pick, revert and
    /// conflicted `rebase --merge` at once, present and future.
    #[test]
    fn init_survives_a_real_conflicted_cherry_pick() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        let root = d.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        repo_mid_cherry_pick(&root);

        // Pre-fix this propagated `fatal: cannot do a partial commit during a
        // cherry-pick` (exit 128) out of `init`, *after* `.ws/` was on disk
        // and *before* `registry::register` ran: a workspace invisible to ws.
        init("proj", &root, "claude", true).expect("init must not fail mid-cherry-pick");

        assert!(root.join(".ws/workspace.toml").is_file(), "the workspace is created");
        assert_eq!(
            registry::lookup("proj").as_deref(),
            Some(root.as_path()),
            "and it is registered — the failure this fix exists to prevent is the unregistered workspace"
        );
        // The user's conflicted cherry-pick is left exactly as it was.
        assert!(root.join(".git/CHERRY_PICK_HEAD").exists(), "ws must not resolve the user's cherry-pick");
    }

    /// The same, for a conflicted revert (REVERT_HEAD) — the third state the
    /// MERGE_HEAD probe missed.
    #[test]
    fn init_survives_a_real_conflicted_revert() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        let root = d.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();

        run_git(&root, &["init", "-q", "-b", "main"]).unwrap();
        run_git(&root, &["config", "user.email", "t@t"]).unwrap();
        run_git(&root, &["config", "user.name", "t"]).unwrap();
        std::fs::write(root.join("f.txt"), "base\n").unwrap();
        run_git(&root, &["add", "f.txt"]).unwrap();
        run_git(&root, &["commit", "-qm", "base"]).unwrap();
        std::fs::write(root.join("f.txt"), "second\n").unwrap();
        run_git(&root, &["commit", "-qam", "second"]).unwrap();
        std::fs::write(root.join("f.txt"), "third\n").unwrap();
        run_git(&root, &["commit", "-qam", "third"]).unwrap();

        // Reverting "second" conflicts because "third" touched the same line.
        let out = Command::new("git")
            .arg("-C").arg(&root)
            .args(["revert", "--no-edit", "HEAD~1"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "the revert must conflict for this fixture to mean anything");
        assert!(root.join(".git/REVERT_HEAD").exists(), "git must have left REVERT_HEAD behind");

        init("proj", &root, "claude", true).expect("init must not fail mid-revert");
        assert!(root.join(".ws/workspace.toml").is_file(), "the workspace is created");
        assert_eq!(registry::lookup("proj").as_deref(), Some(root.as_path()), "and registered");
    }

    #[test]
    fn session_id_roundtrip() {
        let d = TempDir::new().unwrap();
        let state = d.path().join("state.toml");
        assert_eq!(read_session_id(&state, "claude"), None);
        write_session_id(&state, "claude", "abc-123").unwrap();
        assert_eq!(read_session_id(&state, "claude"), Some("abc-123".into()));
        // second agent doesn't clobber the first
        write_session_id(&state, "codex", "xyz").unwrap();
        assert_eq!(read_session_id(&state, "claude"), Some("abc-123".into()));
        assert_eq!(read_session_id(&state, "codex"), Some("xyz".into()));
    }
}
