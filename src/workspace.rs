use anyhow::Result;
use std::path::PathBuf;

use crate::config::{self, Config};
use crate::contract;
use crate::registry;

pub struct Workspace {
    pub name: String,
    pub root: PathBuf,
}

impl Workspace {
    pub fn ws_dir(&self) -> PathBuf {
        self.root.join(".ws")
    }
    pub fn memory_dir(&self) -> PathBuf {
        self.ws_dir().join("memory")
    }
    pub fn local_dir(&self) -> PathBuf {
        self.ws_dir().join("local")
    }
    pub fn state_toml(&self) -> PathBuf {
        self.local_dir().join("state.toml")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.local_dir().join("lock")
    }
    pub fn workspace_toml(&self) -> PathBuf {
        self.ws_dir().join("workspace.toml")
    }
    /// Captured tasks. Shared, append-only, `merge=union` across checkouts.
    pub fn queue_tasks(&self) -> PathBuf {
        self.ws_dir().join("queue").join("tasks.jsonl")
    }
    pub fn readme(&self) -> PathBuf {
        self.ws_dir().join("README.md")
    }
    pub fn notebook_dir(&self) -> PathBuf {
        self.ws_dir().join("notebook")
    }
    pub fn timeline(&self) -> PathBuf {
        self.ws_dir().join("timeline.jsonl")
    }
    pub fn session_log(&self) -> PathBuf {
        self.local_dir().join("log").join("session.log")
    }
    pub fn limit_guard(&self) -> PathBuf {
        self.local_dir().join("limit-guard")
    }
    /// Is this an initialised workspace?
    ///
    /// The identity file, not the `.ws/` directory. Those differ: acquiring the
    /// workspace lock creates `.ws/local/`, so a directory-existence test reported
    /// "already a workspace" for one that had a lock and nothing else.
    pub fn is_initialised(&self) -> bool {
        self.workspace_toml().is_file()
    }
}

pub fn resolve(name: &str, cfg: &Config) -> Workspace {
    let root = registry::lookup(name).unwrap_or_else(|| config::sessions_root(cfg).join(name));
    Workspace { name: name.to_string(), root }
}

pub fn open_or_create(name: &str, agent: &str, cfg: &Config) -> Result<(Workspace, bool)> {
    validate_name(name)?;
    // Resolve through the checked lookup: if the registry is unreadable we
    // cannot tell "not registered" from "I can't see the entry", and guessing
    // the former would create a second, empty workspace beside a real one that
    // is merely invisible.
    let root = match registry::lookup_checked(name)? {
        Some(p) => p,
        None => config::sessions_root(cfg).join(name),
    };
    let ws = Workspace { name: name.to_string(), root };
    // `workspace.toml`, not "does `.ws/` exist": launch acquires the workspace
    // lock *before* calling this (so creation is single-writer), and acquiring the
    // lock creates `.ws/local/` — which made `.ws` exist and this function skip
    // `contract::init` entirely, leaving a workspace with a lock and nothing else.
    // The identity file is what actually distinguishes an initialised workspace.
    if ws.is_initialised() {
        // The contract gate: refuse a workspace a newer `ws` created before this
        // binary ever touches it (launch regenerates the context file, records
        // session state, etc). A brand-new workspace skips this — it is about
        // to be created BY this binary, at CONTRACT_VERSION, so there is
        // nothing yet to be newer than.
        contract::check_gate(&ws.name, &ws.workspace_toml())?;
        // Backfill the private modes on every open, not only at creation. Git
        // records no directory modes, so a workspace that arrived by clone — or
        // one created before ws set them — has a `.ws/` built under whatever
        // umask that machine had.
        contract::harden(&ws.root);
        return Ok((ws, false));
    }
    std::fs::create_dir_all(&ws.root)?;
    contract::init(name, &ws.root, agent, /* commit */ true)?;
    Ok((ws, true))
}

