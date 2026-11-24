//! Google's Antigravity CLI (`agy`), the CLI that shares the `~/.gemini` home.
//!
//! Everything here was measured against agy 1.5.16: `agy --help`, the hooks
//! guide embedded in the binary, and three live runs that logged every hook
//! payload (2026-11-24/25, recorded in the workspace notebook). agy reads
//! `AGENTS.md`, so the ws protocol reaches it through the same context file as
//! Codex.
//!
//! Its hooks differ from Claude's and Codex's in shape, not in purpose. The
//! payload is camelCase and context goes back as `injectSteps`; `hookio`
//! translates both ways, so the handlers in `internal` stay agent-blind.

use anyhow::Result;
use std::process::Command;

use crate::agents::{Agent, LaunchCtx, LaunchMode};
use crate::contract;
use crate::workspace::Workspace;

pub struct AgyAgent;

impl Agent for AgyAgent {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn binary(&self) -> String {
        std::env::var("WS_AGY_BIN").unwrap_or_else(|_| "agy".into())
    }

    fn is_installed(&self) -> bool {
        Command::new(self.binary())
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn context_file(&self) -> &'static str {
        "AGENTS.md"
    }

    fn hooks_config_path(&self) -> std::path::PathBuf {
        crate::hooksetup::agy_hooks_path()
    }

    fn hooks_format(&self) -> crate::hooksetup::HooksFormat {
        crate::hooksetup::HooksFormat::Agy
    }

