use anyhow::Result;
use std::process::Command;

use crate::agents::{Agent, LaunchCtx};
use crate::workspace::Workspace;

pub struct CodexAgent;

fn marker_present(ws: &Workspace) -> bool {
    std::fs::read_to_string(ws.state_toml())
        .ok()
        .and_then(|s| toml::from_str::<toml::Table>(&s).ok())
        .and_then(|t| t.get("codex").and_then(|c| c.get("launched")).and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// Record "codex has been launched here" in `.ws/local/state.toml`.
///
/// C2: this shares the file with `contract::write_session_id` (hook handlers),
/// so it must use the same clobber-safe path — per-process temp name, cleanup
/// on failure, and a refusal to overwrite a `state.toml` that failed to parse.
/// It previously used a fixed `state.toml.tmp`, replaced an unparseable file
/// with a fresh table, and dropped every other agent's `session_id` on the way.
fn record_marker(ws: &Workspace) -> Result<()> {
    let state = ws.state_toml();
    let mut t = crate::contract::read_state_table(&state)?;
    let mut e = match t.get("codex").and_then(|v| v.as_table()) {
        Some(existing) => existing.clone(),
        None => toml::Table::new(),
    };
    e.insert("launched".into(), toml::Value::Boolean(true));
    t.insert("codex".into(), toml::Value::Table(e));
    crate::contract::write_state_table(&state, &t)
}

impl Agent for CodexAgent {
    fn id(&self) -> &'static str { "codex" }
    fn binary(&self) -> String { std::env::var("WS_CODEX_BIN").unwrap_or_else(|_| "codex".into()) }
    fn is_installed(&self) -> bool {
        Command::new(self.binary()).arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }
    fn context_file(&self) -> &'static str { "AGENTS.md" }
    fn has_prior_session(&self, ws: &Workspace) -> bool { marker_present(ws) }

    fn hooks_config_path(&self) -> std::path::PathBuf {
        crate::hooksetup::codex_hooks_path()
    }

    fn prompts_dir(&self) -> std::path::PathBuf {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join(".codex").join("prompts")
    }

    fn prompt_filename(&self, base: &str) -> String {
        format!("ws-{base}.md")
    }

    fn hook_trust_note(&self) -> Option<&'static str> {
        Some("Run `/hooks` in Codex to trust the ws hooks before they take effect.")
    }
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        if ctx.fresh || !marker_present(ws) {
            record_marker(ws)?;               // fresh: `codex`
        } else {
            cmd.arg("resume").arg("--last");  // resume most recent in this cwd
        }
        cmd.current_dir(&ws.root)
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, LaunchCtx};
    use crate::workspace::Workspace;
    use tempfile::TempDir;

    fn ws_at(d: &std::path::Path) -> Workspace {
        std::fs::create_dir_all(d.join(".ws/local")).unwrap();
        Workspace { name: "proj".into(), root: d.to_path_buf() }
    }
    fn args(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn fresh_launches_codex_and_records_marker() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into() };
        let cmd = CodexAgent.launch(&ws, &ctx).unwrap();
        assert!(args(&cmd).is_empty(), "fresh codex takes no resume args");
        assert!(CodexAgent.has_prior_session(&ws), "marker recorded after fresh launch");
    }

    #[test]
    fn resume_uses_resume_last() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        // simulate a prior launch
        CodexAgent.launch(&ws, &LaunchCtx { fresh: true, sessions_root: "/r".into() }).unwrap();
        let cmd = CodexAgent.launch(&ws, &LaunchCtx { fresh: false, sessions_root: "/r".into() }).unwrap();
        assert_eq!(args(&cmd), vec!["resume", "--last"]);
    }

    /// C2. `record_marker` and `contract::write_session_id` write the same
    /// file from different processes; neither may throw away the other's keys,
    /// and neither may replace a `state.toml` it could not parse.
    #[test]
    fn record_marker_preserves_other_keys_and_refuses_a_corrupt_state_toml() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let state = ws.state_toml();

        // A hook handler got there first.
        crate::contract::write_session_id(&state, "claude", "abc-123").unwrap();
        record_marker(&ws).unwrap();
        assert_eq!(
            crate::contract::read_session_id(&state, "claude"),
            Some("abc-123".into()),
            "another agent's session id must survive the codex marker"
        );
        assert!(marker_present(&ws));
        // ...and the reverse direction: a later session-id write keeps it.
        crate::contract::write_session_id(&state, "codex", "xyz").unwrap();
        assert!(marker_present(&ws), "the launched marker must survive a session_id write");

        // Corrupt → refuse, byte for byte.
        std::fs::write(&state, "not toml {{{").unwrap();
        assert!(record_marker(&ws).is_err(), "must not replace an unparseable state.toml");
        assert!(
            crate::contract::write_session_id(&state, "claude", "z").is_err(),
            "and neither may write_session_id"
        );
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "not toml {{{");
    }

    /// The temp file must not be a path two processes share.
    #[test]
    fn record_marker_uses_a_per_process_temp_name() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let fixed = ws.state_toml().with_extension("toml.tmp");
        // Squat the old fixed temp path with a file we own.
        std::fs::write(&fixed, "another process was mid-write").unwrap();
        record_marker(&ws).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fixed).unwrap(),
            "another process was mid-write",
            "a fixed temp name is a shared path between processes; it must not be used"
        );
        assert!(marker_present(&ws));
    }

    #[test]
    fn context_file_and_binary() {
        assert_eq!(CodexAgent.context_file(), "AGENTS.md");
        std::env::set_var("WS_CODEX_BIN", "/fake/codex");
        let b = CodexAgent.binary();
        std::env::remove_var("WS_CODEX_BIN");
        assert_eq!(b, "/fake/codex");
    }
}