/// The one gate on what may become a workspace name.
///
/// This is an **allowlist**, deliberately. The previous denylist (empty, `/`,
/// `..`, leading `-`) admitted spaces, `;`, `$`, backticks, quotes, newlines and
/// control characters, and a workspace name reaches too many hostile contexts
/// for that to be safe: an argv element, a tmux window name, a directory name, a
/// TOML key, and a registry key. `ws -spawn` turned that into arbitrary code
/// execution (see `spawn::TmuxPlan::command`). Enumerating what is safe is the
/// only version of this function that stays correct as new call sites appear.
///
/// `@` is permitted because `base@feature` worktree workspaces are named with
/// it (`worktree::create` → `contract::init`). `.` is permitted for names like
/// `my.project`, but `..` in any position is not, and neither is a name that is
/// only dots — both are path traversal in a value used as a directory name.
///
/// Called from `contract::init` and `registry::register` rather than only from
/// `open_or_create`, because `-adopt` and `migrate-cs` reach those directly and
/// used to bypass validation entirely.
pub fn validate_name(name: &str) -> Result<()> {
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@');
    let bad = name.is_empty()
        || name.contains("..")
        || name.starts_with('-')
        || name.chars().all(|c| c == '.')
        || !name.chars().all(ok_char);
    if bad {
        anyhow::bail!(
            "invalid workspace name: {name:?} \
             (allowed: letters, digits, '-', '_', '.', '@'; no leading '-', no '..')"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::TempDir;

    fn iso_cfg() -> (TempDir, Config) {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let cfg = Config {
            sessions_root: d.path().join("root").to_string_lossy().to_string(),
            ..Config::default()
        };
        (d, cfg)
    }

    #[test]
    fn create_then_resolve_via_registry() {
        let (_d, cfg) = iso_cfg();
        let (ws, created) = open_or_create("proj", "claude", &cfg).unwrap();
        assert!(created);
        assert!(ws.is_initialised());
        assert_eq!(ws.root, resolve("proj", &cfg).root);

        // Second open does not recreate.
        let (_ws2, created2) = open_or_create("proj", "claude", &cfg).unwrap();
        assert!(!created2);
    }

    #[test]
    fn path_helpers() {
        let (_d, cfg) = iso_cfg();
        let (ws, _) = open_or_create("p", "claude", &cfg).unwrap();
        assert_eq!(ws.state_toml(), ws.root.join(".ws/local/state.toml"));
        assert_eq!(ws.memory_dir(), ws.root.join(".ws/memory"));
    }

    #[test]
    fn validate_name_rejects_bad_names() {
        // direct guard: all four rejection cases
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("../evil").is_err());
        assert!(validate_name("-flaglike").is_err());
        // sanity: a normal name passes
        assert!(validate_name("proj").is_ok());

        // end-to-end: open_or_create surfaces the rejection
        let (_d, cfg) = iso_cfg();
        assert!(open_or_create("../evil", "claude", &cfg).is_err());
    }

    /// M2, the validation half. The old denylist admitted every one of these,
    /// and a workspace name becomes an argv element, a tmux window name and a
    /// directory name. One case per character class, so a regression names the
    /// class it reopened rather than just "some name got through".
    #[test]
    fn shell_metacharacters_and_whitespace_are_rejected_by_class() {
        for (name, class) in [
            ("x;touch /tmp/pwned", "command separator"),
            ("x&whoami", "background/chain"),
            ("x|tee", "pipe"),
            ("x`id`", "backtick substitution"),
            ("x$(id)", "dollar substitution"),
            ("x${HOME}", "brace expansion"),
            ("has space", "whitespace"),
            ("has\ttab", "tab"),
            ("has\nnewline", "newline"),
            ("quote'single", "single quote"),
            ("quote\"double", "double quote"),
            ("glob*", "glob"),
            ("redirect>out", "redirection"),
            ("paren(", "paren"),
            ("~tilde", "tilde expansion"),
            ("!bang", "history expansion"),
            ("nul\0byte", "control character"),
            (".", "bare dot"),
            ("...", "only dots"),
            ("a..b", "inner traversal"),
        ] {
            assert!(
                validate_name(name).is_err(),
                "{class} must be rejected, but {name:?} was accepted"
            );
        }
    }

    /// The allowlist must not have become so tight that real names break —
    /// especially `base@feature`, which is how every worktree workspace is named
    /// and which reaches `contract::init` through `worktree::create`.
    #[test]
    fn ordinary_and_worktree_names_still_pass() {
        for name in ["proj", "my-project", "my_project", "my.project", "api@retry", "ws2", "A1"] {
            assert!(validate_name(name).is_ok(), "{name:?} must remain a legal name");
        }
    }

    /// The bypass that made the injection reachable: `-adopt` never called
    /// `validate_name`, so it registered whatever it was handed. Both write
    /// paths must now refuse, or `-spawn` gets a hostile name to run.
    #[test]
    fn adopt_and_register_cannot_bypass_validation() {
        let (_d, _cfg) = iso_cfg();
        let d = TempDir::new().unwrap();
        let nasty = "x;touch /tmp/pwned";

        assert!(
            crate::contract::init(nasty, d.path(), "claude", false).is_err(),
            "contract::init must refuse a hostile name (the -adopt / migrate-cs path)"
        );
        assert!(
            crate::registry::register(nasty, d.path()).is_err(),
            "registry::register must refuse it too (the re-adopt path skips init)"
        );
        assert!(
            crate::registry::lookup_checked(nasty).unwrap().is_none(),
            "and nothing may be left registered under that name"
        );
    }

    #[test]
    fn limit_guard_path() {
        let (_d, cfg) = iso_cfg();
        let (ws, _) = open_or_create("g", "claude", &cfg).unwrap();
        assert_eq!(ws.limit_guard(), ws.root.join(".ws/local/limit-guard"));
    }
}
