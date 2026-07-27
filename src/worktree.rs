use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub base: String,
    pub feature: String,
}

impl Spec {
    pub fn workspace_name(&self) -> String {
        format!("{}@{}", self.base, self.feature)
    }
}

/// `base@feature`. Splits on the FIRST `@` so a branch name may contain one.
/// Both halves must be non-empty; anything else is not a worktree spec and the
/// caller should treat the argument as an ordinary workspace name.
pub fn parse_name(s: &str) -> Option<Spec> {
    let (base, feature) = s.split_once('@')?;
    if base.is_empty() || feature.is_empty() {
        return None;
    }
    Some(Spec { base: base.to_string(), feature: feature.to_string() })
}

fn git_raw(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))
}

/// Both of git's streams, in that order, blank ones dropped.
///
/// C1: `git merge` writes its conflict report ("Auto-merging f.txt",
/// "CONFLICT (content): ...", "Automatic merge failed") to **stdout**, and
/// only "fatal:"-class errors to stderr. Reporting stderr alone produced
/// `ws: git merge … failed: ` — an empty reason — on the single path where the
/// user most needs to be told what happened.
fn combined(out: &std::process::Output) -> String {
    [&out.stdout, &out.stderr]
        .iter()
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_raw(dir, args)?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), combined(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Is a merge in progress in `dir`?
///
/// Resolved via `rev-parse --git-path`, never by joining `.git/MERGE_HEAD`
/// ourselves: in a linked worktree `.git` is a *file* holding a `gitdir:`
/// pointer, and conflating the two produced a Critical in this project's
/// Phase 6.
fn mid_merge(dir: &Path) -> Result<bool> {
    let raw = git_ok(dir, &["rev-parse", "--git-path", "MERGE_HEAD"])?;
    let path = PathBuf::from(raw.trim());
    let resolved = if path.is_absolute() { path } else { dir.join(path) };
    Ok(resolved.exists())
}

/// Untracked files `ws` itself generates in a checkout and deliberately does
/// not commit. They are per-checkout bookkeeping, not the user's work.
///
/// I2: `create` writes both of these into a new worktree, and the dirty check
/// below then refused to merge a worktree in which the user had committed
/// everything they own — the documented `ws base@feature` → `ws base@feature
/// --merge` round trip could not complete from any state a user could reach.
/// Keeping them untracked (rather than committing them) also keeps the base's
/// own untracked `.ws/timeline.jsonl` out of the merge, which was the second
/// half of the same failure.
const WS_BOOKKEEPING: &[&str] = &[".ws/base", ".ws/timeline.jsonl"];

/// `git status --porcelain` minus ws's own untracked bookkeeping. Anything
/// else — modified, staged, or an untracked file the user created — still
/// counts as dirty.
fn user_dirt(porcelain: &str) -> Vec<&str> {
    porcelain
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| match l.strip_prefix("?? ") {
            Some(p) => !WS_BOOKKEEPING.contains(&p.trim_matches('"')),
            None => true,
        })
        .collect()
}

/// `git worktree add -b <branch> <path>` from `base`.
pub fn add_worktree(base: &Path, path: &Path, branch: &str) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let path_s = path.to_string_lossy().to_string();
    git_ok(base, &["worktree", "add", "-b", branch, &path_s])?;
    Ok(())
}

