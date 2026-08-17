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

    fn hooks_config_path(&self) -> std::path::PathBuf {
        crate::hooksetup::claude_settings_path()
    }

    fn tool_matcher(&self, kind: crate::hooksetup::ToolKind) -> &'static str {
        use crate::hooksetup::ToolKind;
        match kind {
            ToolKind::Shell => "Bash",
            // Every Claude tool that puts content on disk, not just the two
            // obvious ones: `MultiEdit` and `NotebookEdit` write files exactly
            // as `Write` does, and secret redaction never saw either of them.
            ToolKind::FileWrite => "Write|Edit|MultiEdit|NotebookEdit",
        }
    }

    /// Claude's event set, as of Claude Code 2.x. `PostToolUseFailure` is the
    /// one Codex lacks, which is why this is asked per agent at all.
    fn supports_event(&self, event: &str) -> bool {
        matches!(
            event,
            "SessionStart"
                | "SessionEnd"
                | "UserPromptSubmit"
                | "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "Stop"
                | "SubagentStart"
                | "SubagentStop"
                | "PermissionRequest"
                | "PreCompact"
                | "PostCompact"
        )
    }

    /// Verified against the installed `claude --help`: `--permission-mode` takes
    /// `acceptEdits | auto | bypassPermissions | manual | dontAsk | plan`, and
    /// bypassing is *also* reachable as its own flag.
    ///
    /// Loco uses `--dangerously-skip-permissions` rather than
    /// `--permission-mode bypassPermissions` deliberately: it is the spelling
    /// Claude Code treats as the deliberate opt-in (it refuses outright in a few
    /// contexts, such as running as root), so a refusal surfaces as a refusal
    /// instead of a session that quietly declines to bypass anything.
    fn mode_args(&self, mode: crate::agents::LaunchMode) -> Vec<String> {
        use crate::agents::LaunchMode;
        match mode {
            LaunchMode::Loco => vec!["--dangerously-skip-permissions".into()],
            LaunchMode::Sane => vec!["--permission-mode".into(), "acceptEdits".into()],
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
        // Read the id once and branch on that single value. The previous
        // version asked `has_prior_session` (a read) whether to resume, then
        // re-read the same file to get the id to `--resume` with — two reads
        // of the same file racing a concurrent writer. `read_session_id` maps
        // every failure (missing, unreadable, corrupt) to `None`, so a writer
        // that landed between the two reads could flip the second one to
        // `None` after the first said `Some`, and the `.unwrap()` that used
        // to sit here panicked mid-launch. One read now feeds both branches,
        // so there is no window left for the two to disagree.
        let prior = contract::read_session_id(&ws.state_toml(), self.id());
        match (ctx.fresh, prior) {
            (false, Some(id)) => {
                cmd.arg("--resume").arg(&id);
            }
            (_, _prior) => {
                // Read the outgoing id *before* overwriting it: once written, the
                // link between the old conversation and the new one is unrecoverable,
                // and that link is the whole of `ws -conversations`. Recording it here
                // rather than inside `write_session_id` keeps `contract` unaware of
                // the timeline, and this is the only place a Claude id is minted for
                // an interactive session.
                let id = uuid::Uuid::new_v4().to_string();
                // The id is recorded here so `--resume` can find it; the
                // *lineage* is recorded by the SessionStart hook, which is the
                // only place both agents' ids are observable from one code path.
                if let Err(e) = contract::write_session_id(&ws.state_toml(), self.id(), &id) {
                    // `write_session_id` refuses to overwrite a state.toml it
                    // could not parse, and that protection must not become a
                    // launch failure: the id above still starts Claude, it just
                    // cannot be resumed later. Warn and degrade.
                    eprintln!(
                        "ws: warning: could not record the new claude session in {} — \
                         continuing with a fresh session that a later launch will not be \
                         able to resume until that file is repaired or removed: {e:#}",
                        ws.state_toml().display()
                    );
                }
                cmd.arg("--session-id").arg(&id);
            }
        }
        // `ctrl+g` (`chat:externalEditor`) writes the composer buffer to a temp
        // file and runs `$EDITOR` on it, replacing the composer with what comes
        // back — the only route by which anything can rewrite prompt text, since
        // a hook can append context but never replace it. Pointing `$EDITOR` at
        // ws's shim turns that round-trip into a rewrite.
        //
        // Opt-in (`ws config set rewrite true`): taking over `$EDITOR` for a
        // whole session is intrusive enough to be asked for. The user's own
        // editor is recorded so the shim can hand it every file that is not a
        // composer buffer — `/memory` must keep working.
        //
        // The path is quoted: `$EDITOR` is a command *line*, split on
        // whitespace by whoever runs it, so an unquoted ws living under a path
        // with a space (`~/Library/Application Support/…`, a user directory with
        // a space in the name) would break ctrl+g *and* — because the shim never
        // runs, so it never delegates — the user's ordinary editor for the whole
        // session.
        if crate::config::load().rewrite {
            if let Ok(exe) = std::env::current_exe() {
                let real = std::env::var("EDITOR").unwrap_or_default();
                cmd.env("WS_REAL_EDITOR", real).env(
                    "EDITOR",
                    format!("{} internal rewrite", crate::hooksetup::shell_quote_path(&exe)),
                );
            }
        }
        // After the session args, so `--resume <id>` keeps its value adjacent.
        if let Some(mode) = ctx.mode {
            cmd.args(self.mode_args(mode));
        }
        cmd.current_dir(&ws.root)
            .env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", ws.memory_dir())
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
    use crate::workspace::Workspace;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    fn ws_at(dir: &std::path::Path) -> Workspace {
        std::fs::create_dir_all(dir.join(".ws/local")).unwrap();
        Workspace { name: "proj".into(), root: dir.to_path_buf() }
    }
    /// The args a launch command would run with, for asserting resume-vs-fresh.
    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    fn env_of(cmd: &std::process::Command, key: &str) -> Option<String> {
        cmd.get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().to_string())
    }

    #[test]
    fn fresh_uses_session_id_and_records_it() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into(), mode: None };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        let a = args_of(&cmd);
        assert_eq!(a[0], "--session-id");
        // recorded to state.toml
        assert_eq!(
            crate::contract::read_session_id(&ws.state_toml(), "claude"),
            Some(a[1].clone())
        );
    }

    #[test]
    fn resume_uses_recorded_id() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        crate::contract::write_session_id(&ws.state_toml(), "claude", "uuid-xyz").unwrap();
        let ctx = LaunchCtx { fresh: false, sessions_root: "/root".into(), mode: None };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        assert_eq!(args_of(&cmd), vec!["--resume", "uuid-xyz"]);
    }

    /// Launch-path panic. The old `launch` asked `has_prior_session` (a read)
    /// whether to resume, then re-read the same file in the `else`-branch to
    /// get the id — two reads of one file, and `read_session_id` maps every
    /// failure (missing, unreadable, corrupt) to `None`. The panic itself
    /// needed a concurrent writer to land between those two reads (a file
    /// corrupt from the start never reaches the second read at all: the
    /// first read already says `None`, so the `if` branch — fresh — is
    /// taken). What this test pins is the half that no longer depends on
    /// timing: with the double read collapsed into one, a `state.toml` that
    /// is corrupt from the start deterministically degrades to a fresh
    /// launch, never a panic, regardless of which of the two old reads it
    /// would have hit.
    #[test]
    fn corrupt_state_toml_falls_back_to_fresh_without_panicking() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        std::fs::write(ws.state_toml(), "not toml {{{").unwrap();
        let ctx = LaunchCtx { fresh: false, sessions_root: "/root".into(), mode: None };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        let a = args_of(&cmd);
        assert_eq!(
            a[0], "--session-id",
            "a corrupt state.toml must fall back to fresh, not resume or panic: {a:?}"
        );
    }

    /// Same failure mode, but the file is missing outright rather than
    /// corrupt: `read_session_id` also maps a missing file to `None`, so
    /// `ctx.fresh: false` with nothing on disk must still launch fresh.
    #[test]
    fn absent_state_toml_with_fresh_false_still_launches_fresh() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: false, sessions_root: "/root".into(), mode: None };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        let a = args_of(&cmd);
        assert_eq!(
            a[0], "--session-id",
            "no state.toml at all must fall back to fresh, not panic: {a:?}"
        );
    }

    /// And unreadable: permission-denied rather than corrupt or absent.
    /// `std::fs::read_to_string` fails the same way, so this exercises the
    /// same `None` path through a different OS error.
    #[cfg(unix)]
    #[test]
    fn unreadable_state_toml_falls_back_to_fresh_without_panicking() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        std::fs::write(ws.state_toml(), "[claude]\nsession_id = \"abc\"\n").unwrap();
        std::fs::set_permissions(ws.state_toml(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let ctx = LaunchCtx { fresh: false, sessions_root: "/root".into(), mode: None };
        let result = ClaudeAgent.launch(&ws, &ctx);
        // Restore permissions unconditionally so TempDir cleanup can remove the file.
        std::fs::set_permissions(ws.state_toml(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let a = args_of(&result.unwrap());
        assert_eq!(
            a[0], "--session-id",
            "an unreadable state.toml must fall back to fresh, not panic: {a:?}"
        );
    }

    #[test]
    fn sets_memory_redirect_and_ws_env() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into(), mode: None };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        assert_eq!(
            env_of(&cmd, "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"),
            Some(ws.memory_dir().to_string_lossy().to_string())
        );
        assert_eq!(env_of(&cmd, "WS_WORKSPACE"), Some("proj".into()));
        assert_eq!(env_of(&cmd, "WS_DIR"), Some(ws.root.to_string_lossy().to_string()));
        assert_eq!(env_of(&cmd, "WS_ROOT"), Some("/root".into()));
    }

    /// The posture flags must survive alongside the session args — a resume
    /// that silently dropped `-loco` (or kept it when `-sane` was asked for)
    /// is the failure that matters, and it is invisible without launching.
    #[test]
    fn mode_args_are_appended_to_both_fresh_and_resume() {
        use crate::agents::LaunchMode;
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        crate::contract::write_session_id(&ws.state_toml(), "claude", "uuid-xyz").unwrap();

        let resumed = ClaudeAgent
            .launch(
                &ws,
                &LaunchCtx {
                    fresh: false,
                    sessions_root: "/r".into(),
                    mode: Some(LaunchMode::Loco),
                },
            )
            .unwrap();
        assert_eq!(
            args_of(&resumed),
            vec!["--resume", "uuid-xyz", "--dangerously-skip-permissions"]
        );

        let fresh = ClaudeAgent
            .launch(
                &ws,
                &LaunchCtx {
                    fresh: true,
                    sessions_root: "/r".into(),
                    mode: Some(LaunchMode::Sane),
                },
            )
            .unwrap();
        let a = args_of(&fresh);
        assert_eq!(a[0], "--session-id", "the session args still come first: {a:?}");
        assert_eq!(&a[2..], ["--permission-mode", "acceptEdits"]);
    }

    /// No mode means no flags at all — a workspace that has never been told
    /// must launch exactly as it did before the modes existed.
    #[test]
    fn no_mode_adds_no_permission_flags() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let cmd = ClaudeAgent
            .launch(&ws, &LaunchCtx { fresh: true, sessions_root: "/r".into(), mode: None })
            .unwrap();
        let a = args_of(&cmd);
        assert_eq!(a.len(), 2, "only the session args: {a:?}");
        assert!(!a.iter().any(|x| x.contains("permission")), "{a:?}");
    }

    #[test]
    fn binary_override_env() {
        std::env::set_var("WS_CLAUDE_BIN", "/fake/claude");
        let b = ClaudeAgent.binary();
        std::env::remove_var("WS_CLAUDE_BIN");
        assert_eq!(b, "/fake/claude");
    }

    /// `$EDITOR` is a command *line*, split on whitespace by whoever runs it. An
    /// unquoted ws path containing a space breaks ctrl+g and, because the shim
    /// never runs and so never delegates, the user's ordinary editor with it.
    #[test]
    fn the_editor_ws_exports_survives_a_path_with_a_space() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        crate::config::set("rewrite", "true").unwrap();

        let ws = ws_at(&d.path().join("proj"));
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into(), mode: None };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        let editor = env_of(&cmd, "EDITOR").expect("rewrite is on, so $EDITOR is ws's shim");
        std::env::remove_var("XDG_CONFIG_HOME");

        assert!(editor.starts_with('\''), "the path must be quoted: {editor}");
        assert!(editor.ends_with("' internal rewrite"), "{editor}");

        // And the quoting is real quoting: a shell must recover the exact path.
        let spaced = std::path::Path::new("/Applications/My Tools/ws");
        let quoted = crate::hooksetup::shell_quote_path(spaced);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), spaced.to_string_lossy());
    }
}
