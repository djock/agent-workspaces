use crate::agents::{self, LaunchCtx};
use crate::cli::ConfigCmd;
use crate::cli::SecretsCmd;
use crate::config;
use crate::context;
use crate::contract;
use crate::limits;
use crate::lock;
use crate::registry;
use crate::secrets;
use crate::term;
use crate::workspace;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::io::Read;

pub fn secrets(cmd: SecretsCmd) -> Result<()> {
    let ws = secrets::workspace_name()?;
    let store = secrets::open(&ws)?;
    match cmd {
        SecretsCmd::Set(name) => {
            let mut value = String::new();
            std::io::stdin().read_to_string(&mut value)?;
            let value = value.strip_suffix('\n').unwrap_or(&value);
            store.set(&name, value)?;
            println!("stored {name}"); // never echoes the value
        }
        SecretsCmd::Get(name) => match store.get(&name)? {
            Some(v) => println!("{v}"),
            None => anyhow::bail!("no such secret: {name}"),
        },
        SecretsCmd::List => {
            for n in store.list()? {
                println!("{n}");
            }
        }
        SecretsCmd::Rm(name) => {
            store.remove(&name)?;
            println!("removed {name}");
        }
        SecretsCmd::Purge => {
            if std::io::stdin().is_terminal() {
                eprint!("Purge ALL secrets for workspace {ws}? [y/N] ");
                use std::io::Write;
                std::io::stderr().flush().ok();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    println!("cancelled");
                    return Ok(());
                }
            } else {
                anyhow::bail!("refusing to purge without a TTY to confirm");
            }
            store.purge()?;
            println!("purged all secrets for {ws}");
        }
        SecretsCmd::Export => {
            for n in store.list()? {
                if let Some(v) = store.get(&n)? {
                    let escaped = v.replace('\'', "'\\''");
                    println!("export {n}='{escaped}'");
                }
            }
        }
        SecretsCmd::Backend => println!("{}", store.backend_name()),
    }
    Ok(())
}

pub fn setup() -> Result<()> {
    let ws_bin = std::env::current_exe()?;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        if !agent.is_installed() {
            continue;
        }
        let nh = crate::hooksetup::install_hooks_for(&agent.hooks_config_path(), &ws_bin)?;
        let np = crate::prompts::install_for(&agent.prompts_dir(), |b| agent.prompt_filename(b))?;
        println!(
            "ws setup [{}]: installed {nh} hook(s) → {}\n            installed {np} prompt(s) → {}",
            agent.id(),
            agent.hooks_config_path().display(),
            agent.prompts_dir().display(),
        );
        if let Some(note) = agent.hook_trust_note() {
            println!("  note: {note}");
        }
    }
    // Configure each installed agent's status bar with the same core fields.
    if crate::agents::for_id("claude")?.is_installed() {
        crate::hooksetup::register_statuslines(&ws_bin)?;
        println!("registered Claude statusline + subagent-statusline");
    }
    if crate::agents::for_id("codex")?.is_installed() {
        crate::hooksetup::register_codex_statusline()?;
        println!("registered Codex statusline");
    }
    Ok(())
}

pub fn doctor() -> Result<()> {
    let mut any_agent = false;
    let mut hard_fail = false;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        if agent.is_installed() {
            any_agent = true;
            println!("✓ {id}: installed ({})", agent_version(&agent.binary()));
            let cfg_path = agent.hooks_config_path();
            let has_hook = std::fs::read_to_string(&cfg_path).ok()
                .map(|s| s.contains(&crate::hooksetup::hooks_dir().to_string_lossy().to_string()))
                .unwrap_or(false);
            println!("  {} ws hooks registered in {}", if has_hook { "✓" } else { "…" }, cfg_path.display());
            if let Some(note) = agent.hook_trust_note() { println!("  note: {note}"); }
        } else {
            println!("… {id}: not installed");
        }
    }
    // shims present?
    let shim = crate::hooksetup::hooks_dir().join("session-start.sh");
    if shim.exists() { println!("✓ ws hook scripts present"); }
    else { println!("… ws hook scripts missing — run `ws setup`"); }

    if !any_agent {
        eprintln!("ws: no agent installed (need claude or codex on PATH)");
        hard_fail = true;
    }
    if hard_fail { std::process::exit(1); }
    Ok(())
}

