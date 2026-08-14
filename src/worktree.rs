use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

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

/// Is a merge in progress in `dir`?
///
/// Resolved via `rev-parse --git-path`, never by joining `.git/MERGE_HEAD`
/// ourselves: in a linked worktree `.git` is a *file* holding a `gitdir:`
/// pointer, and conflating the two produced a Critical in this project's
/// Phase 6.
fn mid_merge(dir: &Path) -> Result<bool> {
    let raw = crate::git::ok(dir, &["rev-parse", "--git-path", "MERGE_HEAD"])?;
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
    crate::git::ok(base, &["worktree", "add", "-b", branch, &path_s])?;
    Ok(())
}

/// Merge `branch` into whatever `base` has checked out, `--no-ff`, then remove
/// the worktree. Refuses if the worktree has uncommitted work: merging would
/// leave the change stranded in a directory this function then deletes.
pub fn merge_worktree(base: &Path, path: &Path, branch: &str) -> Result<()> {
    let porcelain = crate::git::ok(path, &["status", "--porcelain"])?;
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
    let base_porcelain = crate::git::ok(base, &["status", "--porcelain"])?;
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
    let out =
        crate::git::raw(base, &["merge", "--no-ff", "-m", &format!("merge {branch}"), branch])?;
    if !out.status.success() {
        let detail = crate::git::combined(&out);
        // C1: on a conflict git leaves MERGE_HEAD set, conflict markers in the
        // user's source files and staged adds in the index. Put the base back
        // the way we found it. When the merge refused *before* touching the
        // working tree there is nothing to abort, and `git merge --abort`
        // would fail with "no merge to abort" — so probe first.
        // Always attempt the abort. A failed merge may have touched the index
        // before MERGE_HEAD becomes observable, and probing first creates a
        // second failure path that can skip the only cleanup operation.
        let abort = crate::git::raw(base, &["merge", "--abort"]);
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
                        Ok(o) =>
                            format!(" (git merge --abort failed: {})", crate::git::combined(&o)),
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
    crate::git::ok(base, &["worktree", "remove", &path_s])?;
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

    // I1: validate BEFORE any git mutation, not after. `contract::init` and
    // `registry::register` (called from `finish_create` below) both call
    // `validate_name` too, but by the time either runs, `add_worktree` has
    // already created a real branch and checkout in the *user's* repository —
    // a mutation neither of those chokepoints can undo just by returning an
    // error. `ws 'api@$(x)'` used to leave exactly that behind: a branch and
    // worktree only `git` could see, with no registry entry naming either.
    crate::workspace::validate_name(&spec.base)?;
    crate::workspace::validate_name(&name)?;

    // Same gate every other mutating entry point takes: a base written by a
    // newer ws must be refused before this binary creates a branch in it.
    crate::contract::check_gate(&spec.base, &base_path.join(".ws/workspace.toml"))?;

    let path = crate::config::sessions_root(&cfg).join(&name);
    add_worktree(&base_path, &path, &spec.feature)?;

    // From here on `add_worktree` has already mutated `base_path`. Any error
    // in the rest of setup must undo that mutation — best-effort — before
    // this function returns, or a bootstrap failure (a corrupt registry, an
    // unusual committed `.ws/` in the base, disk pressure, ...) leaves behind
    // exactly the kind of orphan the validation above exists to prevent.
    if let Err(e) = finish_create(spec, &name, &path, &base_path, &cfg) {
        rollback_created_worktree(&base_path, &path, &spec.feature, &name);
        return Err(e);
    }
    Ok(path)
}

/// Everything `create` does after `add_worktree`. Split out so `create` can
/// wrap the whole sequence in one rollback call instead of repeating cleanup
/// at every `?`.
fn finish_create(
    spec: &Spec,
    name: &str,
    path: &Path,
    base_path: &Path,
    cfg: &crate::config::Config,
) -> Result<()> {
    // Minimal .ws/ bootstrap. commit=false: the contract files land in the
    // worktree's working copy and the user commits them with their own work.
    //
    // The agent is inherited from the base, not taken from the config: a feature
    // worktree is the same project on a branch, so it must open on the agent that
    // project is already on. Stamping `cfg.default_agent` put a Codex workspace's
    // worktrees on Claude.
    let agent = crate::meta::read(&base_path.join(".ws/workspace.toml"))
        .default_agent
        .unwrap_or_else(|| cfg.default_agent.clone());
    crate::contract::init(name, path, &agent, false)?;
    // `contract::init` uses `write_if_absent`, and a base that committed its
    // `.ws/` hands the worktree checkout a `workspace.toml` before this ever
    // runs — so the agent argument above is inert in exactly the common case,
    // and the checked-in value can be months out of date. Correct it here.
    //
    // Only when it actually differs. `.ws/workspace.toml` is tracked (see
    // `contract::init`'s commit step), so an unconditional write would leave
    // every freshly created worktree dirty and make `ws base@feature --merge`
    // refuse until the user committed a line ws wrote for them. When the value
    // does differ the write is worth that cost — it is the difference between
    // the worktree opening Codex and opening Claude — and `.ws/` in a new
    // worktree is meant to be committed with the user's first work anyway.
    let child_toml = path.join(".ws/workspace.toml");
    if crate::meta::read(&child_toml).default_agent.as_deref() != Some(agent.as_str()) {
        crate::meta::set_default_agent(&child_toml, &agent)?;
    }
    crate::atomic::atomic_write(&path.join(".ws/base"), format!("{}\n", spec.base).as_bytes())?;
    crate::registry::register(name, path)?;
    let actor = crate::actors::actor_slug_in(path);
    crate::timeline::record(
        &path.join(".ws/timeline.jsonl"),
        "worktree-created",
        &actor,
        serde_json::json!({ "base": spec.base, "branch": spec.feature }),
    )?;
    Ok(())
}

/// Best-effort rollback for a `create` abandoned after `add_worktree` already
/// mutated `base`: removes the checkout and the branch it created, and
/// unregisters `name` in case `registry::register` (inside `finish_create`)
/// ran before a later step failed. Every step here is independent of the
/// others — one failing must not skip the rest — and every failure is a
/// warning on stderr, never a returned error: the ORIGINAL error from
/// `create` is always what the caller sees; this function's whole job is to
/// clean up after it, not to compete with it.
fn rollback_created_worktree(base: &Path, path: &Path, branch: &str, name: &str) {
    // `--force`: ws's own not-yet-committed bootstrap files (`create` passes
    // commit=false to `contract::init`) would otherwise read as "uncommitted
    // changes" and block a plain `remove`.
    let path_s = path.to_string_lossy().to_string();
    match crate::git::raw(base, &["worktree", "remove", "--force", &path_s]) {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "ws: warning: rollback could not remove worktree {}: {}",
            path.display(),
            crate::git::combined(&out)
        ),
        Err(e) => eprintln!(
            "ws: warning: rollback could not run `git worktree remove` for {}: {e:#}",
            path.display()
        ),
    }

    match crate::git::raw(base, &["branch", "-D", branch]) {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "ws: warning: rollback could not delete branch {branch}: {}",
            crate::git::combined(&out)
        ),
        Err(e) => eprintln!("ws: warning: rollback could not run `git branch -D {branch}`: {e:#}"),
    }

    // Unregister LAST: while the checkout above still exists, the registry
    // entry is the only handle `ws -rm` has on it. Dropping the entry first
    // would turn a failed `git worktree remove` into an orphan ws can no
    // longer see.
    if let Err(e) = crate::registry::unregister(name) {
        eprintln!("ws: warning: rollback could not unregister {name}: {e:#}");
    }
}

