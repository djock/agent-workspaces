use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Every field here must be read by something. `prompt_on_launch` and
/// `nerd_fonts` were removed rather than kept: both were settable, listed by
/// `config list`, and read nowhere, so `ws config set nerd_fonts true` reported
/// success and changed nothing. A config key that silently does nothing is worse
/// than a missing one — the user believes they configured something. They are now
/// unknown keys, so `config set` rejects them with the list of real keys.
///
/// Unknown keys in an existing `config.toml` are ignored by serde, so a config
/// written by an older ws still loads; the stale lines are simply inert until the
/// next `config set` rewrites the file without them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_agent: String,
    pub limit_warn_5h: u8,
    pub limit_warn_week: u8,
    pub theme: String,
    /// Whether `ws setup` registers ws's status line with the agents.
    /// Honored in `commands::setup`; set it false to keep your own.
    pub statusline: bool,
    pub sessions_root: String,
    pub limit_action: String,
    pub secrets_backend: String,
    /// Whether the Stop hook surfaces captured tasks and asks whether to start
    /// the oldest. Fires once per change to the queue, never per turn.
    pub task_prompt: bool,
    /// Whether the Stop hook reminds the agent to write up its findings in the
    /// workspace notebook. Rate-limited to once per cooldown, and skipped
    /// entirely on continuation stops; set it false to never be reminded.
    pub notebook_prompt: bool,
    /// Whether `ws <name>` asks before resuming a previous conversation.
    /// The prompt defaults to No, so pressing Enter resumes.
    pub resume_prompt: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_agent: "claude".into(),
            limit_warn_5h: 85,
            limit_warn_week: 90,
            theme: "auto".into(),
            statusline: true,
            sessions_root: "~/.agent-workspaces".into(),
            limit_action: "handoff-stop".into(),
            secrets_backend: "auto".into(),
            task_prompt: true,
            notebook_prompt: true,
            resume_prompt: true,
        }
    }
}

/// Base config directory for ws (`<config>/ws`). Honors `XDG_CONFIG_HOME`
/// explicitly because `dirs::config_dir()` ignores it on macOS (it always
/// returns `~/Library/Application Support` there); the spec (§5) places config
/// at `~/.config/ws/`, which the XDG path honors. Falls back to the OS config
/// dir, then `.config`. Shared by `config_path` and `registry::registry_path`
/// so both always resolve to the same directory.
pub fn ws_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("ws");
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("ws")
}

pub fn config_path() -> PathBuf {
    ws_config_dir().join("config.toml")
}

/// Read the config, substituting defaults for anything unreadable or
/// unparseable. Deliberately lenient, and deliberately *not* changed to match
/// `set`'s strictness — see below.
///
/// M2. Under the corrected read-error discriminator ("trusted for an
/// irreversible write-back **or** a guard"), this belongs in the audit table
/// and did not have a row: its `sessions_root` feeds
/// `commands::deletes_whole_directory`, which chooses between
/// `remove_dir_all(path)` and `remove_dir_all(path/.ws)`. A lenient read is
/// gating an irreversible decision, which is normally exactly the shape this
/// project has spent two phases removing.
///
/// It stays lenient anyway, for two reasons.
///
/// It fails **closed**, and not by luck once you check it: the substituted
/// value is `~/.agent-workspaces`, and `deletes_whole_directory` only returns
/// `true` when the workspace is a direct child of the root it computed. Any
/// *wrong* root — the default standing in for a user's real one — makes the
/// registered path stop looking like a direct child, so the narrow `.ws`-only
/// branch wins. Substituting a default here can turn a whole-directory delete
/// into a `.ws`-only delete, never the reverse. The failure mode is "ws
/// deleted less than you asked", which is recoverable; the dangerous
/// direction is unreachable.
///
/// And erroring here cannot be done without breaking first run. An absent
/// `config.toml` is the normal state of a fresh install and must yield
/// defaults, and `load()` returns `Config` rather than `Result` precisely
/// because ~every call site (theme resolution, the statusline, row rendering)
/// has no sensible way to fail. Distinguishing *corrupt* from *absent* would
/// mean a `Result` and an error path through all of them, to protect a
/// decision that is already fail-closed.
///
/// The strictness lives at the two points where it changes an outcome
/// instead: `set` refuses to write back over a config it could not read or
/// parse (so a corrupt file is never silently replaced by defaults-plus-one-
/// key), and `remove_one` no longer trusts `sessions_root` as its only
/// evidence at all.
pub fn load() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn list(cfg: &Config) -> Vec<(String, String)> {
    vec![
        ("default_agent".into(), cfg.default_agent.clone()),
        ("limit_warn_5h".into(), cfg.limit_warn_5h.to_string()),
        ("limit_warn_week".into(), cfg.limit_warn_week.to_string()),
        ("theme".into(), cfg.theme.clone()),
        ("statusline".into(), cfg.statusline.to_string()),
        ("sessions_root".into(), cfg.sessions_root.clone()),
        ("limit_action".into(), cfg.limit_action.clone()),
        ("secrets_backend".into(), cfg.secrets_backend.clone()),
        ("task_prompt".into(), cfg.task_prompt.to_string()),
        ("notebook_prompt".into(), cfg.notebook_prompt.to_string()),
        ("resume_prompt".into(), cfg.resume_prompt.to_string()),
    ]
}

pub fn get(cfg: &Config, key: &str) -> Result<String> {
    list(cfg)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or_else(|| anyhow::anyhow!("unknown config key: {key}"))
}

pub fn set(key: &str, value: &str) -> Result<()> {
    let path = config_path();
    crate::txn::transaction(&path, || set_locked(&path, key, value))
}