/// Merge `branch` into whatever `base` has checked out, `--no-ff`, then remove
/// the worktree. Refuses if the worktree has uncommitted work: merging would
/// leave the change stranded in a directory this function then deletes.
pub fn merge_worktree(base: &Path, path: &Path, branch: &str) -> Result<()> {
    let porcelain = git_ok(path, &["status", "--porcelain"])?;
    let dirty = user_dirt(&porcelain);
    if !dirty.is_empty() {
        bail!(
            "{} has uncommitted changes — commit or discard them first:\n{}",
            path.display(),
            dirty.join("\n")
        );
    }

    // Refuse before touching a base repository that is already in a merge.
    // Otherwise our failed `git merge` below would observe MERGE_HEAD and the
    // cleanup path would abort a merge the user started, not one ws started.
    if mid_merge(base)? {
        bail!(
            "{} already has a merge in progress — finish or abort it before merging {branch}",
            base.display()
        );
    }

    // Git permits some merges on top of unrelated local edits. That is unsafe
    // for an automated merge followed by worktree deletion: on a later
    // conflict, `merge --abort` cannot promise to reconstruct arbitrary
    // pre-existing changes. Require a clean base, except for ws's exact
    // per-checkout bookkeeping paths.
    let base_porcelain = git_ok(base, &["status", "--porcelain"])?;
    let base_dirty = user_dirt(&base_porcelain);
    if !base_dirty.is_empty() {
        bail!(
            "{} has uncommitted changes — commit or discard them before merging:\n{}",
            base.display(),
            base_dirty.join("\n")
        );
    }

    // Not through `git_ok`: a failure here has already written into a
    // repository `ws` does not own, and must be undone before we bail.
    let out = git_raw(base, &["merge", "--no-ff", "-m", &format!("merge {branch}"), branch])?;
    if !out.status.success() {
        let detail = combined(&out);
        // C1: on a conflict git leaves MERGE_HEAD set, conflict markers in the
        // user's source files and staged adds in the index. Put the base back
        // the way we found it. When the merge refused *before* touching the
        // working tree there is nothing to abort, and `git merge --abort`
        // would fail with "no merge to abort" — so probe first.
        // Always attempt the abort. A failed merge may have touched the index
        // before MERGE_HEAD becomes observable, and probing first creates a
        // second failure path that can skip the only cleanup operation.
        let abort = git_raw(base, &["merge", "--abort"]);
        let note = match abort {
            Ok(o) if o.status.success() => format!(
                "\n{} was left untouched (the merge was aborted and rolled back).",
                base.display()
            ),
            other => match mid_merge(base) {
                Ok(false) => format!("\n{} was left untouched.", base.display()),
                Ok(true) | Err(_) => format!(
                    "\n!! {} is STILL mid-merge — `git merge --abort` there before anything else{}",
                    base.display(),
                    match other {
                        Ok(o) => format!(" (git merge --abort failed: {})", combined(&o)),
                        Err(e) => format!(" ({e})"),
                    }
                ),
            },
        };
        // The worktree is deliberately NOT removed: the work is still on
        // `branch` and the user needs somewhere to resolve the conflict.
        bail!(
            "merging {branch} into {} failed:\n{detail}{note}\n\
             The worktree at {} is still there — resolve the overlap on {branch} \
             (merge the base branch into it and fix the conflict there), then \
             run --merge again.",
            base.display(),
            path.display()
        );
    }

    // The merge landed. Clear ws's own untracked bookkeeping so `git worktree
    // remove` succeeds *without* `--force`: forcing would also delete files
    // the user created, and the whole point of the dirty check above is that
    // nothing of theirs is left here. Anything else untracked still blocks the
    // removal, loudly, which is the behaviour we want.
    for rel in WS_BOOKKEEPING {
        let f = path.join(rel);
        if f.exists() {
            std::fs::remove_file(&f).with_context(|| format!("cannot remove {}", f.display()))?;
        }
    }

    let path_s = path.to_string_lossy().to_string();
    git_ok(base, &["worktree", "remove", &path_s])?;
    Ok(())
}

/// Create `<base>@<feature>`: a git worktree of the base workspace's repo, with
/// its own `.ws/` naming the base, registered under the combined name.
pub fn create(spec: &Spec) -> Result<PathBuf> {
    let cfg = crate::config::load();
    let base_path = crate::registry::lookup_checked(&spec.base)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {}", spec.base))?;
    if !base_path.join(".git").exists() {
        bail!("{} is not a git repository — worktrees need one", base_path.display());
    }
    let name = spec.workspace_name();
    if crate::registry::lookup_checked(&name)?.is_some() {
        bail!("{name} already exists");
    }

    let path = crate::config::sessions_root(&cfg).join(&name);
    add_worktree(&base_path, &path, &spec.feature)?;

    // Minimal .ws/ bootstrap. commit=false: the contract files land in the
    // worktree's working copy and the user commits them with their own work.
    let agent = cfg.default_agent.clone();
    crate::contract::init(&name, &path, &agent, false)?;
    crate::atomic::atomic_write(&path.join(".ws/base"), format!("{}\n", spec.base).as_bytes())?;
    crate::registry::register(&name, &path)?;
    let actor = crate::actors::actor_slug_in(&path);
    crate::timeline::record(
        &path.join(".ws/timeline.jsonl"),
        "worktree-created",
        &actor,
        serde_json::json!({ "base": spec.base, "branch": spec.feature }),
    )?;
    Ok(path)
}

