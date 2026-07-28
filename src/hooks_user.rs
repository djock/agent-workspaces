//! User-defined hooks: one declaration, both agents.
//!
//! ws ships six built-in hooks. This module lets the user add their own without
//! forking the binary, and — the part a hand-written hook cannot do — have one
//! declaration register correctly for **both** Claude and Codex. The agents do
//! not name their tools the same way (`Write|Edit|MultiEdit|NotebookEdit` versus
//! `Write|Edit|apply_patch`), so a user writing raw JSON into two config files
//! has to know both vocabularies and keep them in step forever. Here they write
//! `tool = "file-write"` once and each agent's matcher is resolved for them.
//!
//! ## Trust
//!
//! `hooks.toml` is read **only** from ws's own config directory, never from a
//! workspace or a repository. A hook command runs on every matching event inside
//! the agent's process context, so a repo-local hook file would let a cloned
//! project execute code the moment someone opened it. `ws hooks check` prints
//! exactly what would be registered without writing anything, so the file can be
//! reviewed before `ws setup` acts on it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::hooksetup::{Scope, ToolKind};

/// Default per-hook timeout, matching the built-ins.
const DEFAULT_TIMEOUT_SECS: u64 = 10;
/// Upper bound. A hook is on the agent's critical path; ten minutes is already
/// far past "something is wrong", and an unbounded value would hang every turn.
const MAX_TIMEOUT_SECS: u64 = 600;

/// One `[[hook]]` entry as written by the user.
#[derive(Debug, Deserialize)]
struct RawHook {
    event: String,
    #[serde(default)]
    tool: Option<String>,
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    agents: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawFile {
    #[serde(default)]
    hook: Vec<RawHook>,
}

/// A validated user hook, ready to register.
#[derive(Debug, Clone, PartialEq)]
pub struct UserHook {
    pub event: String,
    pub scope: Scope,
    /// The user's command, `~`-expanded and verified to exist.
    pub command: PathBuf,
    pub timeout: u64,
    /// Agent ids this hook applies to.
    pub agents: Vec<String>,
    /// Stable, filesystem-safe identifier derived from the command and event.
    /// Names the generated shim, so re-running `setup` replaces rather than
    /// duplicates.
    pub slug: String,
}

pub fn hooks_toml_path() -> PathBuf {
    crate::config::ws_config_dir().join("hooks.toml")
}

/// Load and validate `hooks.toml`.
///
/// Absent → no user hooks. Unreadable or invalid → refuse, naming the entry:
/// silently dropping a hook the user believes is running is the failure mode this
/// whole module has to avoid.
pub fn load() -> Result<Vec<UserHook>> {
    load_from(&hooks_toml_path())
}

pub fn load_from(path: &Path) -> Result<Vec<UserHook>> {
    let raw = match crate::io_read::read_or_absent(path)? {
        None => return Ok(Vec::new()),
        Some(s) => s,
    };
    let parsed: RawFile = toml::from_str(&raw)
        .with_context(|| format!("{} is not valid TOML", path.display()))?;

    let mut out = Vec::new();
    for (i, h) in parsed.hook.into_iter().enumerate() {
        out.push(validate(h, i + 1, path)?);
    }
    Ok(out)
}

fn validate(h: RawHook, n: usize, path: &Path) -> Result<UserHook> {
    let where_ = format!("{} entry #{n}", path.display());

    if !crate::hooksetup::is_known_event(&h.event) {
        bail!(
            "{where_}: unknown event {:?}. ws knows: {}",
            h.event,
            crate::hooksetup::KNOWN_EVENTS.join(", ")
        );
    }

    let scope = match h.tool.as_deref() {
        None => Scope::Always,
        Some("shell") => Scope::Tool(ToolKind::Shell),
        Some("file-write") => Scope::Tool(ToolKind::FileWrite),
        Some(other) => bail!("{where_}: unknown tool {other:?} (want \"shell\" or \"file-write\")"),
    };

    let command = expand_tilde(&h.command);
    if !command.exists() {
        bail!("{where_}: command {} does not exist", command.display());
    }
    if !is_executable(&command) {
        bail!("{where_}: command {} is not executable", command.display());
    }

    let timeout = h.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
    if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
        bail!("{where_}: timeout must be between 1 and {MAX_TIMEOUT_SECS} seconds, got {timeout}");
    }

    let agents = match h.agents {
        None => vec!["claude".to_string(), "codex".to_string()],
        Some(list) if list.is_empty() => {
            bail!("{where_}: `agents` is empty — omit it to mean every agent")
        }
        Some(list) => {
            for a in &list {
                crate::agents::for_id(a).with_context(|| format!("{where_}: bad agent"))?;
            }
            list
        }
    };