pub fn uninstall(force: bool) -> Result<()> {
    if !force {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("refusing to uninstall without a TTY; use `ws -uninstall --force`");
        }
        eprint!(
            "Remove the ws binary, hooks, prompts, and status lines?\n\
             Workspaces and configuration will be kept. [y/N] "
        );
        use std::io::Write;
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            println!("cancelled");
            return Ok(());
        }
    }

    let ws_bin = std::env::current_exe().context("cannot locate the current ws binary")?;
    if ws_bin.file_name().and_then(|name| name.to_str()) != Some("ws") {
        anyhow::bail!(
            "refusing to remove unexpected executable {}; remove it manually",
            ws_bin.display()
        );
    }

    let mut hooks = 0;
    let mut prompts = 0;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        hooks += crate::hooksetup::unregister_hooks_for(&agent.hooks_config_path())?;
        prompts += crate::prompts::uninstall_for(&agent.prompts_dir(), |base| {
            agent.prompt_filename(base)
        })?;
    }
    let statuslines = crate::hooksetup::unregister_statuslines(&ws_bin)?
        + crate::hooksetup::unregister_codex_statusline()?;
    let scripts = crate::hooksetup::remove_hook_scripts()?;

    std::fs::remove_file(&ws_bin)
        .with_context(|| format!("failed to remove {}", ws_bin.display()))?;
    println!(
        "Uninstalled ws from {} ({hooks} hooks, {scripts} scripts, {prompts} prompts, \
         {statuslines} status lines removed).",
        ws_bin.display()
    );
    println!("Your workspaces and ws configuration were kept.");
    Ok(())
}

fn agent_version(bin: &str) -> String {
    std::process::Command::new(bin).arg("--version").output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn adopt(name: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let name = match name {
        Some(n) => n,
        None => cwd
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("cannot derive a workspace name from {}", cwd.display()))?,
    };
    if cwd.join(".ws").is_dir() {
        // Already a workspace: just (re)register.
        crate::registry::register(&name, &cwd)?;
        println!("re-registered existing workspace: {name}");
        return Ok(());
    }
    let cfg = config::load();
    let agent = cfg.default_agent.clone();
    contract::init(&name, &cwd, &agent, /* commit */ false)?;
    println!("adopted {name} at {}", cwd.display());
    Ok(())
}

/// Resolve which workspace a metadata command applies to: an explicit name,
/// else $WS_WORKSPACE, else the current directory if it is a workspace.
pub(crate) fn current_or_named(name: Option<String>) -> Result<(String, std::path::PathBuf)> {
    if let Some(n) = name {
        let path = registry::lookup(&n)
            .ok_or_else(|| anyhow::anyhow!("no such workspace: {n}"))?;
        return Ok((n, path));
    }
    if let Ok(n) = std::env::var("WS_WORKSPACE") {
        if let Some(path) = registry::lookup(&n) {
            return Ok((n, path));
        }
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".ws").is_dir() {
        let n = crate::meta::read(&cwd.join(".ws/workspace.toml")).name;
        let n = if n.is_empty() {
            cwd.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()
        } else {
            n
        };
        return Ok((n, cwd));
    }
    anyhow::bail!("not in a workspace (name one with --workspace, or run inside one)")
}

pub fn tag(cmd: crate::cli::TagCmd) -> Result<()> {
    use crate::cli::TagCmd;
    let name = match &cmd {
        TagCmd::Add { name, .. } | TagCmd::Rm { name, .. } | TagCmd::List { name } => name.clone(),
    };
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    match cmd {
        TagCmd::Add { tags, .. } => {
            let all = crate::meta::add_tags(&wt, &tags)?;
            println!("{ws_name}: {}", all.join(" "));
        }
        TagCmd::Rm { tags, .. } => {
            let all = crate::meta::remove_tags(&wt, &tags)?;
            println!("{ws_name}: {}", all.join(" "));
        }
        TagCmd::List { .. } => {
            let all = crate::meta::read(&wt).tags;
            if all.is_empty() {
                println!("{ws_name}: (no tags)");
            } else {
                println!("{ws_name}: {}", all.join(" "));
            }
        }
    }
    Ok(())
}

pub fn status(name: Option<String>, text: Option<String>) -> Result<()> {
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    crate::meta::set_status(&wt, text.as_deref())?;
    match text {
        Some(t) => println!("{ws_name}: {t}"),
        None => println!("{ws_name}: status cleared"),
    }
    Ok(())
}

pub fn archive(names: Vec<String>, archived: bool) -> Result<()> {
    let mut failed = false;
    for name in names {
        let path = match registry::lookup(&name) {
            Some(p) => p,
            None => {
                eprintln!("ws: no such workspace: {name}");
                failed = true;
                continue;
            }
        };
        if let Err(e) = crate::meta::set_archived(&path.join(".ws/workspace.toml"), archived) {
            eprintln!("ws: failed to update {name}: {e}");
            failed = true;
            continue;
        }
        println!("{name}: {}", if archived { "archived" } else { "unarchived" });
    }
    if failed {
        anyhow::bail!("some workspaces could not be updated");
    }
    Ok(())
}