    /// Seen live: a shell call is `run_command` (`args.CommandLine`), a new file
    /// is `write_file` (`args.Path`). The edit tools come from the binary's
    /// strings and have not been seen in a live payload yet; listing them costs
    /// nothing if they never fire, and leaving them out would let an edit skip
    /// redaction silently.
    fn tool_matcher(&self, kind: crate::hooksetup::ToolKind) -> &'static str {
        use crate::hooksetup::ToolKind;
        match kind {
            ToolKind::Shell => "run_command",
            ToolKind::FileWrite => {
                "write_file|create_file|edit_file|replace_file_content|multi_replace_file_content"
            }
        }
    }

    /// agy fires SessionStart, PreToolUse, PostToolUse, PreInvocation,
    /// PostInvocation and Stop. ws registers its prompt-time hook on
    /// PreInvocation, so `UserPromptSubmit` counts as supported here.
    ///
    /// `PreToolUse` is deliberately reported unsupported: agy denies the tool
    /// call unless the hook prints `{"decision":"allow"}` (seen live: `{}`
    /// denied it), so a user hook written for Claude's contract would block
    /// every tool. There is no `SessionEnd` at all.
    fn supports_event(&self, event: &str) -> bool {
        matches!(event, "SessionStart" | "UserPromptSubmit" | "PostToolUse" | "Stop")
    }

    /// `--dangerously-skip-permissions` approves every tool. `--mode
    /// accept-edits` lets edits land and still asks for commands (seen live:
    /// a shell call was refused headless under it).
    fn mode_args(&self, mode: LaunchMode) -> Vec<String> {
        match mode {
            LaunchMode::Loco => vec!["--dangerously-skip-permissions".into()],
            LaunchMode::Sane => vec!["--mode".into(), "accept-edits".into()],
        }
    }

    /// agy's global workflows. Not yet confirmed that agy lists them as slash
    /// commands; at worst they are files the user can point it at.
    fn prompts_dir(&self) -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".gemini")
            .join("config")
            .join("global_workflows")
    }

    fn prompt_filename(&self, base: &str) -> String {
        format!("ws-{base}.md")
    }

    /// Interactive with an opening prompt (`-i`), in the scratch directory, with
    /// each project granted by `--add-dir`.
    fn dispatch(
        &self,
        scratch: &std::path::Path,
        dirs: &[std::path::PathBuf],
        prompt: &str,
        mode: Option<LaunchMode>,
    ) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        for d in dirs {
            cmd.arg("--add-dir").arg(d);
        }
        if let Some(m) = mode {
            cmd.args(self.mode_args(m));
        }
        cmd.arg("-i").arg(prompt).current_dir(scratch);
        for key in crate::agents::WORKSPACE_ENV {
            cmd.env_remove(key);
        }
        Ok(cmd)
    }

    /// Resume the recorded conversation by id, or start fresh.
    ///
    /// agy assigns the id itself; its SessionStart payload carries it as
    /// `conversationId` (the same id as its `brain/<id>` directory), and
    /// `internal::record_session_identity` files it under `agy`. As with Codex,
    /// no recorded id means a fresh start, never a guess like `--continue`.
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        let prior = contract::read_session_id(&ws.state_toml(), self.id());
        match (ctx.fresh, prior) {
            (false, Some(id)) => {
                cmd.arg("--conversation").arg(&id);
            }
            (false, None) => {
                eprintln!(
                    "ws: no recorded agy conversation for this workspace — starting a fresh one."
                );
            }
            (true, _) => {}
        }
        if let Some(mode) = ctx.mode {
            cmd.args(self.mode_args(mode));
        }
        if let Some(p) = &ctx.initial_prompt {
            cmd.arg("-i").arg(p);
        }
        cmd.current_dir(&ws.root)
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root)
            .env("WS_AGENT", self.id());
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws_at(d: &std::path::Path) -> Workspace {
        std::fs::create_dir_all(d.join(".ws/local")).unwrap();
        Workspace { name: "proj".into(), root: d.to_path_buf() }
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    fn ctx(fresh: bool) -> LaunchCtx {
        LaunchCtx {
            fresh,
            sessions_root: std::path::PathBuf::from("/tmp/ws-root"),
            mode: None,
            initial_prompt: None,
        }
    }

    #[test]
    fn a_recorded_conversation_is_resumed_by_id() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        contract::write_session_id(&ws.state_toml(), "agy", "291f5260").unwrap();
        let cmd = AgyAgent.launch(&ws, &ctx(false)).unwrap();
        assert_eq!(args(&cmd), vec!["--conversation", "291f5260"]);
        let env: Vec<_> = cmd.get_envs().collect();
        assert!(env.iter().any(|(k, v)| *k == "WS_AGENT" && v.unwrap() == "agy"));
    }

    #[test]
    fn fresh_wins_over_a_recorded_id_and_no_id_starts_fresh() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        assert!(args(&AgyAgent.launch(&ws, &ctx(false)).unwrap()).is_empty());
        contract::write_session_id(&ws.state_toml(), "agy", "x").unwrap();
        assert!(args(&AgyAgent.launch(&ws, &ctx(true)).unwrap()).is_empty());
    }

    #[test]
    fn mode_and_opening_prompt_follow_the_resume() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        contract::write_session_id(&ws.state_toml(), "agy", "id1").unwrap();
        let mut c = ctx(false);
        c.mode = Some(LaunchMode::Sane);
        c.initial_prompt = Some("go".into());
        assert_eq!(
            args(&AgyAgent.launch(&ws, &c).unwrap()),
            vec!["--conversation", "id1", "--mode", "accept-edits", "-i", "go"]
        );
    }

    #[test]
    fn dispatch_grants_dirs_and_strips_the_workspace_env() {
        let d = TempDir::new().unwrap();
        let cmd = AgyAgent
            .dispatch(d.path(), &[d.path().join("a")], "do it", Some(LaunchMode::Loco))
            .unwrap();
        let a = args(&cmd);
        assert_eq!(a[0], "--add-dir");
        assert_eq!(&a[2..], ["--dangerously-skip-permissions", "-i", "do it"]);
        assert!(cmd.get_envs().any(|(k, v)| k == "WS_AGENT" && v.is_none()));
    }

    #[test]
    fn no_session_end_and_no_pre_tool_use() {
        assert!(!AgyAgent.supports_event("SessionEnd"));
        assert!(!AgyAgent.supports_event("PreToolUse"));
        assert!(AgyAgent.supports_event("UserPromptSubmit"));
        assert!(AgyAgent
            .tool_matcher(crate::hooksetup::ToolKind::FileWrite)
            .contains("write_file"));
    }
}