    let slug = slug_for(&h.event, &command);
    Ok(UserHook { event: h.event, scope, command, timeout, agents, slug })
}

/// A stable shim name. Deliberately derived from event + command rather than the
/// entry's position, so reordering `hooks.toml` does not orphan shims.
fn slug_for(event: &str, command: &Path) -> String {
    let stem = command
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("hook")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>();
    let ev = event.to_ascii_lowercase();
    format!("{ev}-{stem}")
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Which of these hooks apply to `agent`, and which were skipped because the
/// agent does not support the event.
///
/// Codex has no `PostToolUseFailure`. Registering it there would write an entry
/// that can never fire — the same silent-no-op class as the matcher bug that once
/// disabled secret redaction on Codex — so it is skipped and reported instead.
pub fn for_agent<'a>(
    hooks: &'a [UserHook],
    agent: &dyn crate::agents::Agent,
) -> (Vec<&'a UserHook>, Vec<(&'a UserHook, &'static str)>) {
    let mut applies = Vec::new();
    let mut skipped = Vec::new();
    for h in hooks {
        if !h.agents.iter().any(|a| a == agent.id()) {
            continue;
        }
        if !agent.supports_event(&h.event) {
            skipped.push((h, agent.id()));
            continue;
        }
        applies.push(h);
    }
    (applies, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn write_toml(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("hooks.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn an_absent_file_means_no_user_hooks() {
        let d = TempDir::new().unwrap();
        assert!(load_from(&d.path().join("nope.toml")).unwrap().is_empty());
    }

    #[test]
    fn a_valid_entry_loads_with_defaults() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "my-hook.sh");
        let p = write_toml(
            d.path(),
            &format!("[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n", cmd.to_str().unwrap()),
        );
        let hooks = load_from(&p).unwrap();
        assert_eq!(hooks.len(), 1);
        let h = &hooks[0];
        assert_eq!(h.event, "Stop");
        assert_eq!(h.scope, Scope::Always, "no `tool` means every tool");
        assert_eq!(h.timeout, DEFAULT_TIMEOUT_SECS);
        assert_eq!(h.agents, vec!["claude", "codex"], "both agents by default");
        assert_eq!(h.slug, "stop-my-hook");
    }

    #[test]
    fn tool_scopes_resolve() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        for (tool, want) in [
            ("shell", Scope::Tool(ToolKind::Shell)),
            ("file-write", Scope::Tool(ToolKind::FileWrite)),
        ] {
            let p = write_toml(
                d.path(),
                &format!(
                    "[[hook]]\nevent = \"PreToolUse\"\ntool = {tool:?}\ncommand = {:?}\n",
                    cmd.to_str().unwrap()
                ),
            );
            assert_eq!(load_from(&p).unwrap()[0].scope, want);
        }
    }

    #[test]
    fn an_unknown_event_refuses_and_names_the_entry() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        let p = write_toml(
            d.path(),
            &format!("[[hook]]\nevent = \"OnTuesday\"\ncommand = {:?}\n", cmd.to_str().unwrap()),
        );
        let err = format!("{:#}", load_from(&p).unwrap_err());
        assert!(err.contains("OnTuesday"), "{err}");
        assert!(err.contains("entry #1"), "the error must locate the entry: {err}");
    }

    #[test]
    fn an_unknown_tool_refuses() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        let p = write_toml(
            d.path(),
            &format!(
                "[[hook]]\nevent = \"PreToolUse\"\ntool = \"network\"\ncommand = {:?}\n",
                cmd.to_str().unwrap()
            ),
        );
        assert!(format!("{:#}", load_from(&p).unwrap_err()).contains("network"));
    }

    /// A command that does not exist is the most likely mistake in this file, and
    /// the one whose silent failure is hardest to notice: the hook would be
    /// registered and simply never do anything.
    #[test]
    fn a_missing_command_refuses() {
        let d = TempDir::new().unwrap();
        let p = write_toml(d.path(), "[[hook]]\nevent = \"Stop\"\ncommand = \"/no/such/hook.sh\"\n");
        let err = format!("{:#}", load_from(&p).unwrap_err());
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn a_non_executable_command_refuses() {
        let d = TempDir::new().unwrap();
        let p_cmd = d.path().join("not-exec.sh");
        std::fs::write(&p_cmd, "#!/bin/sh\n").unwrap();
        let p = write_toml(
            d.path(),
            &format!("[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n", p_cmd.to_str().unwrap()),
        );
        let err = format!("{:#}", load_from(&p).unwrap_err());
        assert!(err.contains("not executable"), "{err}");
    }

    #[test]
    fn a_bad_timeout_refuses() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        for bad in [0, MAX_TIMEOUT_SECS + 1] {
            let p = write_toml(
                d.path(),
                &format!(
                    "[[hook]]\nevent = \"Stop\"\ncommand = {:?}\ntimeout = {bad}\n",
                    cmd.to_str().unwrap()
                ),
            );
            assert!(load_from(&p).is_err(), "timeout {bad} must be refused");
        }
    }

    #[test]
    fn an_unknown_agent_refuses() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        let p = write_toml(
            d.path(),
            &format!(
                "[[hook]]\nevent = \"Stop\"\ncommand = {:?}\nagents = [\"gpt5\"]\n",
                cmd.to_str().unwrap()
            ),
        );
        assert!(format!("{:#}", load_from(&p).unwrap_err()).contains("gpt5"));
    }

    #[test]
    fn an_empty_agents_list_refuses_rather_than_meaning_nothing() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        let p = write_toml(
            d.path(),
            &format!(
                "[[hook]]\nevent = \"Stop\"\ncommand = {:?}\nagents = []\n",
                cmd.to_str().unwrap()
            ),
        );
        assert!(load_from(&p).is_err());
    }

    #[test]
    fn invalid_toml_refuses_and_names_the_file() {
        let d = TempDir::new().unwrap();
        let p = write_toml(d.path(), "[[hook]\nevent =\n");
        let err = format!("{:#}", load_from(&p).unwrap_err());
        assert!(err.contains("hooks.toml"), "{err}");
    }

    /// The slug names the generated shim, so it must be stable across reorderings
    /// and safe as a filename.
    #[test]
    fn slugs_are_stable_and_filesystem_safe() {
        let s = slug_for("PostToolUse", Path::new("/home/me/bin/my hook!.sh"));
        assert_eq!(s, "posttooluse-my-hook-");
        assert!(!s.contains('/') && !s.contains(' '));
        assert_eq!(s, slug_for("PostToolUse", Path::new("/home/me/bin/my hook!.sh")));
    }

    #[test]
    fn for_agent_filters_by_declared_agents() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        let p = write_toml(
            d.path(),
            &format!(
                "[[hook]]\nevent = \"Stop\"\ncommand = {:?}\nagents = [\"claude\"]\n",
                cmd.to_str().unwrap()
            ),
        );
        let hooks = load_from(&p).unwrap();
        let (claude, _) = for_agent(&hooks, &crate::agents::claude::ClaudeAgent);
        let (codex, _) = for_agent(&hooks, &crate::agents::codex::CodexAgent);
        assert_eq!(claude.len(), 1);
        assert_eq!(codex.len(), 0, "not declared for codex");
    }

    /// The Codex-unsupported-event path: skipping must be *reported*, because a
    /// registered-but-never-fires hook is exactly the silent no-op that once
    /// disabled secret redaction on Codex.
    #[test]
    fn an_event_the_agent_cannot_fire_is_skipped_and_reported() {
        let d = TempDir::new().unwrap();
        let cmd = exe(d.path(), "h.sh");
        let p = write_toml(
            d.path(),
            &format!(
                "[[hook]]\nevent = \"PostToolUseFailure\"\ncommand = {:?}\n",
                cmd.to_str().unwrap()
            ),
        );
        let hooks = load_from(&p).unwrap();

        let (claude, claude_skipped) = for_agent(&hooks, &crate::agents::claude::ClaudeAgent);
        assert_eq!(claude.len(), 1, "Claude has PostToolUseFailure");
        assert!(claude_skipped.is_empty());

        let (codex, codex_skipped) = for_agent(&hooks, &crate::agents::codex::CodexAgent);
        assert_eq!(codex.len(), 0, "Codex has no PostToolUseFailure");
        assert_eq!(codex_skipped.len(), 1, "and the skip is reported, not silent");
    }

    #[test]
    fn several_entries_all_load() {
        let d = TempDir::new().unwrap();
        let a = exe(d.path(), "a.sh");
        let b = exe(d.path(), "b.sh");
        let p = write_toml(
            d.path(),
            &format!(
                "[[hook]]\nevent = \"Stop\"\ncommand = {:?}\n\n\
                 [[hook]]\nevent = \"PostToolUse\"\ntool = \"file-write\"\ncommand = {:?}\ntimeout = 30\n",
                a.to_str().unwrap(),
                b.to_str().unwrap()
            ),
        );
        let hooks = load_from(&p).unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[1].timeout, 30);
        assert_ne!(hooks[0].slug, hooks[1].slug, "distinct shims");
    }
}
