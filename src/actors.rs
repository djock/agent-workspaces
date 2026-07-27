use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Slugify an identifier: lowercase, non-alphanumerics → '-', collapse repeats, trim.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Actor slug for a specific directory: the git `user.email` configured there,
/// falling back to `$USER`. Taking the directory explicitly matters because
/// `ws -whoami <name>` may run from anywhere.
pub fn actor_slug_in(dir: &std::path::Path) -> String {
    if let Ok(o) = Command::new("git")
        .args(["config", "user.email"])
        .current_dir(dir)
        .output()
    {
        if o.status.success() {
            let email = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !email.is_empty() {
                return slugify(&email);
            }
        }
    }
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() {
            return slugify(&u);
        }
    }
    "unknown".to_string()
}

pub fn actor_slug() -> String {
    match std::env::current_dir() {
        Ok(d) => actor_slug_in(&d),
        Err(_) => "unknown".to_string(),
    }
}

/// Actors who have committed to `ws_dir`, ranked by commit count (descending,
/// then by slug for a stable order). Errors when the history cannot be read —
/// "unreadable" must not be reported to the user as "nobody".
pub fn who(ws_dir: &Path) -> Result<Vec<(String, usize)>> {
    let repo = match ws_dir.parent() {
        Some(p) => p,
        None => bail!("{} has no parent directory", ws_dir.display()),
    };
    let out = Command::new("git")
        .args(["log", "--format=%ae", "--"])
        .arg(ws_dir)
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        bail!(
            "cannot read git history for {}: {}",
            ws_dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let email = line.trim();
        if email.is_empty() {
            continue;
        }
        *counts.entry(slugify(email)).or_insert(0) += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn slug_is_nonempty_and_lowercase() {
        let s = actor_slug();
        assert!(!s.is_empty());
        assert_eq!(s, s.to_lowercase());
        assert!(!s.contains(' '));
    }
    #[test]
    fn slugify_rules() {
        assert_eq!(slugify("Im.Ionut@Gmail.com"), "im-ionut-gmail-com");
        assert_eq!(slugify("a__b"), "a-b");
    }

    #[test]
    fn actor_slug_in_reads_the_given_repos_email_not_the_cwds() {
        let td = TempDir::new().unwrap();
        let repo = td.path();
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "Someone.Else@Example.COM"]);
        assert_eq!(actor_slug_in(repo), "someone-else-example-com");
    }

    #[test]
    fn actor_slug_in_falls_back_when_the_dir_is_not_a_repo() {
        let td = TempDir::new().unwrap();
        // No git repo here, and no user.email to find. The fallback must still
        // produce a usable slug rather than an empty string.
        let s = actor_slug_in(td.path());
        assert!(!s.is_empty());
        assert_eq!(s, s.to_lowercase());
    }

    #[test]
    fn who_ranks_actors_by_commit_count_in_the_ws_dir() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        run_git(root, &["init", "-q"]);
        std::fs::create_dir_all(root.join(".ws/notebook")).unwrap();

        // Two commits from alice, one from bob, all touching .ws/.
        for (i, (name, email)) in [
            ("Alice", "alice@example.com"),
            ("Alice", "alice@example.com"),
            ("Bob", "bob@example.com"),
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(root.join(format!(".ws/notebook/n{i}.md")), "x").unwrap();
            run_git(root, &["config", "user.name", name]);
            run_git(root, &["config", "user.email", email]);
            run_git(root, &["add", ".ws"]);
            run_git(root, &["commit", "-q", "-m", "note"]);
        }
        // A commit outside .ws/ must not count.
        std::fs::write(root.join("unrelated.txt"), "x").unwrap();
        run_git(root, &["config", "user.email", "carol@example.com"]);
        run_git(root, &["add", "unrelated.txt"]);
        run_git(root, &["commit", "-q", "-m", "unrelated"]);

        let ranked = who(&root.join(".ws")).unwrap();
        assert_eq!(ranked, vec![("alice-example-com".to_string(), 2), ("bob-example-com".to_string(), 1)]);
    }

    #[test]
    fn who_on_a_non_repo_is_an_error_not_an_empty_list() {
        // An empty list means "nobody has worked here", which is a real answer.
        // "I could not read the history" is a different answer and must not be
        // flattened into the first one.
        let td = TempDir::new().unwrap();
        assert!(who(&td.path().join(".ws")).is_err());
    }
}