pub fn list(tag: Option<String>, archived: bool) -> Result<()> {
    let opts = crate::rows::ListOpts { tag: tag.clone(), include_archived: archived };
    let listing = crate::rows::list_all(&opts)?;
    if listing.rows.is_empty() {
        // I8: "you have none" and "none matched" are different sentences, and
        // they were the wrong way round. Only the unfiltered count can tell
        // them apart — pointing a user with an empty registry at
        // `-list --archived` sends them after workspaces that cannot exist.
        match (listing.total, tag) {
            (0, _) => println!("no workspaces yet — create one with: ws <name>"),
            (_, Some(t)) => println!("no workspaces tagged {t}"),
            // Registered workspaces exist but all are archived. (With
            // --archived and no tag nothing is filtered, so this is the
            // plain-`-list` case.)
            (_, None) => println!("no active workspaces (try: ws -list --archived)"),
        }
        return Ok(());
    }
    for r in listing.rows {
        let state = match &r.state {
            crate::rows::RowState::Ok => String::new(),
            crate::rows::RowState::Missing => "  (missing)".to_string(),
            crate::rows::RowState::Corrupt(e) => format!("  (corrupt: {e})"),
        };
        let flag = if r.archived { "  [archived]" } else { "" };
        let tags = if r.tags.is_empty() { String::new() } else { format!("  [{}]", r.tags.join(" ")) };
        let status = r.status.map(|s| format!("  — {s}")).unwrap_or_default();
        println!("{}\t{}{state}{flag}{tags}{status}", r.name, r.path.display());
    }
    Ok(())
}

fn print_limits_row(label: &str, snap: &limits::LimitsSnapshot, now: i64) {
    println!(
        "{label}\t5h {}% (resets in {})\twk {}% (resets in {})",
        snap.five_hour.used_pct.round() as i64,
        limits::countdown(snap.five_hour.resets_at, now),
        snap.seven_day.used_pct.round() as i64,
        limits::countdown(snap.seven_day.resets_at, now),
    );
}

pub fn limits() -> Result<()> {
    let now = limits::now_epoch();
    let mut shown = 0;
    for (name, path) in crate::registry::all() {
        let m = crate::meta::read(&path.join(".ws/workspace.toml"));
        if m.archived {
            continue;
        }
        let lp = path.join(".ws/local/limits.json");
        if let Some(snap) = limits::read(&lp) {
            print_limits_row(&name, &snap, now);
            shown += 1;
        }
    }
    if shown == 0 {
        if let Some(snap) = limits::read(&limits::global_path()) {
            print_limits_row("(global)", &snap, now);
        } else {
            println!("no limit data yet (run a ws session so the statusline can sense them)");
        }
    }
    Ok(())
}

/// What went wrong in `remove_one`, distinguished so callers never report a
/// successful removal as a failure. `Delete` means nothing changed — the
/// directory (or `.ws/`) is still there. `Unregister` means the removal
/// itself *succeeded* and only the registry write afterward failed, leaving
/// a stale (but harmless — `lookup()` still resolves it) registry entry.
#[derive(Debug)]
pub enum RemoveError {
    Delete(anyhow::Error),
    Unregister(anyhow::Error),
    /// A live process holds the workspace lock. Deleting it out from under a
    /// running agent loses whatever that session has not written yet.
    Live(u32),
    /// The lock file exists but could not be read (permission error, I/O
    /// error) — so whether a live process holds it could not be determined.
    /// Unreadable is not proof of absence: refuse rather than guess and risk
    /// deleting a live workspace out from under it. `--force` still overrides.
    LockUnreadable(anyhow::Error),
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoveError::Delete(e) => write!(f, "{e}"),
            RemoveError::Unregister(e) => {
                write!(f, "removed, but could not clear the registry entry: {e}")
            }
            RemoveError::Live(pid) => write!(
                f,
                "workspace is in use by pid {pid} (another terminal). Close it, or re-run with --force"
            ),
            RemoveError::LockUnreadable(e) => write!(
                f,
                "could not tell whether this workspace is in use (lock file unreadable: {e:#}). \
                 Close any process that might be using it, or re-run with --force"
            ),
        }
    }
}

impl std::error::Error for RemoveError {}

/// Is `path_c` (already canonicalized) a workspace ws itself created — i.e. a
/// *direct child* of the sessions root — and therefore safe to delete whole?
///
/// C3: this is the only guard standing between `-rm` and `remove_dir_all` on a
/// user-supplied path, and the old form (`path_c.starts_with(&root_c)` over a
/// `canonicalize().unwrap_or(root)` fallback) failed open in three ways:
///
/// * `Path::starts_with("")` is **true for every path**, and `sessions_root`
///   is unvalidated — `ws config set sessions_root ""` made every adopted
///   project anywhere on disk take the delete-the-whole-directory branch.
/// * `path_c == root_c` satisfies `starts_with`, so a workspace registered
///   *at* the root deleted the entire root.
/// * A relative root would match relative paths by accident.
///
/// Everything that is not provably a direct child of a real, absolute,
/// non-filesystem-root directory falls to the conservative `.ws`-only branch.
fn is_managed_workspace(root: &std::path::Path, path_c: &std::path::Path) -> bool {
    // An unreadable/nonexistent root cannot be proven to contain anything.
    let Ok(root_c) = root.canonicalize() else { return false };
    // Reject "" (canonicalize already errors on it, but be explicit), any
    // relative root, and the filesystem root itself — under `/` every
    // absolute path is a descendant.
    if !root_c.is_absolute() || root_c.parent().is_none() {
        return false;
    }
    // Exactly one component below the root: `<root>/<name>`, nothing deeper
    // and never the root itself.
    path_c.strip_prefix(&root_c).is_ok_and(|rest| rest.components().count() == 1)
}

