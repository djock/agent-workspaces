pub mod claude;
pub mod codex;

use std::path::PathBuf;
use std::process::Command;

use crate::workspace::Workspace;

pub struct LaunchCtx {
    pub fresh: bool,
    pub sessions_root: PathBuf,
    /// The permission posture this launch runs with, or `None` to leave the
    /// agent's own default alone — which is what every launch did before the
    /// modes existed, and what a workspace that has never been told still gets.
    pub mode: Option<LaunchMode>,
}

/// How much the agent is allowed to do without asking.
///
/// Two named postures rather than a passthrough for each agent's flags: the
/// whole point is that one word means the same *intent* on both agents, even
/// though claude and codex spell it with entirely different flags (and codex
/// needs two of them). The translation is `Agent::mode_args`, per agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Everything, without asking: bypassed permissions, no sandbox.
    Loco,
    /// Edits land without asking; anything further still escalates.
    Sane,
}

impl LaunchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchMode::Loco => "loco",
            LaunchMode::Sane => "sane",
        }
    }

    /// Parse the value recorded in `workspace.toml`. Unknown text is `None`,
    /// and the caller says so rather than guessing at a posture — guessing
    /// wrong in the permissive direction is the whole risk here.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "loco" => Some(LaunchMode::Loco),
            "sane" => Some(LaunchMode::Sane),
            _ => None,
        }
    }
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

    /// The flags that put this agent into `mode`.
    ///
    /// **No default**, for the same reason as `tool_matcher` and
    /// `supports_event`: the agents do not agree on how a permission posture is
    /// spelled — claude takes one `--permission-mode` value, codex takes a
    /// sandbox *and* an approval policy — and inheriting someone else's spelling
    /// here would not fail loudly, it would launch an agent in a posture the
    /// user did not choose. A new agent must state its own translation.
    fn mode_args(&self, mode: LaunchMode) -> Vec<String>;

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