/// The body of [`set`], run holding the config lock. Split out so the lock spans
/// the read *and* the write: two `ws config set` calls for different keys would
/// otherwise each read the same starting config and write back only their own
/// change, silently dropping the other.
fn set_locked(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    // Safe read: absent → defaults; present-but-unparseable → refuse (don't
    // clobber); present-but-unreadable (permission error, I/O error) → refuse
    // too, not default — defaulting here would write the whole config back
    // with only this one key set, over everything already there.
    let mut cfg: Config = match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid TOML ({e}); refusing to overwrite it. Fix it or move it aside.",
                path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    match key {
        // Validated, not stored blindly. An unknown agent used to be accepted
        // here and only surfaced much later, as `agents::for_id` bailing during a
        // launch — long after the user believed the setting had taken.
        "default_agent" => {
            crate::agents::for_id(value)?;
            cfg.default_agent = value.to_string();
        }
        "limit_warn_5h" => cfg.limit_warn_5h = value.parse()?,
        "limit_warn_week" => cfg.limit_warn_week = value.parse()?,
        "theme" => {
            if !matches!(value, "auto" | "light" | "dark") {
                bail!("theme must be auto, light, or dark");
            }
            cfg.theme = value.to_string();
        }
        "statusline" => cfg.statusline = parse_bool(value)?,
        "task_prompt" => cfg.task_prompt = parse_bool(value)?,
        "notebook_prompt" => cfg.notebook_prompt = parse_bool(value)?,
        "resume_prompt" => cfg.resume_prompt = parse_bool(value)?,
        // C3: `sessions_root` is the base of `remove_one`'s "is this a
        // workspace ws created?" test. `remove_one` no longer trusts it, but
        // an empty or relative root is a misconfiguration in its own right —
        // every workspace would be created relative to the cwd. Reject it at
        // the point of entry too.
        "sessions_root" => {
            let v = value.trim();
            if v.is_empty() {
                bail!("sessions_root must not be empty");
            }
            if !expand_tilde(v).is_absolute() {
                bail!("sessions_root must be an absolute path (got {v:?})");
            }
            cfg.sessions_root = v.to_string();
        }
        // Only `"warn"` was ever honoured (`internal::limit_check`); every other
        // value behaved as `handoff-stop` while `config set` reported success.
        // This module's own docs call a key that silently does nothing worse than
        // a missing one — the same is true of a *value*.
        "limit_action" => {
            if !matches!(value, "warn" | "handoff-stop") {
                bail!("limit_action must be warn or handoff-stop");
            }
            cfg.limit_action = value.to_string();
        }
        "secrets_backend" => {
            if !matches!(value, "auto" | "keyring" | "file") {
                bail!("secrets_backend must be auto, keyring, or file");
            }
            cfg.secrets_backend = value.to_string();
        }
        other => bail!("unknown config key: {other}"),
    }
    crate::atomic::atomic_write(path, toml::to_string_pretty(&cfg)?)?;
    Ok(())
}

fn parse_bool(v: &str) -> Result<bool> {
    match v {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => bail!("expected a boolean, got: {other}"),
    }
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

pub fn sessions_root(cfg: &Config) -> PathBuf {
    if let Ok(env) = std::env::var("WS_ROOT") {
        if !env.is_empty() {
            return expand_tilde(&env);
        }
    }
    expand_tilde(&cfg.sessions_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_spec_values() {
        let c = Config::default();
        assert_eq!(c.default_agent, "claude");
        assert_eq!(c.limit_warn_5h, 85);
        assert_eq!(c.limit_warn_week, 90);
    }

    #[test]
    fn get_unknown_is_err() {
        assert!(get(&Config::default(), "nope").is_err());
    }

    #[test]
    fn ws_root_env_wins() {
        std::env::set_var("WS_ROOT", "/tmp/ws-test-root");
        let r = sessions_root(&Config::default());
        std::env::remove_var("WS_ROOT");
        assert_eq!(r, PathBuf::from("/tmp/ws-test-root"));
    }

    #[test]
    fn limit_action_default_and_set() {
        assert_eq!(Config::default().limit_action, "handoff-stop");
        assert!(list(&Config::default()).iter().any(|(k,_)| k == "limit_action"));
    }

    /// C3: an empty or relative sessions_root is a foot-gun (it used to make
    /// `-rm` delete adopted projects whole). Refuse it at the point of entry.
    #[test]
    fn set_rejects_an_empty_or_relative_sessions_root() {
        let d = tempfile::TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path());
        assert!(set("sessions_root", "").is_err(), "empty must be rejected");
        assert!(set("sessions_root", "   ").is_err(), "whitespace-only too");
        assert!(set("sessions_root", "relative/dir").is_err(), "relative must be rejected");
        assert!(set("sessions_root", "/tmp/ws-abs").is_ok(), "an absolute path is fine");
        assert!(set("sessions_root", "~/ws-home").is_ok(), "and so is a ~-relative one");
    }

    #[test]
    fn set_refuses_to_clobber_unparseable_config() {
        // isolate config dir
        let d = tempfile::TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path());
        let p = config_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "this = is not : valid toml ][").unwrap();

        let r = set("default_agent", "codex");
        assert!(r.is_err(), "set must refuse to overwrite an unparseable config");
        // original bytes untouched
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "this = is not : valid toml ][");
    }

    #[test]
    #[cfg(unix)]
    fn set_refuses_to_clobber_an_unreadable_config() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

        let d = tempfile::TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path());
        let p = config_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let original = "default_agent = \"claude\"\n";
        std::fs::write(&p, original).unwrap();

        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&p, perms).unwrap();

        let result = set("default_agent", "codex");

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&p, perms).unwrap();

        assert!(result.is_err(), "set must refuse to overwrite an unreadable config");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original, "the original config must survive untouched");
    }
}