/// Does removing `path` delete the whole workspace directory (true) or only
/// its `.ws/` (false)? This is the exact predicate `remove_one` acts on —
/// exported so the TUI's confirm dialog can state the same thing it does,
/// rather than re-deriving (and risking disagreeing with) the root
/// comparison that C3 made deliberately fail closed.
pub fn deletes_whole_directory(path: &std::path::Path) -> bool {
    let cfg = config::load();
    let root = config::sessions_root(&cfg);
    // Canonicalize before comparing: on macOS temp dirs live under a symlink
    // (/var -> /private/var), so a literal prefix check can mismatch even when
    // one path is truly nested under the other.
    let path_c = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    is_managed_workspace(&root, &path_c)
}

/// Remove one workspace with no prompting: the whole directory when it lives
/// under the workspaces root, otherwise just its `.ws/` (an adopted project
/// keeps its source), then drop the registry entry. Refuses (unless `force`)
/// when a live process holds the workspace's lock — `lock::acquire` already
/// refuses a live workspace for launch, and deleting it out from under a
/// running agent loses whatever that session has not written yet.
pub fn remove_one(name: &str, path: &std::path::Path, force: bool) -> std::result::Result<(), RemoveError> {
    if !force {
        match crate::lock::live_pid_checked(&path.join(".ws/local/lock")) {
            Ok(Some(pid)) => return Err(RemoveError::Live(pid)),
            Ok(None) => {}
            Err(e) => return Err(RemoveError::LockUnreadable(e)),
        }
    }
    let result = if deletes_whole_directory(path) {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_dir_all(path.join(".ws"))
    };
    if let Err(e) = result {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(RemoveError::Delete(
                anyhow::Error::new(e).context(format!("failed to remove {name}")),
            ));
        }
        // NotFound → already gone; fall through and unregister the stale entry.
    }
    crate::registry::unregister(name).map_err(RemoveError::Unregister)
}

