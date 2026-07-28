pub mod claude;
pub mod codex;

use std::path::PathBuf;
use std::process::Command;

use crate::workspace::Workspace;

pub struct LaunchCtx {
    pub fresh: bool,
    pub sessions_root: PathBuf,
}

pub trait Agent {
    fn id(&self) -> &'static str;
    fn binary(&self) -> String;
    fn is_installed(&self) -> bool;
    fn context_file(&self) -> &'static str;
    /// Build the launch Command, deciding fresh vs resume itself and persisting any
    /// per-agent launch state (e.g. Claude's session-id, Codex's "launched" marker).
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> anyhow::Result<Command>;
    /// Where this agent's hooks config lives (a JSON file with a top-level `hooks` object).
    fn hooks_config_path(&self) -> PathBuf;

    /// The `tool_name` regex this agent uses for a given kind of tool.
    ///
    /// Hook matchers are regexes over the payload's `tool_name`, and the agents
    /// do not agree on what tools are called. Deliberately has **no default**:
    /// a shared const carrying Claude's names is exactly what silently disabled
    /// secret redaction on Codex, so every agent must state its own mapping and
    /// a newly added agent cannot inherit one by accident.
    fn tool_matcher(&self, kind: crate::hooksetup::ToolKind) -> &'static str;

    /// Whether this agent fires `event` at all.
    ///
    /// Codex has no `PostToolUseFailure`. Registering a hook on an event the
    /// agent never fires writes an entry that looks installed and can never run —
    /// the same silent-no-op class as the matcher bug above. User hooks are
    /// validated against this, so an unsupported event is reported and skipped
    /// rather than quietly accepted.
    ///
    /// **No default**, for the same reason as `tool_matcher`: a new agent must
    /// state its own event set instead of inheriting a list that happens to
    /// describe someone else.
    fn supports_event(&self, event: &str) -> bool;

    /// Where this agent's ws-installed prompts/commands live.
    fn prompts_dir(&self) -> PathBuf;
    /// File name (under `prompts_dir()`) for a given prompt base name (e.g. "summary").
    fn prompt_filename(&self, base: &str) -> String;
    /// A note to surface after install if the agent needs an extra trust/enable step.
    fn hook_trust_note(&self) -> Option<&'static str> {
        None
    }

}

pub fn for_id(id: &str) -> anyhow::Result<Box<dyn Agent>> {
    match id {
        "claude" => Ok(Box::new(claude::ClaudeAgent)),
        "codex" => Ok(Box::new(codex::CodexAgent)),
        other => anyhow::bail!("unknown agent: {other} (ws supports claude and codex)"),
    }
}
