use anyhow::Result;
use std::process::Command;

use crate::agents::{Agent, LaunchCtx};
use crate::contract;
use crate::workspace::Workspace;

pub struct CodexAgent;

impl Agent for CodexAgent {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn binary(&self) -> String {
        std::env::var("WS_CODEX_BIN").unwrap_or_else(|_| "codex".into())
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
        crate::hooksetup::codex_hooks_path()
    }

    /// Verified against Codex CLI 0.145.0 by capturing real hook payloads
    /// (`docs/2026-07-27-codex-hook-contract-verified.md`): a shell call arrives
    /// as `tool_name: "Bash"` — the same name Claude uses — but a file edit
    /// arrives as `"apply_patch"`, never as `Write` or `Edit`. `Write|Edit` alone
    /// therefore never matched, and secret redaction never ran on Codex.
    ///
    /// `Write|Edit` is kept in the alternation rather than replaced: it costs
    /// nothing, and if Codex later grows a direct file-write tool the hook keeps
    /// working instead of silently going dead again.
    fn tool_matcher(&self, kind: crate::hooksetup::ToolKind) -> &'static str {
        use crate::hooksetup::ToolKind;
        match kind {
            ToolKind::Shell => "Bash",
            ToolKind::FileWrite => "Write|Edit|apply_patch",
        }
    }

    /// Codex's event set, confirmed by string-probing the 0.145.0 binary and by
    /// a live capture of every event firing (see the verified-contract doc).
    ///
    /// `PostToolUseFailure` is absent — Codex has no equivalent — which is the
    /// entire reason `supports_event` exists rather than a shared constant.
    fn supports_event(&self, event: &str) -> bool {
        matches!(
            event,
            "SessionStart"
                | "SessionEnd"
                | "UserPromptSubmit"
                | "PreToolUse"
                | "PostToolUse"
                | "Stop"
                | "SubagentStart"
                | "SubagentStop"
                | "PermissionRequest"
                | "PreCompact"
                | "PostCompact"
        )
    }

    fn prompts_dir(&self) -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".codex")
            .join("prompts")
    }

    fn prompt_filename(&self, base: &str) -> String {
        format!("ws-{base}.md")
    }

    fn hook_trust_note(&self) -> Option<&'static str> {
        Some(
            "Run `/hooks` in Codex to trust the ws hooks. Until you do, ws cannot record \
             Codex session ids, so every launch starts a fresh session instead of resuming.",
        )
    }

    /// Resume an exact session, or start a fresh one.
    ///
    /// This used to be `codex resume --last` behind an ownership marker and an
    /// 81-line scan of `$CODEX_HOME/sessions` (up to 500 files × 64 KiB per
    /// launch) — all of it guesswork, because `--last` means "the most recent
    /// session in this directory" and there was no id to check it against. One
    /// bare `codex` run in the workspace redirected it permanently, and a codex
    /// that died before creating a session left the marker pointing at nothing,
    /// so every later launch retried `--last` against a session that never
    /// existed.
    ///
    /// Two verified facts replaced the whole apparatus: `codex resume
    /// [SESSION_ID]` accepts a UUID, and Codex's SessionStart hook payload
    /// carries `session_id`. So `internal::record_session_identity` records the
    /// real id and this function addresses it directly.
    ///
    /// The fallback is deliberate and visible: no recorded id means fresh, with a
    /// line saying so. That is the state when Codex's hooks have not been trusted
    /// via `/hooks` — `ws -doctor` says as much — and a wrong-session resume is a
    /// worse outcome than an honest fresh start.
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        let prior = contract::read_session_id(&ws.state_toml(), self.id());
        match (ctx.fresh, prior) {
            (false, Some(id)) => {
                cmd.arg("resume").arg(&id);
            }
            (false, None) => {
                eprintln!(
                    "ws: no recorded codex session for this workspace — starting a fresh one. \
                     (If you expected to resume, check `ws -doctor`: Codex only reports session \
                     ids once its hooks are trusted via `/hooks`.)"
                );
            }
            (true, _) => {}
        }
        cmd.current_dir(&ws.root)
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root)
            // The SessionStart hook records the session id under this agent, and
            // has no other way to know which agent it is running inside.
            .env("WS_AGENT", self.id());
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, LaunchCtx};
    use tempfile::TempDir;

    /// One module-level lock: these tests write `state.toml` under a TempDir and
    /// read process-global env, and fn-local statics would each be a *different*
    /// mutex, serializing nothing.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ws_at(d: &std::path::Path) -> Workspace {
        std::fs::create_dir_all(d.join(".ws/local")).unwrap();
        Workspace { name: "proj".into(), root: d.to_path_buf() }
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    fn ctx(fresh: bool) -> LaunchCtx {
        LaunchCtx { fresh, sessions_root: std::path::PathBuf::from("/tmp/ws-root") }
    }

    #[test]
    fn context_file_and_binary() {
        assert_eq!(CodexAgent.context_file(), "AGENTS.md");
        assert_eq!(CodexAgent.id(), "codex");
    }

    /// The core of the refocus: a recorded id is resumed *by id*, never by
    /// `--last`. `--last` is what made a bare `codex` run in the same directory
    /// silently steal ws's resume target.
    #[test]
    fn a_recorded_session_is_resumed_by_id() {
        let _g = lock();
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let id = "019fa430-273a-7fa2-a329-89fab081f383";
        contract::write_session_id(&ws.state_toml(), "codex", id).unwrap();

        let cmd = CodexAgent.launch(&ws, &ctx(false)).unwrap();
        assert_eq!(args(&cmd), vec!["resume".to_string(), id.to_string()]);
        assert!(!args(&cmd).contains(&"--last".to_string()), "--last must be gone");
    }

    #[test]
    fn no_recorded_session_launches_fresh() {
        let _g = lock();
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let cmd = CodexAgent.launch(&ws, &ctx(false)).unwrap();
        assert!(args(&cmd).is_empty(), "a fresh codex takes no resume args: {:?}", args(&cmd));
    }

    /// `--fresh` is an explicit override and must win over a recorded id.
    #[test]
    fn fresh_overrides_a_recorded_session() {
        let _g = lock();
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        contract::write_session_id(&ws.state_toml(), "codex", "some-uuid").unwrap();

        let cmd = CodexAgent.launch(&ws, &ctx(true)).unwrap();
        assert!(args(&cmd).is_empty(), "fresh must not resume: {:?}", args(&cmd));
    }

    /// A `state.toml` that has become unreadable or corrupt must degrade to a
    /// fresh launch, never panic and never resume a guess. This is the shape that
    /// used to `.unwrap()` on the Claude side.
    #[test]
    fn a_corrupt_state_toml_degrades_to_fresh() {
        let _g = lock();
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        std::fs::write(ws.state_toml(), "this is not toml {{{").unwrap();

        let cmd = CodexAgent.launch(&ws, &ctx(false)).unwrap();
        assert!(args(&cmd).is_empty());
    }

    /// Another agent's recorded id must not be mistaken for codex's.
    #[test]
    fn a_claude_session_id_is_not_resumed_as_codex() {
        let _g = lock();
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        contract::write_session_id(&ws.state_toml(), "claude", "claude-only-id").unwrap();

        let cmd = CodexAgent.launch(&ws, &ctx(false)).unwrap();
        assert!(args(&cmd).is_empty(), "codex has no session of its own yet");
    }

    #[test]
    fn launch_exports_the_agent_id_for_the_hook() {
        let _g = lock();
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let cmd = CodexAgent.launch(&ws, &ctx(true)).unwrap();
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().to_string(), v.map(|v| v.to_string_lossy().to_string()))
            })
            .collect();
        assert!(
            envs.contains(&("WS_AGENT".to_string(), Some("codex".to_string()))),
            "the SessionStart hook cannot file the session id without WS_AGENT: {envs:?}"
        );
    }

    /// Codex has no `PostToolUseFailure`, and a user hook on it must be
    /// reported rather than registered into a config where it can never fire.
    #[test]
    fn supports_event_excludes_what_codex_cannot_fire() {
        assert!(CodexAgent.supports_event("SessionStart"));
        assert!(CodexAgent.supports_event("PermissionRequest"));
        assert!(!CodexAgent.supports_event("PostToolUseFailure"));
        assert!(!CodexAgent.supports_event("NotAnEvent"));
    }

    #[test]
    fn prompts_are_namespaced_so_they_cannot_collide_with_the_users_own() {
        assert_eq!(CodexAgent.prompt_filename("task"), "ws-task.md");
    }

    #[test]
    fn the_trust_note_explains_the_identity_consequence() {
        let note = CodexAgent.hook_trust_note().unwrap();
        assert!(note.contains("/hooks"));
        assert!(
            note.contains("session ids") || note.contains("resuming"),
            "the note must say what breaks without trust: {note}"
        );
    }
}