pub fn rm(names: Vec<String>, force: bool) -> Result<()> {
    for name in names {
        let path = match crate::registry::lookup(&name) {
            Some(p) => p,
            None => {
                eprintln!("ws: no such workspace: {name}");
                continue;
            }
        };
        if !force {
            if !std::io::stdin().is_terminal() {
                anyhow::bail!("refusing to remove {name} without --force (no TTY to confirm)");
            }
            eprint!("Remove workspace {name} at {}? [y/N] ", path.display());
            use std::io::Write;
            std::io::stderr().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if !matches!(line.trim(), "y" | "Y" | "yes") {
                println!("skipped {name}");
                continue;
            }
        }
        match remove_one(&name, &path, force) {
            Ok(()) => println!("removed {name}"),
            Err(RemoveError::Delete(e)) => {
                eprintln!("ws: failed to remove {name}: {e}");
                continue; // keep the registry entry — it still exists on disk
            }
            Err(RemoveError::Live(pid)) => {
                // A live workspace is a per-name condition, not a reason to
                // abandon the rest of the batch.
                eprintln!("ws: {name}: workspace is in use by pid {pid} (another terminal). Close it, or re-run with --force");
                continue;
            }
            Err(RemoveError::LockUnreadable(e)) => {
                // Same per-name reasoning as Live: could not prove this one
                // is safe, but that says nothing about the rest of the batch.
                eprintln!("ws: {name}: {e}");
                continue;
            }
            Err(RemoveError::Unregister(e)) => {
                // The workspace itself is gone; only the registry write
                // failed. This restores the pre-extraction behavior of
                // `unregister(&name)?`: abort the whole batch rather than
                // silently continuing with a stale registry entry.
                return Err(e).with_context(|| {
                    format!("{name} was removed, but its registry entry could not be cleared")
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod remove_tests {
    use std::sync::Mutex;
    use tempfile::TempDir;

    // remove_one() resolves config through process-global XDG_CONFIG_HOME /
    // WS_ROOT and deletes real directories — serialize explicitly rather than
    // leaning on the RUST_TEST_THREADS pin (see registry.rs, rows.rs).
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn remove_one_deletes_only_ws_for_an_adopted_project() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::fs::write(project.join("keep-me.txt"), "source code").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        super::remove_one("myproj", &project, false).unwrap();

        assert!(!project.join(".ws").exists(), ".ws is gone");
        assert!(project.join("keep-me.txt").exists(), "an adopted project itself must survive");
        assert!(crate::registry::lookup("myproj").is_none(), "and the registry entry is cleared");
    }

    /// M2. `config::load` is lenient — a corrupt or unreadable `config.toml`
    /// silently becomes `Config::default()` — and its `sessions_root` is what
    /// `deletes_whole_directory` compares against. `load`'s doc comment
    /// asserts that this fails *closed*, i.e. that a substituted default can
    /// only ever narrow a whole-directory delete to a `.ws`-only one and
    /// never the reverse. That claim gates an irreversible operation, so pin
    /// it rather than leaving it as prose.
    #[test]
    fn a_corrupt_config_narrows_the_deletion_scope_and_never_widens_it() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::remove_var("WS_ROOT"); // the config value is what's under test

        let root = d.path().join("roots");
        let managed = root.join("alpha");
        std::fs::create_dir_all(&managed).unwrap();
        let cfg_path = crate::config::config_path();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();

        // With a valid config naming this root, `alpha` is a direct child and
        // the whole directory goes.
        std::fs::write(&cfg_path, format!("sessions_root = {:?}\n", root.display().to_string())).unwrap();
        assert!(
            super::deletes_whole_directory(&managed),
            "a correctly configured root must still delete the whole directory"
        );

        // Corrupt the same config. `load` substitutes defaults, so the root
        // becomes ~/.agent-workspaces, `alpha` stops looking like a direct
        // child of it, and the narrow branch wins.
        std::fs::write(&cfg_path, "sessions_root = not valid toml ][").unwrap();
        assert!(
            !super::deletes_whole_directory(&managed),
            "a corrupt config must fail closed: narrow to .ws/, never widen to the whole directory"
        );
    }

    /// C3 regression. An empty `sessions_root` made `Path::starts_with`
    /// return true for every path, so `-rm` deleted an adopted project's
    /// entire source tree. Only `.ws` may go.
    #[test]
    fn an_empty_sessions_root_does_not_delete_an_adopted_projects_source() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::remove_var("WS_ROOT"); // so the config value is what's used
        // `config set` now rejects "", but a hand-edited config.toml can still
        // carry one — write it directly so the guard itself is what's tested.
        let cfg_path = crate::config::config_path();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "sessions_root = \"\"\n").unwrap();
        assert_eq!(crate::config::sessions_root(&crate::config::load()).as_os_str(), "");

        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::fs::write(project.join("src.rs"), "fn main() {}").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        super::remove_one("myproj", &project, false).unwrap();

        assert!(project.join("src.rs").exists(), "the project's source must survive");
        assert!(!project.join(".ws").exists(), "only .ws goes");
    }

    /// A workspace registered *at* the sessions root must never take the
    /// delete-the-whole-directory branch — `path == root` also satisfied
    /// `starts_with`, which deleted the entire root.
    #[test]
    fn a_workspace_at_the_sessions_root_itself_is_never_deleted_whole() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join(".ws")).unwrap();
        std::fs::write(root.join("other-stuff.txt"), "not ours").unwrap();
        std::env::set_var("WS_ROOT", &root);
        crate::registry::register("root", &root).unwrap();

        super::remove_one("root", &root, false).unwrap();

        assert!(root.exists(), "the sessions root itself must survive");
        assert!(root.join("other-stuff.txt").exists(), "and everything in it");
        assert!(!root.join(".ws").exists());
    }

    /// The counterpart: a genuine `<root>/<name>` workspace still goes whole.
    #[test]
    fn a_direct_child_of_the_sessions_root_is_still_removed_whole() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        let root = d.path().join("root");
        let project = root.join("mine");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::env::set_var("WS_ROOT", &root);
        crate::registry::register("mine", &project).unwrap();

        super::remove_one("mine", &project, false).unwrap();

        assert!(!project.exists(), "a ws-created workspace is removed whole");
        assert!(root.exists(), "but not its root");
    }

    #[test]
    fn remove_one_reports_a_deletion_failure_as_delete_not_unregister() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(&project).unwrap();
        // `.ws` exists as a *file*, not a directory: `remove_dir_all` on it
        // fails with a real error (not NotFound) — this must surface as
        // Delete, not silently fall through to unregister and get reported
        // as if the registry write were the problem.
        std::fs::write(project.join(".ws"), "not a directory").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        let err = super::remove_one("myproj", &project, false).unwrap_err();

        assert!(
            matches!(err, super::RemoveError::Delete(_)),
            "a real deletion failure must not be reported as a registry failure: {err:?}"
        );
        assert!(project.join(".ws").exists(), "nothing was removed");
        assert!(crate::registry::lookup("myproj").is_some(), "registry entry untouched on a failed delete");
    }

    #[test]
    fn remove_one_refuses_a_workspace_a_live_process_holds() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let ws = d.path().join("busy");
        std::fs::create_dir_all(ws.join(".ws/local")).unwrap();
        crate::registry::register("busy", &ws).unwrap();
        // Our own pid is by definition alive.
        let me = std::process::id();
        std::fs::write(
            ws.join(".ws/local/lock"),
            format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n"),
        )
        .unwrap();

        let err = super::remove_one("busy", &ws, false).unwrap_err();
        assert!(matches!(err, super::RemoveError::Live(p) if p == me), "got {err:?}");
        assert!(ws.join(".ws").exists(), "nothing was deleted");
        assert!(crate::registry::lookup("busy").is_some(), "and it is still registered");

        // force overrides, the same escape hatch launch has.
        super::remove_one("busy", &ws, true).unwrap();
        assert!(!ws.join(".ws").exists());
    }

    #[test]
    #[cfg(unix)]
    fn remove_one_refuses_a_workspace_whose_lock_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let ws = d.path().join("busy");
        std::fs::create_dir_all(ws.join(".ws/local")).unwrap();
        crate::registry::register("busy", &ws).unwrap();
        let me = std::process::id();
        let lock_path = ws.join(".ws/local/lock");
        std::fs::write(&lock_path, format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();

        // Write-only, no read: a live pid is recorded (this very process),
        // but `remove_one` cannot read it to find out. Pre-fix, `live_pid`
        // folded that read failure into "not live" and proceeded to delete
        // the whole (live) workspace directory.
        let mut perms = std::fs::metadata(&lock_path).unwrap().permissions();
        perms.set_mode(0o200);
        std::fs::set_permissions(&lock_path, perms).unwrap();

        let err = super::remove_one("busy", &ws, false).unwrap_err();

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&lock_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&lock_path, perms).unwrap();

        assert!(
            matches!(err, super::RemoveError::LockUnreadable(_)),
            "an unreadable lock must be its own error, not silently treated as 'not live': {err:?}"
        );
        assert!(ws.join(".ws").exists(), "the workspace directory must survive — nothing was proven safe to delete");
        assert!(crate::registry::lookup("busy").is_some(), "and it is still registered");

        // force overrides, the same escape hatch launch has.
        super::remove_one("busy", &ws, true).unwrap();
        assert!(!ws.join(".ws").exists());
    }

    #[test]
    #[cfg(unix)]
    fn remove_one_reports_unregister_failure_after_a_successful_delete() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        crate::registry::register("myproj", &project).unwrap();

        let registry_path = crate::registry::registry_path();
        let mut perms = std::fs::metadata(&registry_path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&registry_path, perms).unwrap();

        let err = super::remove_one("myproj", &project, false).unwrap_err();

        // Restore permissions before any assertion (and before TempDir
        // teardown) so cleanup doesn't itself fail.
        let mut perms = std::fs::metadata(&registry_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&registry_path, perms).unwrap();

        assert!(
            matches!(err, super::RemoveError::Unregister(_)),
            "a registry write failure after a successful delete must not be reported as a deletion failure: {err:?}"
        );
        assert!(!project.join(".ws").exists(), "the workspace itself was actually removed");
    }
}