/// Merge the worktree back into its base and remove it.
pub fn merge(spec: &Spec) -> Result<()> {
    let name = spec.workspace_name();
    let path = crate::registry::lookup_checked(&name)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {name}"))?;
    let base_path = crate::registry::lookup_checked(&spec.base)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {}", spec.base))?;

    // live_pid_checked: this deletes a directory. An unreadable lock must stop
    // us, not read as "nobody home". Takes the lock FILE, not the root.
    if let Some(pid) = crate::lock::live_pid_checked(&path.join(".ws/local/lock"))? {
        bail!("{name} is in use by pid {pid} — close it before merging");
    }

    merge_worktree(&base_path, &path, &spec.feature)?;
    crate::registry::unregister(&name)?;
    println!("merged {} into {} and removed the worktree", spec.feature, spec.base);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_at_feature() {
        assert_eq!(
            parse_name("api@retry-logic"),
            Some(Spec { base: "api".into(), feature: "retry-logic".into() })
        );
    }

    #[test]
    fn a_plain_name_is_not_a_worktree_spec() {
        assert_eq!(parse_name("api"), None);
    }

    #[test]
    fn empty_halves_are_rejected() {
        assert_eq!(parse_name("@feature"), None);
        assert_eq!(parse_name("api@"), None);
        assert_eq!(parse_name("@"), None);
    }

    #[test]
    fn only_the_first_at_splits_so_branch_names_may_contain_one() {
        assert_eq!(
            parse_name("api@fix@2"),
            Some(Spec { base: "api".into(), feature: "fix@2".into() })
        );
    }

    #[test]
    fn workspace_name_round_trips() {
        let s = parse_name("api@retry").unwrap();
        assert_eq!(s.workspace_name(), "api@retry");
    }

    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn base_repo(td: &TempDir) -> PathBuf {
        let root = td.path().join("api");
        std::fs::create_dir_all(root.join(".ws/notebook")).unwrap();
        git(td.path(), &["init", "-q", "api"]);
        git(&root, &["config", "user.email", "dev@example.com"]);
        git(&root, &["config", "user.name", "Dev"]);
        std::fs::write(root.join(".ws/README.md"), "# api\n\nObjective: ship it\n").unwrap();
        std::fs::write(
            root.join(".ws/.gitattributes"),
            "notebook/*.md merge=union\ntimeline.jsonl merge=union\n",
        )
        .unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);
        root
    }

    #[test]
    fn add_worktree_creates_a_branch_a_checkout_and_a_ws_dir_naming_its_base() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");

        add_worktree(&base, &wt, "retry").unwrap();

        assert!(wt.join(".git").exists(), "worktree checkout exists");
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "retry");
        let branches = git(&base, &["branch", "--list", "retry"]);
        assert!(branches.contains("retry"), "branch created: {branches}");
    }

    #[test]
    fn merging_brings_the_branch_back_with_a_merge_commit_and_removes_the_worktree() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();

        std::fs::create_dir_all(wt.join(".ws/notebook")).unwrap();
        std::fs::write(wt.join(".ws/notebook/notebook.dev.md"), "found a thing\n").unwrap();
        git(&wt, &["config", "user.email", "dev@example.com"]);
        git(&wt, &["config", "user.name", "Dev"]);
        git(&wt, &["add", ".ws"]);
        git(&wt, &["commit", "-q", "-m", "note"]);

        merge_worktree(&base, &wt, "retry").unwrap();

        assert!(base.join(".ws/notebook/notebook.dev.md").is_file(), "work landed in base");
        let log = git(&base, &["log", "--oneline", "--merges"]);
        assert!(!log.trim().is_empty(), "--no-ff produced a merge commit: {log}");
        assert!(!wt.exists(), "worktree directory removed");
    }

    #[test]
    fn merging_refuses_while_the_worktree_has_uncommitted_changes() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();
        std::fs::write(wt.join("scratch.txt"), "unsaved work\n").unwrap();
        git(&wt, &["add", "scratch.txt"]);

        let err = merge_worktree(&base, &wt, "retry").unwrap_err().to_string();
        assert!(err.contains("uncommitted"), "explains why: {err}");
        // Refusing must not destroy the thing it refused to merge.
        assert!(wt.join("scratch.txt").is_file(), "the worktree survives a refusal");
    }

    /// I2. ws's own untracked bookkeeping must not read as "the user has
    /// uncommitted work" — but nothing else may be waved through.
    #[test]
    fn only_ws_own_untracked_bookkeeping_is_ignored_by_the_dirty_check() {
        assert!(user_dirt("?? .ws/base\n?? .ws/timeline.jsonl\n").is_empty());
        // Untracked user files still count.
        assert_eq!(user_dirt("?? .ws/base\n?? notes.txt\n"), vec!["?? notes.txt"]);
        // A *modified* or *staged* .ws/base is the user's own commit decision,
        // not our bookkeeping — status codes other than `??` are never waived.
        assert_eq!(user_dirt(" M .ws/base\n"), vec![" M .ws/base"]);
        assert_eq!(user_dirt("A  .ws/timeline.jsonl\n"), vec!["A  .ws/timeline.jsonl"]);
        // And a path that merely starts the same way is not the same path.
        assert_eq!(user_dirt("?? .ws/base.bak\n"), vec!["?? .ws/base.bak"]);
    }

    /// C1 (critical), reproduced. A conflicting `--merge` used to leave the
    /// user's base repository mid-merge: MERGE_HEAD set, conflict markers
    /// written into their source files, staged adds in the index — and `ws`
    /// printed `failed: ` with an empty reason, because `git_ok` captured only
    /// stderr while git writes its conflict report to stdout.
    ///
    /// Discriminator, both halves: drop the `merge --abort` and the MERGE_HEAD
    /// assertion fails; go back to `String::from_utf8_lossy(&out.stderr)` and
    /// the "CONFLICT"/non-empty-reason assertions fail.
    #[test]
    fn a_conflicting_merge_leaves_the_base_repo_clean_and_says_why() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();
        git(&wt, &["config", "user.email", "dev@example.com"]);
        git(&wt, &["config", "user.name", "Dev"]);

        // Both sides change the same line of the same file: a real conflict.
        std::fs::write(wt.join("feature.txt"), "real work\n").unwrap();
        git(&wt, &["add", "feature.txt"]);
        git(&wt, &["commit", "-q", "-m", "feature work"]);
        std::fs::write(base.join("feature.txt"), "base version\n").unwrap();
        git(&base, &["add", "feature.txt"]);
        git(&base, &["commit", "-q", "-m", "base version"]);

        let before_head = git(&base, &["rev-parse", "HEAD"]);
        let before_status = git(&base, &["status", "--porcelain"]);

        let err = merge_worktree(&base, &wt, "retry").unwrap_err().to_string();

        // The message must name the conflict, not be empty.
        assert!(!err.trim_end().ends_with("failed:"), "the reason must not be empty: {err:?}");
        assert!(err.contains("CONFLICT"), "git's own conflict report is surfaced: {err}");
        assert!(err.contains("feature.txt"), "and names the conflicting file: {err}");

        // The base repository must be exactly as we found it.
        assert!(!mid_merge(&base).unwrap(), "no merge may be left in progress");
        assert_eq!(git(&base, &["rev-parse", "HEAD"]), before_head, "HEAD unmoved");
        assert_eq!(
            git(&base, &["status", "--porcelain"]),
            before_status,
            "no staged adds, no modified files left behind"
        );
        let content = std::fs::read_to_string(base.join("feature.txt")).unwrap();
        assert_eq!(content, "base version\n", "no conflict markers in the user's file");
        assert!(!content.contains("<<<<<<<"));

        // And the work is not stranded: the worktree survives a refused merge.
        assert!(wt.exists(), "a failed merge must not remove the worktree");
        assert!(wt.join("feature.txt").is_file(), "the branch's work is still there");
    }

    /// C1 review follow-up. Cleanup may only abort a merge ws itself started.
    /// If the base already has MERGE_HEAD, refuse before invoking `git merge`
    /// and preserve the user's in-progress conflict byte for byte.
    #[test]
    fn an_existing_base_merge_is_never_aborted_by_ws() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();
        let base_branch = git(&base, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let base_branch = base_branch.trim();

        // Create an unrelated branch, then conflict it with the checked-out
        // base branch so the base enters a genuine mid-merge state.
        git(&base, &["checkout", "-q", "-b", "other"]);
        std::fs::write(base.join("feature.txt"), "other\n").unwrap();
        git(&base, &["add", "feature.txt"]);
        git(&base, &["commit", "-q", "-m", "other"]);
        git(&base, &["checkout", "-q", base_branch]);
        std::fs::write(base.join("feature.txt"), "base\n").unwrap();
        git(&base, &["add", "feature.txt"]);
        git(&base, &["commit", "-q", "-m", "base"]);
        let conflict = git_raw(&base, &["merge", "other"]).unwrap();
        assert!(!conflict.status.success(), "the fixture must create a conflict");
        assert!(mid_merge(&base).unwrap(), "the fixture must be mid-merge");

        let before_status = git(&base, &["status", "--porcelain"]);
        let before_content = std::fs::read_to_string(base.join("feature.txt")).unwrap();
        let err = merge_worktree(&base, &wt, "retry").unwrap_err().to_string();

        assert!(err.contains("already has a merge in progress"), "{err}");
        assert!(mid_merge(&base).unwrap(), "the user's merge must still be active");
        assert_eq!(git(&base, &["status", "--porcelain"]), before_status);
        assert_eq!(std::fs::read_to_string(base.join("feature.txt")).unwrap(), before_content);

        git(&base, &["merge", "--abort"]);
    }

    /// `rev-parse --git-path` must resolve MERGE_HEAD for a linked worktree,
    /// where `.git` is a pointer file rather than a directory.
    #[test]
    fn mid_merge_detects_a_merge_inside_a_linked_worktree() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();
        let base_branch = git(&base, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let base_branch = base_branch.trim();

        std::fs::write(wt.join("feature.txt"), "worktree\n").unwrap();
        git(&wt, &["add", "feature.txt"]);
        git(&wt, &["commit", "-q", "-m", "worktree"]);
        std::fs::write(base.join("feature.txt"), "base\n").unwrap();
        git(&base, &["add", "feature.txt"]);
        git(&base, &["commit", "-q", "-m", "base"]);

        let conflict = git_raw(&wt, &["merge", base_branch]).unwrap();
        assert!(!conflict.status.success(), "the fixture must create a conflict");
        assert!(wt.join(".git").is_file(), "linked worktree .git is a file");
        assert!(mid_merge(&wt).unwrap(), "MERGE_HEAD must resolve through the gitdir pointer");

        git(&wt, &["merge", "--abort"]);
    }

    #[test]
    fn merging_refuses_a_dirty_base_before_git_can_touch_it() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();
        std::fs::write(base.join("local.txt"), "keep me\n").unwrap();

        let err = merge_worktree(&base, &wt, "retry").unwrap_err().to_string();

        assert!(err.contains("uncommitted changes"), "{err}");
        assert!(base.join("local.txt").is_file(), "the user's local file survives");
        assert!(!mid_merge(&base).unwrap(), "no merge was attempted");
        assert!(wt.exists(), "the worktree survives the refusal");
    }

    /// I7 — the end-to-end test whose absence let C1 and I2 both ship. Every
    /// other git test here hand-builds its fixture and so verifies a shape
    /// `create` never produces. This one drives the real product path:
    /// `worktree::create` → commit work → `worktree::merge`, with no manual
    /// git steps in between, which is precisely the documented workflow.
    ///
    /// Pre-fix this failed twice over: `create`'s own untracked `.ws/base` and
    /// `.ws/timeline.jsonl` tripped the dirty check, and after committing them
    /// by hand the base's untracked `.ws/timeline.jsonl` blocked the merge.
    #[test]
    fn create_then_merge_round_trips_with_no_manual_git_steps() {
        // TEST_LOCK first so it drops LAST, after the TempDirs: this test
        // mutates process-global env (XDG_CONFIG_HOME, WS_ROOT) that the
        // registry and sessions_root read.
        static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let home = TempDir::new().unwrap();
        let td = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
        std::env::set_var("WS_ROOT", td.path());

        // A base workspace exactly as ws leaves one: contract::init with the
        // convenience commit, then an untracked timeline.jsonl on top.
        let base = td.path().join("api");
        std::fs::create_dir_all(&base).unwrap();
        git(td.path(), &["init", "-q", "api"]);
        git(&base, &["config", "user.email", "dev@example.com"]);
        git(&base, &["config", "user.name", "Dev"]);
        crate::contract::init("api", &base, "claude", true).unwrap();
        assert!(
            base.join(".ws/timeline.jsonl").is_file(),
            "the fixture must reproduce the untracked timeline that broke the merge"
        );

        let spec = Spec { base: "api".into(), feature: "retry".into() };
        let wt = create(&spec).unwrap();
        assert!(wt.join(".ws/base").is_file(), "create wrote its bookkeeping");
        assert_eq!(crate::registry::lookup("api@retry").as_deref(), Some(wt.as_path()));

        // The user does their work and commits it. Nothing else.
        git(&wt, &["config", "user.email", "dev@example.com"]);
        git(&wt, &["config", "user.name", "Dev"]);
        std::fs::write(wt.join("feature.txt"), "real work\n").unwrap();
        git(&wt, &["add", "feature.txt"]);
        git(&wt, &["commit", "-q", "-m", "feature work"]);

        merge(&spec).expect("the documented round trip must complete with no manual git steps");

        assert_eq!(
            std::fs::read_to_string(base.join("feature.txt")).unwrap(),
            "real work\n",
            "the work landed in the base"
        );
        let log = git(&base, &["log", "--oneline", "--merges"]);
        assert!(!log.trim().is_empty(), "--no-ff produced a merge commit: {log}");
        assert!(!wt.exists(), "the worktree is removed");
        assert_eq!(crate::registry::lookup("api@retry"), None, "and unregistered");

        std::env::remove_var("WS_ROOT");
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
