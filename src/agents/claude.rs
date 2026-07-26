use std::process::Command;

use crate::agents::{Agent, LaunchCtx};
use crate::contract;
use crate::workspace::Workspace;

pub struct ClaudeAgent;

impl Agent for ClaudeAgent {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn binary(&self) -> String {
        std::env::var("WS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
    }

    fn is_installed(&self) -> bool {
        Command::new(self.binary())
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn context_file(&self) -> &'static str {
        "CLAUDE.local.md"
    }

    fn has_prior_session(&self, ws: &Workspace) -> bool {
        contract::read_session_id(&ws.state_toml(), self.id()).is_some()
    }

    fn hooks_config_path(&self) -> std::path::PathBuf {
        crate::hooksetup::claude_settings_path()
    }

    fn prompts_dir(&self) -> std::path::PathBuf {
        crate::prompts::commands_dir()
    }

    fn prompt_filename(&self, base: &str) -> String {
        format!("{base}.md")
    }

    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> anyhow::Result<Command> {
        let mut cmd = Command::new(self.binary());
        if ctx.fresh || !self.has_prior_session(ws) {
            let id = uuid::Uuid::new_v4().to_string();
            contract::write_session_id(&ws.state_toml(), self.id(), &id)?;
            cmd.arg("--session-id").arg(&id);
        } else {
            let id = contract::read_session_id(&ws.state_toml(), self.id()).unwrap();
            cmd.arg("--resume").arg(&id);
        }
        cmd.current_dir(&ws.root)
            .env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", ws.memory_dir())
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
    use std::ffi::OsStr;
    use tempfile::TempDir;

    fn ws_at(dir: &std::path::Path) -> Workspace {
        std::fs::create_dir_all(dir.join(".ws/local")).unwrap();
        Workspace { name: "proj".into(), root: dir.to_path_buf() }
    }
    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }
    fn env_of(cmd: &std::process::Command, key: &str) -> Option<String> {
        cmd.get_envs().find(|(k, _)| *k == OsStr::new(key)).and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().to_string())
    }

    #[test]
    fn fresh_uses_session_id_and_records_it() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into() };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        let a = args_of(&cmd);
        assert_eq!(a[0], "--session-id");
        // recorded to state.toml
        assert_eq!(crate::contract::read_session_id(&ws.state_toml(), "claude"), Some(a[1].clone()));
    }

    #[test]
    fn resume_uses_recorded_id() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        crate::contract::write_session_id(&ws.state_toml(), "claude", "uuid-xyz").unwrap();
        let ctx = LaunchCtx { fresh: false, sessions_root: "/root".into() };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        assert_eq!(args_of(&cmd), vec!["--resume", "uuid-xyz"]);
    }

    #[test]
    fn sets_memory_redirect_and_ws_env() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into() };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        assert_eq!(env_of(&cmd, "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"), Some(ws.memory_dir().to_string_lossy().to_string()));
        assert_eq!(env_of(&cmd, "WS_WORKSPACE"), Some("proj".into()));
        assert_eq!(env_of(&cmd, "WS_DIR"), Some(ws.root.to_string_lossy().to_string()));
        assert_eq!(env_of(&cmd, "WS_ROOT"), Some("/root".into()));
    }

    #[test]
    fn binary_override_env() {
        std::env::set_var("WS_CLAUDE_BIN", "/fake/claude");
        let b = ClaudeAgent.binary();
        std::env::remove_var("WS_CLAUDE_BIN");
        assert_eq!(b, "/fake/claude");
    }
}