pub fn config(cmd: ConfigCmd) -> Result<()> {
    let cfg = config::load();
    match cmd {
        ConfigCmd::List => {
            for (k, v) in config::list(&cfg) {
                println!("{k} = {v}");
            }
        }
        ConfigCmd::Get(key) => {
            println!("{}", config::get(&cfg, &key)?);
        }
        ConfigCmd::Set { key, value, workspace } => {
            if workspace {
                anyhow::bail!("per-workspace config is added in a later task");
            }
            config::set(&key, &value)?;
        }
    }
    Ok(())
}

pub fn launch(
    name: String,
    agent_override: Option<String>,
    fresh: bool,
    force: bool,
    handoff: bool,
) -> Result<()> {
    let cfg = config::load();

    // 1. Resolve agent id: --agent > workspace default > config default.
    //    Read the recorded default BEFORE open_or_create so a brand-new workspace
    //    (no recorded default yet) is never mistaken for an agent switch — the switch
    //    invariant must not depend on what contract::init happens to write.
    let recorded_default =
        crate::meta::read(&workspace::resolve(&name, &cfg).workspace_toml()).default_agent;
    let agent_id = agent_override
        .or_else(|| recorded_default.clone())
        .unwrap_or_else(|| cfg.default_agent.clone());
    let agent = agents::for_id(&agent_id)?;
    if !agent.is_installed() {
        anyhow::bail!(
            "{} is not installed or not on PATH (looked for `{}`). Install it, or set WS_CLAUDE_BIN.",
            agent.id(),
            agent.binary()
        );
    }

    // Whether this launch switches the workspace to a different agent than its
    // recorded default. None (first launch) means "not switching".
    let switching = recorded_default.as_deref().is_some_and(|d| d != agent.id());

    // 2. Create/resolve.
    let (ws, _created) = workspace::open_or_create(&name, agent.id(), &cfg)?;

    // 3. Lock.
    let guard = lock::acquire(&ws.lock_file(), force)?;

    // 4. Regenerate context file, seeding a handoff pointer when requested or switching.
    let hint = if handoff || switching {
        crate::handoff::latest_handoff(&ws)
    } else {
        None
    };
    context::regenerate_with_handoff(&ws.root.join(agent.context_file()), &ws.name, hint.as_deref())?;

    if switching {
        let _ = std::fs::remove_file(ws.limit_guard());
        crate::meta::set_default_agent(&ws.workspace_toml(), agent.id())?;
        let _ = crate::timeline::record(
            &ws.timeline(),
            "agent-switch",
            &crate::actors::actor_slug(),
            serde_json::json!({ "to": agent.id() }),
        );
    }

    // 5. Tab title + color.
    let color = crate::meta::read(&ws.workspace_toml()).color;
    term::set_tab(&ws.name, color.as_deref());

    // 6. Build + run — the agent owns the fresh/resume decision and persists its own state.
    let ctx = LaunchCtx {
        fresh,
        sessions_root: config::sessions_root(&cfg),
    };
    let cmd = agent.launch(&ws, &ctx)?;

    // Keep the lock file in place; the launched agent inherits our PID.
    guard.keep();

    if std::env::var_os("WS_NO_EXEC").is_some() {
        let mut cmd = cmd;
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }

    exec(cmd)
}