/// Merge the worktree back into its base and remove it.
pub fn merge(spec: &Spec) -> Result<()> {
    let name = spec.workspace_name();
    let path = crate::registry::lookup_checked(&name)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {name}"))?;
    let base_path = crate::registry::lookup_checked(&spec.base)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {}", spec.base))?;

    // Merge rewrites both sides; refuse if either was written by a newer ws.
    crate::contract::check_gate(&name, &path.join(".ws/workspace.toml"))?;
    crate::contract::check_gate(&spec.base, &base_path.join(".ws/workspace.toml"))?;

    // live_pid_checked: this deletes a directory. An unreadable lock must stop
    // us, not read as "nobody home". Takes the lock FILE, not the root.
    if let Some(pid) = crate::lock::live_pid_checked(&path.join(".ws/local/lock"))? {
        bail!("{name} is in use by pid {pid} — close it before merging");
    }
    // The BASE side needs checking too, and it did not used to be: a merge
    // rewrites the base's working tree, so doing it under a live agent pulls
    // files out from under whoever is editing them. Only the feature side was
    // ever checked.
    if let Some(pid) = crate::lock::live_pid_checked(&base_path.join(".ws/local/lock"))? {
        bail!(
            "{} is in use by pid {pid} — merging rewrites its working tree, so close it first",
            spec.base
        );
    }

    merge_worktree(&base_path, &path, &spec.feature)?;
    crate::registry::unregister(&name)?;
    println!("merged {} into {} and removed the worktree", spec.feature, spec.base);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // One module-level lock, not one per test fn: fn-local statics are
    // distinct mutexes, so tests that mutate process-global env (WS_ROOT,
    // XDG_CONFIG_HOME) would not serialize against each other.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
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
        let conflict = crate::git::raw(&base, &["merge", "other"]).unwrap();
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

        let conflict = crate::git::raw(&wt, &["merge", base_branch]).unwrap();
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

    /// I1, part (a). `ws 'api@$(x)'` used to run `add_worktree` — a real
    /// branch + checkout in the base repo — before `contract::init` ever
    /// validated the name, leaving an orphan only `git` could see. Validation
    /// must now happen before any git mutation, so the branch and checkout
    /// are never created in the first place.
    #[test]
    fn create_refuses_an_invalid_name_before_touching_git() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let home = TempDir::new().unwrap();
        let td = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
        std::env::set_var("WS_ROOT", td.path());

        let base = td.path().join("api");
        std::fs::create_dir_all(&base).unwrap();
        git(td.path(), &["init", "-q", "api"]);
        git(&base, &["config", "user.email", "dev@example.com"]);
        git(&base, &["config", "user.name", "Dev"]);
        crate::contract::init("api", &base, "claude", true).unwrap();

        // The exact repro from the brief: a feature half containing shell
        // metacharacters. Valid as a git branch name, invalid as a workspace
        // name — the gap the old ordering fell through.
        let spec = Spec { base: "api".into(), feature: "$(x)".into() };
        let err = create(&spec).unwrap_err().to_string();
        assert!(err.contains("invalid workspace name"), "{err}");

        let wt = td.path().join("api@$(x)");
        assert!(!wt.exists(), "no worktree directory was ever created: {err}");
        let branches = git(&base, &["branch", "--list"]);
        assert!(!branches.contains("$(x)"), "no orphan branch created: {branches}");
        let worktrees = git(&base, &["worktree", "list"]);
        assert_eq!(worktrees.lines().count(), 1, "only the base checkout is listed: {worktrees}");
        assert_eq!(crate::registry::lookup("api@$(x)"), None, "nothing registered");

        std::env::remove_var("WS_ROOT");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    /// I1, part (b). Once `add_worktree` has mutated the base repository, a
    /// later failure must undo it — best-effort — rather than leave an
    /// orphan branch + checkout behind. Forced here by committing
    /// `.ws/notebook` into the base as a plain FILE rather than the directory
    /// `contract::init` expects: `add_worktree` succeeds (it only checks out
    /// whatever the base's HEAD already has), and `contract::init`'s
    /// `create_dir_all` on that same path then fails because a file already
    /// occupies it — a bootstrap failure with nothing to do with the name.
    #[test]
    fn create_rolls_back_the_branch_and_worktree_when_a_later_step_fails() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let home = TempDir::new().unwrap();
        let td = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
        std::env::set_var("WS_ROOT", td.path());

        let base = td.path().join("api");
        std::fs::create_dir_all(base.join(".ws")).unwrap();
        git(td.path(), &["init", "-q", "api"]);
        git(&base, &["config", "user.email", "dev@example.com"]);
        git(&base, &["config", "user.name", "Dev"]);
        std::fs::write(base.join(".ws/notebook"), "not a directory\n").unwrap();
        git(&base, &["add", "."]);
        git(&base, &["commit", "-q", "-m", "init"]);
        crate::registry::register("api", &base).unwrap();

        let spec = Spec { base: "api".into(), feature: "retry".into() };
        let err = create(&spec).unwrap_err().to_string();
        assert!(!err.trim().is_empty());

        let wt = td.path().join("api@retry");
        assert!(!wt.exists(), "the worktree directory was rolled back: {err}");
        let worktrees = git(&base, &["worktree", "list"]);
        assert_eq!(worktrees.lines().count(), 1, "no dangling worktree entry: {worktrees}");
        let branches = git(&base, &["branch", "--list", "retry"]);
        assert!(branches.trim().is_empty(), "the orphan branch was deleted: {branches}");
        assert_eq!(crate::registry::lookup("api@retry"), None, "and nothing left registered");

        std::env::remove_var("WS_ROOT");
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
