//! One git wrapper for the whole crate.
//!
//! There used to be three, with six invocation styles between them, and they did
//! not agree on what an error says. `contract.rs`'s reported **stderr only** —
//! which is exactly the bug the `combined` doc comment below describes, so
//! `contract::init`'s failures could surface as `git … failed: ` with no reason
//! at all. Having one wrapper means the reporting rule is stated once and cannot
//! silently regress in the modules that forgot it.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Run git and hand back the raw output, success or not.
pub fn raw(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))
}

/// Both of git's streams, in that order, blank ones dropped.
///
/// `git merge` writes its conflict report ("Auto-merging f.txt", "CONFLICT
/// (content): …", "Automatic merge failed") to **stdout**, and only
/// "fatal:"-class errors to stderr. Reporting stderr alone produced
/// `ws: git merge … failed: ` — an empty reason — on the single path where the
/// user most needs to be told what happened.
pub fn combined(out: &std::process::Output) -> String {
    [&out.stdout, &out.stderr]
        .iter()
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run git, require success, return stdout.
pub fn ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = raw(dir, args)?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), combined(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Run git and return stdout on success, `None` on any failure.
///
/// For the genuinely optional reads — the current branch for the status line,
/// the actor slug from `user.email` — where "not a repo" and "git absent" are
/// ordinary states, not errors to report.
pub fn maybe(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> TempDir {
        let d = TempDir::new().unwrap();
        ok(d.path(), &["init", "-q"]).unwrap();
        ok(d.path(), &["config", "user.email", "dev@example.com"]).unwrap();
        ok(d.path(), &["config", "user.name", "Dev"]).unwrap();
        d
    }

    #[test]
    fn ok_returns_stdout() {
        let d = repo();
        let out = ok(d.path(), &["config", "user.email"]).unwrap();
        assert_eq!(out.trim(), "dev@example.com");
    }

    #[test]
    fn ok_fails_with_a_reason() {
        let d = repo();
        let err = ok(d.path(), &["rev-parse", "HEAD"]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("rev-parse"), "{msg}");
        // The discriminator: the reason must not be empty. A stderr-only
        // wrapper produced "git rev-parse HEAD failed: " here.
        assert!(!msg.trim_end().ends_with("failed:"), "empty reason: {msg}");
    }

    /// The reason `combined` exists: a git command whose diagnostics go to
    /// stdout must still produce a non-empty reason.
    #[test]
    fn combined_includes_stdout_not_only_stderr() {
        let out = std::process::Output {
            status: std::process::Command::new("false").status().unwrap(),
            stdout: b"CONFLICT (content): Merge conflict in f.txt\n".to_vec(),
            stderr: Vec::new(),
        };
        assert!(combined(&out).contains("CONFLICT"));
    }

    #[test]
    fn combined_drops_blank_streams() {
        let out = std::process::Output {
            status: std::process::Command::new("true").status().unwrap(),
            stdout: b"   \n".to_vec(),
            stderr: b"fatal: nope\n".to_vec(),
        };
        assert_eq!(combined(&out), "fatal: nope");
    }

    #[test]
    fn maybe_is_none_outside_a_repo() {
        let d = TempDir::new().unwrap();
        assert_eq!(maybe(d.path(), &["rev-parse", "--abbrev-ref", "HEAD"]), None);
    }

    #[test]
    fn maybe_is_none_for_empty_output() {
        let d = repo();
        // A config key that is not set exits non-zero and prints nothing.
        assert_eq!(maybe(d.path(), &["config", "ws.nosuchkey"]), None);
    }
}