#[cfg(unix)]
fn exec(mut cmd: std::process::Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Err(cmd.exec().into()) // exec only returns on failure
}

#[cfg(not(unix))]
fn exec(mut cmd: std::process::Command) -> Result<()> {
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(0));
}

pub fn migrate_cs(names: Vec<String>, all: bool, dry_run: bool) -> Result<()> {
    let cfg = config::load();
    let sessions_root = config::sessions_root(&cfg);
    let cs_root = crate::migrate::cs_root();
    let (found, broken) = crate::migrate::discover(&cs_root);

    let mut failed = false;
    let selected: Vec<(String, std::path::PathBuf)> = if all {
        if found.is_empty() && broken.is_empty() {
            anyhow::bail!("no cs sessions found under {}", cs_root.display());
        }
        // I4: a dangling symlink entry is real but broken — say so, don't
        // just skip it without a trace.
        for (name, target) in &broken {
            println!("{name}: broken symlink (target {} does not exist) — skipping", target.display());
        }
        found
    } else {
        let mut out = Vec::new();
        for n in &names {
            if let Some(hit) = found.iter().find(|(name, _)| name == n) {
                out.push(hit.clone());
                continue;
            }
            if let Some((_, target)) = broken.iter().find(|(name, _)| name == n) {
                eprintln!(
                    "ws: {n}: broken symlink (target {} does not exist) — skipping",
                    target.display()
                );
                failed = true;
                continue;
            }
            anyhow::bail!("no cs session named {n} under {}", cs_root.display());
        }
        out
    };

    // F4: a warning line — anything migrate() reports as data present in the
    // source but dropped from the destination (a skipped symlink, a
    // non-regular-file under memory/, a failed best-effort copy step) — must
    // not be indistinguishable from clean success — count sessions that
    // produced at least one, and route those lines to stderr so a script
    // watching stdout can't mistake them for progress noise.
    let mut warned_sessions = 0usize;
    for (name, entry) in selected {
        let plan = match crate::migrate::plan_for(&name, &entry, &sessions_root) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ws: {name}: {e:#}");
                failed = true;
                continue;
            }
        };
        // I5: under --all, a session that's already migrated is a skip, not
        // a failure — re-running --all after migrating more cs sessions
        // should not report failure for every one already done. An
        // explicitly named session that's already migrated still errors
        // (migrate() below bails), since the user asked for that one.
        if all && plan.dest.join(".ws").is_dir() {
            println!("{name}: already migrated (skipping)");
            continue;
        }
        match crate::migrate::migrate(&plan, &cfg.default_agent, dry_run) {
            Ok(log) => {
                let mut had_warning = false;
                for line in log {
                    if line.trim_start().starts_with("WARNING:") {
                        eprintln!("{line}");
                        had_warning = true;
                    } else {
                        println!("{line}");
                    }
                }
                if had_warning {
                    warned_sessions += 1;
                }
            }
            Err(e) => {
                eprintln!("ws: {name}: {e:#}");
                failed = true;
            }
        }
    }
    if failed {
        anyhow::bail!("some sessions could not be migrated");
    }
    if warned_sessions > 0 {
        anyhow::bail!(
            "{warned_sessions} session(s) completed with warnings — see stderr for what was skipped"
        );
    }
    Ok(())
}

pub fn whoami() -> Result<()> {
    let dir = std::env::current_dir()?;
    println!("{}", crate::actors::actor_slug_in(&dir));
    Ok(())
}

