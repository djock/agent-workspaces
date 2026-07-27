use std::path::Path;
use std::process::Command;

use crate::agents::{Agent, LaunchCtx};
use crate::contract;
use crate::workspace::Workspace;

pub struct ClaudeAgent;

/// `state.toml` key for a drain's own chained session, kept separate from
/// the interactive `claude` key that `launch`/`has_prior_session` use. A
/// headless drain must never `--resume` a session the user started by hand —
/// it mints and resumes its own lineage under this key instead.
const DRAIN_SESSION_KEY: &str = "claude-drain";

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

    fn tool_matcher(&self, kind: crate::hooksetup::ToolKind) -> &'static str {
        use crate::hooksetup::ToolKind;
        match kind {
            ToolKind::Shell => "Bash",
            ToolKind::FileWrite => "Write|Edit",
        }
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
            // Read the outgoing id *before* overwriting it: once written, the
            // link between the old conversation and the new one is unrecoverable,
            // and that link is the whole of `ws -conversations`. Recording it here
            // rather than inside `write_session_id` keeps `contract` unaware of
            // the timeline, and this is the only place a Claude id is minted for
            // an interactive session.
            let prior = contract::read_session_id(&ws.state_toml(), self.id());
            let id = uuid::Uuid::new_v4().to_string();
            contract::write_session_id(&ws.state_toml(), self.id(), &id)?;
            crate::conversations::record_rotation(
                ws,
                self.id(),
                prior.as_deref(),
                &id,
                if prior.is_some() { "fresh" } else { "first" },
            );
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

    fn headless(
        &self,
        ws: &Workspace,
        prompt: &str,
        ctx: &LaunchCtx,
        _out_file: &Path,
    ) -> anyhow::Result<Command> {
        let mut cmd = Command::new(self.binary());
        cmd.arg("-p").arg(prompt).arg("--output-format").arg("json");
        // Chain onto the prior session so a multi-task drain keeps its
        // context — but the drain's OWN session, never the interactive one.
        // The first task of a drain mints a fresh id and records it under
        // DRAIN_SESSION_KEY; later tasks resume that id. If no drain session
        // has been minted yet (defensive: `run` always sets `fresh: true` on
        // task 1), fall back to a fresh run rather than resuming nothing.
        if ctx.fresh {
            let id = uuid::Uuid::new_v4().to_string();
            contract::write_session_id(&ws.state_toml(), DRAIN_SESSION_KEY, &id)?;
            cmd.arg("--session-id").arg(&id);
        } else if let Some(id) = contract::read_session_id(&ws.state_toml(), DRAIN_SESSION_KEY) {
            cmd.arg("--resume").arg(id);
        }
        cmd.current_dir(&ws.root)
            .env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", ws.memory_dir())
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
        Ok(cmd)
    }

    fn headless_succeeded(&self, out: &std::process::Output, _out_file: &Path) -> bool {
        if !out.status.success() {
            return false;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        match serde_json::from_str::<serde_json::Value>(text.trim()) {
            Ok(v) => v["is_error"].as_bool() == Some(false),
            Err(_) => false,
        }
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

    #[test]
    fn headless_asks_for_json_and_never_escalates_permissions() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: td.path().to_path_buf() };
        let out_file = td.path().join("out.txt");
        let cmd = ClaudeAgent.headless(&ws, "do the thing", &ctx, &out_file).unwrap();
        let a = args_of(&cmd);

        assert!(a.contains(&"-p".to_string()), "headless mode: {a:?}");
        assert!(a.contains(&"do the thing".to_string()), "prompt is passed: {a:?}");
        assert!(a.windows(2).any(|w| w[0] == "--output-format" && w[1] == "json"),
                "JSON result so failure is detectable: {a:?}");

        // The drain runs with nobody watching. Escalation flags must never
        // appear, and this assertion is the thing standing between an
        // unattended agent and the user's filesystem.
        for forbidden in ["--dangerously-skip-permissions", "--permission-mode",
                          "--allow-dangerously-skip-permissions"] {
            assert!(!a.iter().any(|x| x == forbidden), "{forbidden} must never be passed: {a:?}");
        }
        assert_eq!(env_of(&cmd, "WS_WORKSPACE").as_deref(), Some("proj"));
    }

    /// I1. The old version of this test wrote the session id by hand with
    /// `contract::write_session_id`, which only proved the `--resume` flag is
    /// emitted when *something* has already populated the file — not that
    /// `headless` itself ever populates it. Here nothing hand-writes the id:
    /// task 1 (`fresh: true`) must mint and persist its own session id, and
    /// task 2 (`fresh: false`) must resume exactly that id.
    #[test]
    fn headless_task_two_resumes_the_session_task_one_actually_created() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let out_file = td.path().join("out.txt");

        let fresh_ctx = LaunchCtx { fresh: true, sessions_root: td.path().to_path_buf() };
        let first = args_of(&ClaudeAgent.headless(&ws, "one", &fresh_ctx, &out_file).unwrap());
        let i = first.iter().position(|a| a == "--session-id").expect("task 1 passes --session-id: {first:?}");
        let minted_id = first[i + 1].clone();

        let resume_ctx = LaunchCtx { fresh: false, sessions_root: td.path().to_path_buf() };
        let second = args_of(&ClaudeAgent.headless(&ws, "two", &resume_ctx, &out_file).unwrap());
        assert!(
            second.windows(2).any(|w| w[0] == "--resume" && w[1] == minted_id),
            "task 2 must resume task 1's own drain session {minted_id:?}: {second:?}"
        );

        // The interactive `claude` key (what `launch` reads/writes) must stay
        // untouched — a drain must never look like it created or continued
        // the user's own interactive session.
        assert_eq!(
            crate::contract::read_session_id(&ws.state_toml(), "claude"),
            None,
            "a drain must not touch the interactive session key"
        );
    }

    /// I1. A drain must never silently resume a session an *interactive*
    /// `launch` created — that would mean an unattended agent appending
    /// turns to a human's own conversation transcript.
    #[test]
    fn headless_never_resumes_a_session_an_interactive_launch_created() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let out_file = td.path().join("out.txt");
        crate::contract::write_session_id(&ws.state_toml(), "claude", "the-users-own-session").unwrap();

        let ctx = LaunchCtx { fresh: false, sessions_root: td.path().to_path_buf() };
        let a = args_of(&ClaudeAgent.headless(&ws, "next", &ctx, &out_file).unwrap());
        assert!(
            !a.iter().any(|x| x == "the-users-own-session"),
            "a drain must never resume a session it did not itself create: {a:?}"
        );
        assert!(!a.contains(&"--resume".to_string()), "no drain session exists yet: {a:?}");
    }

    #[test]
    fn success_requires_both_a_clean_exit_and_is_error_false() {
        use std::os::unix::process::ExitStatusExt;
        let ok = |code: i32, stdout: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        let out_file = Path::new("/nonexistent/does-not-matter-for-claude");
        assert!(ClaudeAgent.headless_succeeded(&ok(0, r#"{"is_error":false,"subtype":"success"}"#), out_file));
        assert!(!ClaudeAgent.headless_succeeded(&ok(1, r#"{"is_error":false,"subtype":"success"}"#), out_file),
                "non-zero exit is a failure whatever the body says");
        assert!(!ClaudeAgent.headless_succeeded(&ok(0, r#"{"is_error":true,"subtype":"error"}"#), out_file),
                "is_error true is a failure");
        assert!(!ClaudeAgent.headless_succeeded(&ok(0, "not json at all"), out_file),
                "output we cannot read is a failure, never an assumed success");
        assert!(!ClaudeAgent.headless_succeeded(&ok(0, ""), out_file), "empty output is a failure");
    }
}
