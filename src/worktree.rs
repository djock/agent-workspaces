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

fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
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
    let dirty = git_ok(path, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        bail!(
            "{} has uncommitted changes — commit or discard them first:\n{}",
            path.display(),
            dirty.trim()
        );
    }
    git_ok(base, &["merge", "--no-ff", "-m", &format!("merge {branch}"), branch])?;
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
}
