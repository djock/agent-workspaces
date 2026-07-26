//! Import `cs` sessions (`~/.claude-sessions/<name>`) into ws workspaces.
//!
//! Copies, never moves: cs stays fully usable afterwards and nothing is ever
//! written into `~/.claude-sessions`.
//!
//! A cs entry may be a SYMLINK to a live project directory (adopted sessions —
//! 8 of 20 on the author's machine). Those are migrated *in place*: `.ws/` is
//! created inside the real project next to the existing `.cs/`. Only a genuine
//! session directory is copied.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub name: String,
    /// The cs session directory (symlink resolved).
    pub source: PathBuf,
    /// Where the ws workspace will live.
    pub dest: PathBuf,
    /// True when dest is an existing project we must not copy into place.
    pub in_place: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct Frontmatter {
    pub archived: bool,
    pub tags: Vec<String>,
}

/// `$WS_CS_ROOT` (test seam) or `~/.claude-sessions`.
pub fn cs_root() -> PathBuf {
    if let Some(p) = std::env::var_os("WS_CS_ROOT") {
        return PathBuf::from(p);
    }
    dirs::home_dir().unwrap_or_default().join(".claude-sessions")
}

/// Every entry under `root` that looks like a cs session (has a `.cs/` dir),
/// plus a second list of entries that are symlinks whose target does not
/// resolve (dangling — e.g. the linked project was moved or deleted). A
/// dangling entry is real but broken: it must never be silently dropped or
/// misreported as "no cs session named X".
pub type DiscoveredSessions = Vec<(String, PathBuf)>;

pub fn discover(root: &Path) -> (DiscoveredSessions, DiscoveredSessions) {
    let mut out = Vec::new();
    let mut broken = Vec::new();
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(_) => return (out, broken),
    };
    for e in rd.flatten() {
        let path = e.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        let is_symlink = std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink && std::fs::metadata(&path).is_err() {
            // Symlink whose target doesn't resolve.
            let target = std::fs::read_link(&path).unwrap_or_default();
            broken.push((name, target));
            continue;
        }
        // `path.join(".cs")` follows symlinks, which is what we want here.
        if path.join(".cs").is_dir() {
            out.push((name, path));
        }
    }
    out.sort();
    broken.sort();
    (out, broken)
}

pub fn plan_for(name: &str, entry: &Path, sessions_root: &Path) -> Result<Plan> {
    let meta = std::fs::symlink_metadata(entry)
        .with_context(|| format!("cannot stat {}", entry.display()))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::canonicalize(entry)
            .with_context(|| format!("dangling symlink: {}", entry.display()))?;
        return Ok(Plan {
            name: name.to_string(),
            source: target.clone(),
            dest: target,
            in_place: true,
        });
    }
    Ok(Plan {
        name: name.to_string(),
        source: entry.to_path_buf(),
        dest: sessions_root.join(name),
        in_place: false,
    })
}

