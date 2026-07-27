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
    pub fn queue_dir(&self) -> PathBuf {
        self.ws_dir().join("queue")
    }
    pub fn queue_tasks(&self) -> PathBuf {
        self.queue_dir().join("tasks.jsonl")
    }
    /// Drain journal: per-checkout run output, not shared state.
    pub fn queue_journal(&self) -> PathBuf {
        self.local_dir().join("queue-journal.log")
    }
    /// Present when the circuit breaker has tripped. Cleared by `--reset`.
    pub fn circuit_marker(&self) -> PathBuf {
        self.local_dir().join("queue-circuit-open")
    }
    pub fn readme(&self) -> PathBuf {
        self.ws_dir().join("README.md")
    }
    pub fn notebook_dir(&self) -> PathBuf {
        self.ws_dir().join("notebook")
    }
    pub fn mail_dir(&self) -> PathBuf {
        self.ws_dir().join("mail")
    }
    /// Marker for the newest message already surfaced. Lives under local/ because
    /// "what I have read" is per-checkout, not shared state to merge.
    pub fn mail_seen(&self) -> PathBuf {
        self.local_dir().join("mail-seen")
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
    pub fn exists(&self) -> bool {
        self.ws_dir().is_dir()
    }
}

pub fn resolve(name: &str, cfg: &Config) -> Workspace {
    let root = registry::lookup(name)
        .unwrap_or_else(|| config::sessions_root(cfg).join(name));
    Workspace {
        name: name.to_string(),
        root,
    }
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
    if ws.exists() {
        return Ok((ws, false));
    }
    std::fs::create_dir_all(&ws.root)?;
    contract::init(name, &ws.root, agent, /* commit */ true)?;
    Ok((ws, true))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains("..")
        || name.starts_with('-')
    {
        anyhow::bail!("invalid workspace name: {name:?}");
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
        assert!(ws.exists());
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

    #[test]
    fn limit_guard_path() {
        let (_d, cfg) = iso_cfg();
        let (ws, _) = open_or_create("g", "claude", &cfg).unwrap();
        assert_eq!(ws.limit_guard(), ws.root.join(".ws/local/limit-guard"));
    }
}