pub fn who(name: Option<String>) -> Result<()> {
    // current_or_named is the established resolution for "this workspace or the
    // named one": it honours $WS_WORKSPACE, which matters because -who is most
    // often run from inside an agent session. Every command in this phase that
    // takes an optional workspace name uses it — do not hand-roll a second path.
    let (_name, root) = current_or_named(name)?;
    let ranked = crate::actors::who(&root.join(".ws"))?;
    if ranked.is_empty() {
        println!("no commits to .ws/ yet");
        return Ok(());
    }
    for (actor, n) in ranked {
        println!("{actor}  {n}");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod who_tests {
    use std::sync::Mutex;
    use tempfile::TempDir;

    // who() resolves through current_or_named(), which reads the process-global
    // WS_WORKSPACE env var, XDG_CONFIG_HOME-scoped registry, and current_dir.
    // Serialize explicitly rather than leaning on the RUST_TEST_THREADS pin
    // (see registry.rs, rows.rs, remove_tests above): this module changes the
    // process cwd, which no other module here does.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    // Discriminates the Important review finding: who(None) must resolve the
    // workspace the same way tag/status/archive do, i.e. via current_or_named
    // and therefore via $WS_WORKSPACE, not by treating the bare cwd as "the"
    // workspace. Before the fix, `who` built `Workspace { name: "here", root: cwd }`
    // directly, ignored $WS_WORKSPACE entirely, and this test failed with "not
    // in a workspace" because the cwd used here deliberately is not one.
    #[test]
    fn who_with_no_name_honours_ws_workspace_env_var() {
        let _g = lock_env();
        let config_dir = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", config_dir.path());

        // The registered workspace: a real git repo with a .ws dir, so
        // actors::who() has real history to read.
        let ws_dir = TempDir::new().unwrap();
        let root = ws_dir.path();
        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "someone@example.com"]);
        run_git(root, &["config", "user.name", "Someone"]);
        std::fs::create_dir_all(root.join(".ws")).unwrap();
        std::fs::write(root.join(".ws/marker.txt"), "x").unwrap();
        run_git(root, &["add", ".ws/marker.txt"]);
        run_git(root, &["commit", "-q", "-m", "seed"]);
        crate::registry::register("envres", root).unwrap();

        // The cwd: deliberately NOT the registered workspace and not a
        // workspace at all, so resolution can only succeed via $WS_WORKSPACE.
        let elsewhere = TempDir::new().unwrap();
        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(elsewhere.path()).unwrap();
        std::env::set_var("WS_WORKSPACE", "envres");

        let result = super::who(None);

        std::env::set_current_dir(&orig_cwd).unwrap();
        std::env::remove_var("WS_WORKSPACE");

        assert!(
            result.is_ok(),
            "who(None) must resolve via $WS_WORKSPACE like tag/status/archive do: {result:?}"
        );
    }

    #[test]
    fn who_with_no_name_and_no_env_var_in_a_non_workspace_cwd_errors() {
        let _g = lock_env();
        let config_dir = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", config_dir.path());
        std::env::remove_var("WS_WORKSPACE");

        let elsewhere = TempDir::new().unwrap();
        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(elsewhere.path()).unwrap();

        let result = super::who(None);

        std::env::set_current_dir(&orig_cwd).unwrap();

        assert!(result.is_err(), "with no name and no env var, a non-workspace cwd must error, not print empty");
    }
}

pub fn search(query: String, include_archived: bool) -> Result<()> {
    let hits = crate::search::search_all(&query, include_archived)?;
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(());
    }
    // Group consecutive hits by workspace — search_all emits them workspace-by-workspace,
    // and each workspace's group may carry one extra hit past MAX_HITS_PER_WORKSPACE
    // (see search_dir) purely as a truncation signal, not an actual match to display.
    let mut groups: Vec<(&str, Vec<&crate::search::Hit>)> = Vec::new();
    for h in &hits {
        match groups.last_mut() {
            Some((name, g)) if *name == h.workspace => g.push(h),
            _ => groups.push((h.workspace.as_str(), vec![h])),
        }
    }

    let mut shown_total = 0usize;
    let mut any_truncated = false;
    for (name, g) in &groups {
        println!("\n{name}");
        let truncated = g.len() > crate::search::MAX_HITS_PER_WORKSPACE;
        let display_count = g.len().min(crate::search::MAX_HITS_PER_WORKSPACE);
        for h in g.iter().take(display_count) {
            // Show the path relative to the workspace root when we can — the absolute
            // prefix is noise once the workspace name is the heading.
            let shown = h
                .file
                .iter()
                .skip_while(|c| *c != ".ws")
                .collect::<std::path::PathBuf>();
            println!("  {}:{}: {}", shown.display(), h.line, h.text.trim());
        }
        if truncated {
            println!("  … more matches in this workspace — refine your query");
            any_truncated = true;
        }
        shown_total += display_count;
    }

    if any_truncated {
        println!("\n{shown_total} match(es) in {} workspace(s) (some results hidden)", groups.len());
    } else {
        println!("\n{shown_total} match(es) in {} workspace(s)", groups.len());
    }
    Ok(())
}