/// Parse the YAML frontmatter cs writes at the top of `.cs/README.md`.
/// Only the two fields ws cares about; anything else is ignored.
pub fn parse_frontmatter(readme: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut lines = readme.lines();
    if lines.next().map(str::trim) != Some("---") {
        return fm;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("status:") {
            fm.archived = v.trim() == "archived";
        }
        if let Some(v) = line.strip_prefix("tags:") {
            fm.tags = v
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    fm
}

/// Recursively copy `src` into `dst`, skipping any path for which `skip` is true.
/// `root` is the session root, used only to render skipped-symlink paths in
/// `log` relative to something the user recognizes.
fn copy_tree(
    src: &Path,
    dst: &Path,
    root: &Path,
    skip: &dyn Fn(&Path) -> bool,
    log: &mut Vec<String>,
) -> Result<()> {
    if skip(src) {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        // Don't chase symlinks inside a session tree — but say so, rather
        // than silently producing a copy that's missing a link the source had.
        let rel = src.strip_prefix(root).unwrap_or(src);
        log.push(format!("  WARNING: skipped symlink: {}", rel.display()));
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            copy_tree(&e.path(), &dst.join(e.file_name()), root, skip, log)?;
        }
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Canonicalize `path`, walking up to the nearest existing ancestor when
/// `path` itself doesn't exist yet (the common case for a migration
/// destination) and re-joining the trailing components that were walked
/// past. Without this, comparing a resolved source against an unresolved,
/// not-yet-created destination can miss a real nesting relationship — e.g.
/// on macOS, where temp dirs live under a symlink (`/var` -> `/private/var`),
/// same precedent as `commands::rm`'s prefix check.
fn canonicalize_nearest(path: &Path) -> PathBuf {
    let mut ancestor = path.to_path_buf();
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(resolved) = ancestor.canonicalize() {
            let mut result = resolved;
            for comp in trailing.iter().rev() {
                result.push(comp);
            }
            return result;
        }
        match ancestor.file_name() {
            Some(name) => trailing.push(name.to_os_string()),
            None => return path.to_path_buf(),
        }
        if !ancestor.pop() {
            return path.to_path_buf();
        }
    }
}

/// Copy one file if it exists. Returns whether it was copied.
fn copy_file(src: &Path, dst: &Path) -> Result<bool> {
    if !src.is_file() {
        return Ok(false);
    }
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::copy(src, dst)?;
    Ok(true)
}

/// Execute a plan. Returns the log lines to print.
pub fn migrate(plan: &Plan, default_agent: &str, dry_run: bool) -> Result<Vec<String>> {
    let mut log = Vec::new();
    let cs = plan.source.join(".cs");
    if !cs.is_dir() {
        anyhow::bail!("{} has no .cs directory", plan.source.display());
    }

    // C1: a copy-path session whose `.git` is a *file* (not a directory, not a
    // symlink) is a linked git worktree — the file contains a `gitdir:`
    // pointer into some other repo's `.git/worktrees/<name>` admin dir.
    // copy_tree would copy that pointer verbatim, so the destination and the
    // original worktree end up sharing one admin dir in a third repo the
    // migration was never meant to touch: git commands in either rewrite the
    // other's HEAD/index. Refuse this session rather than attempt to
    // reconstruct the worktree — a per-session refusal, so `--all` still
    // proceeds with the rest, and this runs before the dry_run early return
    // so `--dry-run` surfaces it too.
    if !plan.in_place {
        let dot_git = plan.source.join(".git");
        if std::fs::symlink_metadata(&dot_git).map(|m| m.is_file()).unwrap_or(false) {
            anyhow::bail!(
                "refusing to migrate {}: {} is a linked git worktree pointer (a file, not a repo) — copying it would let two checkouts share one worktree admin dir; migrate the real repo instead",
                plan.source.display(),
                dot_git.display()
            );
        }
    }

    // I7: dest must never be the source, or nested inside it — copying a tree
    // into itself either truncates a file (self-copy) or recurses forever.
    // Canonicalize before comparing: on macOS temp dirs live under a symlink
    // (/var -> /private/var), same precedent as commands::rm's prefix check.
    if !plan.in_place {
        let src_c = plan.source.canonicalize().unwrap_or_else(|_| plan.source.clone());
        // dest almost never exists yet on a fresh migration, so canonicalize
        // the nearest existing ancestor instead of falling back to the raw,
        // unresolved path (see canonicalize_nearest doc comment).
        let dest_c = canonicalize_nearest(&plan.dest);
        if dest_c == src_c || dest_c.starts_with(&src_c) {
            anyhow::bail!(
                "refusing to migrate: destination {} is the source {} or nested inside it",
                plan.dest.display(),
                plan.source.display()
            );
        }
    }

    if plan.dest.join(".ws").is_dir() {
        anyhow::bail!("{} is already a ws workspace (skipping)", plan.dest.display());
    }

    // I2: a name collision in the registry must never silently repoint an
    // existing, unrelated workspace.
    if let Some(existing) = crate::registry::lookup(&plan.name) {
        if existing != plan.dest {
            anyhow::bail!(
                "{} is already registered to {} (refusing to repoint it to {})",
                plan.name,
                existing.display(),
                plan.dest.display()
            );
        }
    }

    // C1: an existing, non-empty, non-workspace directory at the destination
    // must never be silently merged into and overwritten. Only applies to the
    // copy path — in_place's destination is the live project, always non-empty.
    if !plan.in_place && plan.dest.exists() {
        if plan.dest.is_file() {
            anyhow::bail!(
                "refusing to migrate into {}: a file already exists there",
                plan.dest.display()
            );
        }
        // Fail closed: an unreadable destination (e.g. EACCES) must be treated
        // as "assume non-empty, refuse", not "assume empty, proceed" — this
        // guard exists specifically to refuse, so an unknown state must refuse.
        // But say so accurately: "could not read" is a different cause than
        // "non-empty", and the refusal message should not misattribute one
        // for the other.
        match std::fs::read_dir(&plan.dest) {
            Ok(mut rd) => {
                if rd.next().is_some() {
                    anyhow::bail!(
                        "refusing to migrate into non-empty directory {} (not a ws workspace)",
                        plan.dest.display()
                    );
                }
            }
            Err(e) => {
                anyhow::bail!(
                    "refusing to migrate into {}: could not read directory to confirm it is empty: {e:#}",
                    plan.dest.display()
                );
            }
        }
    }

    if dry_run {
        log.push(format!(
            "would migrate {} → {} ({})",
            plan.source.display(),
            plan.dest.display(),
            if plan.in_place { "in place — symlinked project" } else { "copy" }
        ));
        return Ok(log);
    }

    // 1. Materialize the destination.
    if plan.in_place {
        log.push(format!("{}: adopting in place at {}", plan.name, plan.dest.display()));
    } else {
        // Copy the whole session tree except .cs (mapped below) — project files,
        // .git history and all.
        let cs_dir = cs.clone();
        copy_tree(&plan.source, &plan.dest, &plan.source, &|p: &Path| p == cs_dir, &mut log)?;
        log.push(format!("{}: copied {} → {}", plan.name, plan.source.display(), plan.dest.display()));
    }

    // 2. ws skeleton (write_if_absent — never clobbers what we copy in next),
    //    registry entry, git init when needed. This step and anything above
    //    stays fatal: past this point the workspace exists and is registered,
    //    so a half-done migration must still be reported as such, not lost.
    crate::contract::init(&plan.name, &plan.dest, default_agent, /* commit */ false)?;
    let ws = plan.dest.join(".ws");

    // 3. Map cs files onto the ws contract. NOTE: .cs/local/ is never copied —
    //    it holds the bash audit log, limits, state and the lock.
    // I3: from here on, a failure in any one mapping step must not abort the
    // whole migration — the workspace is already created and registered, so
    // the run must finish and report exactly what it could and couldn't do.
    let readme = cs.join("README.md");
    let readme_text = std::fs::read_to_string(&readme).unwrap_or_default();
    match copy_file(&readme, &ws.join("README.md")) {
        Ok(true) => log.push("  README.md".into()),
        Ok(false) => {}
        Err(e) => log.push(format!("  WARNING: could not copy README.md: {e:#}")),
    }
    match copy_file(&cs.join("timeline.jsonl"), &ws.join("timeline.jsonl")) {
        Ok(true) => log.push("  timeline.jsonl".into()),
        Ok(false) => {}
        Err(e) => log.push(format!("  WARNING: could not copy timeline.jsonl: {e:#}")),
    }
    match copy_file(&cs.join("summary.md"), &ws.join("summary.md")) {
        Ok(true) => log.push("  summary.md".into()),
        Ok(false) => {}
        Err(e) => log.push(format!("  WARNING: could not copy summary.md: {e:#}")),
    }
    // memory/: narratives become notebooks, everything else stays memory.
    // I8: a subdirectory under memory/ is copied with copy_tree (copy_file
    // returns false for non-regular-files, which would otherwise drop it).
    // F2: classify by symlink_metadata first — is_dir() on symlink_metadata
    // is false for a symlink pointing at a directory, so that case must be
    // checked (and logged) before falling through to "is it a real dir".
    if let Ok(rd) = std::fs::read_dir(cs.join("memory")) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            let src_path = e.path();
            let dst = match fname.strip_prefix("narrative.") {
                Some(rest) => ws.join("notebook").join(format!("notebook.{rest}")),
                None => ws.join("memory").join(&fname),
            };
            let meta = std::fs::symlink_metadata(&src_path);
            let is_symlink = meta.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            if is_symlink {
                log.push(format!("  WARNING: skipped symlink: memory/{fname}"));
            } else if is_dir {
                match copy_tree(&src_path, &dst, &plan.source, &|_| false, &mut log) {
                    Ok(()) => log.push(format!(
                        "  memory/{fname}/ → {}",
                        dst.strip_prefix(&ws).unwrap_or(&dst).display()
                    )),
                    Err(e) => log.push(format!("  WARNING: could not copy memory/{fname}/: {e:#}")),
                }
            } else {
                match copy_file(&src_path, &dst) {
                    Ok(true) => log.push(format!(
                        "  memory/{fname} → {}",
                        dst.strip_prefix(&ws).unwrap_or(&dst).display()
                    )),
                    Ok(false) => log.push(format!("  WARNING: skipped memory/{fname}: not a regular file")),
                    Err(e) => log.push(format!("  WARNING: could not copy memory/{fname}: {e:#}")),
                }
            }
        }
    }
    for sub in ["handoffs", "artifacts", "plans", "checkpoints"] {
        let src = cs.join(sub);
        if src.is_dir() {
            match copy_tree(&src, &ws.join(sub), &plan.source, &|_| false, &mut log) {
                Ok(()) => log.push(format!("  {sub}/")),
                Err(e) => log.push(format!("  WARNING: could not copy {sub}/: {e:#}")),
            }
        }
    }

    // 4. workspace.toml enrichment from the cs README frontmatter.
    let fm = parse_frontmatter(&readme_text);
    let wt = ws.join("workspace.toml");
    if !fm.tags.is_empty() {
        match crate::meta::add_tags(&wt, &fm.tags) {
            Ok(_) => log.push(format!("  tags: {}", fm.tags.join(" "))),
            Err(e) => log.push(format!("  WARNING: could not set tags: {e:#}")),
        }
    }
    if fm.archived {
        match crate::meta::set_archived(&wt, true) {
            Ok(()) => log.push("  archived (was archived in cs)".into()),
            Err(e) => log.push(format!("  WARNING: could not mark archived: {e:#}")),
        }
    }
    if let Err(e) = crate::meta::update(&wt, |t| {
        t.insert("migrated_from".into(), toml::Value::String("cs".into()));
    }) {
        log.push(format!("  WARNING: could not record migrated_from: {e:#}"));
    }

    // 5. Context file for the default agent.
    match crate::agents::for_id(default_agent) {
        Ok(agent) => {
            if let Err(e) = crate::context::regenerate(&plan.dest.join(agent.context_file()), &plan.name) {
                log.push(format!("  WARNING: could not regenerate context file: {e:#}"));
            }
        }
        Err(e) => log.push(format!("  WARNING: could not resolve default agent {default_agent:?}: {e:#}")),
    }

    let _ = crate::timeline::record(
        &ws.join("timeline.jsonl"),
        "migrated",
        &crate::actors::actor_slug(),
        serde_json::json!({ "from": plan.source.to_string_lossy(), "tool": "cs" }),
    );

    // 6. Secrets: names only, never values. Best-effort.
    if let Some(names) = cs_secret_names(&plan.source) {
        if !names.is_empty() {
            log.push(format!(
                "  secrets NOT migrated (values are never read): {}",
                names.join(" ")
            ));
            log.push(format!(
                "    re-store each with:  cs -secrets get <NAME> | WS_WORKSPACE={} ws -secrets set <NAME>",
                plan.name
            ));
        }
    }
    Ok(log)
}

/// Ask `cs` which secrets a session has. Names only — never values.
/// Returns None when cs isn't installed or the call fails.
fn cs_secret_names(source: &Path) -> Option<Vec<String>> {
    let out = std::process::Command::new("cs")
        .arg("-secrets")
        .arg("list")
        .current_dir(source)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("No secrets stored") {
        return Some(Vec::new());
    }
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.contains(' '))
            .map(String::from)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A self-contained cs session: real dir with .cs/ and a project file.
    fn cs_session(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        let cs = dir.join(".cs");
        std::fs::create_dir_all(cs.join("memory")).unwrap();
        std::fs::create_dir_all(cs.join("local/log")).unwrap();
        std::fs::create_dir_all(cs.join("plans")).unwrap();
        std::fs::write(
            cs.join("README.md"),
            "---\nstatus: active\ncreated: 2026-07-01\ntags: [\"rust\", \"cli\"]\n---\n# Session: x\n\n## Objective\n\nShip it\n",
        )
        .unwrap();
        std::fs::write(cs.join("memory/narrative.me.md"), "day 1\n").unwrap();
        std::fs::write(cs.join("memory/MEMORY.md"), "- index\n").unwrap();
        std::fs::write(cs.join("plans/p.md"), "plan\n").unwrap();
        std::fs::write(cs.join("timeline.jsonl"), "{\"e\":1}\n").unwrap();
        std::fs::write(cs.join("local/log/session.log"), "TOKEN=hunter2\n").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn discover_finds_sessions_and_skips_non_sessions() {
        let d = TempDir::new().unwrap();
        cs_session(d.path(), "alpha");
        std::fs::create_dir_all(d.path().join("not-a-session")).unwrap();
        std::fs::write(d.path().join("index.md"), "# Sessions\n").unwrap();

        let (found, broken) = discover(d.path());
        let names: Vec<_> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
        assert!(broken.is_empty());
    }

    #[test]
    fn plan_for_symlink_migrates_in_place() {
        let d = TempDir::new().unwrap();
        let project = cs_session(d.path(), "real-project");
        let cs_root = d.path().join("cs");
        std::fs::create_dir_all(&cs_root).unwrap();
        let link = cs_root.join("proj");
        std::os::unix::fs::symlink(&project, &link).unwrap();

        let sessions_root = d.path().join("agent-workspaces");
        let plan = plan_for("proj", &link, &sessions_root).unwrap();
        assert!(plan.in_place, "a symlinked cs session must never be copied");
        assert_eq!(
            plan.dest.canonicalize().unwrap(),
            project.canonicalize().unwrap()
        );
    }

    #[test]
    fn plan_for_real_dir_copies_into_sessions_root() {
        let d = TempDir::new().unwrap();
        let src = cs_session(d.path(), "alpha");
        let sessions_root = d.path().join("agent-workspaces");
        let plan = plan_for("alpha", &src, &sessions_root).unwrap();
        assert!(!plan.in_place);
        assert_eq!(plan.dest, sessions_root.join("alpha"));
    }

    #[test]
    fn frontmatter_parsing() {
        let fm = parse_frontmatter(
            "---\nstatus: archived\ncreated: 2026-07-01\ntags: [\"rust\", \"cli\"]\naliases: [\"a\"]\n---\n# body\n",
        );
        assert!(fm.archived);
        assert_eq!(fm.tags, vec!["rust".to_string(), "cli".to_string()]);

        let none = parse_frontmatter("# no frontmatter\n");
        assert!(!none.archived);
        assert!(none.tags.is_empty());
    }
}
