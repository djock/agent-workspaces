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
    /// Whether this workspace has a prior session for this agent (drives resume).
    fn has_prior_session(&self, ws: &Workspace) -> bool;

    /// Where this agent's hooks config lives (a JSON file with a top-level `hooks` object).
    fn hooks_config_path(&self) -> PathBuf;
    /// Where this agent's ws-installed prompts/commands live.
    fn prompts_dir(&self) -> PathBuf;
    /// File name (under `prompts_dir()`) for a given prompt base name (e.g. "summary").
    fn prompt_filename(&self, base: &str) -> String;
    /// A note to surface after install if the agent needs an extra trust/enable step.
    fn hook_trust_note(&self) -> Option<&'static str> {
        None
    }

    /// Build a non-interactive Command that runs `prompt` to completion.
    /// Implementations MUST NOT pass any permission-escalation flag: the drain
    /// runs unattended, and an agent that needed approval should fail, not proceed.
    ///
    /// `out_file` is a per-attempt scratch path under the workspace's
    /// `local_dir()` that the caller has *not* created. Implementations that
    /// have no reliable stdout success signal (codex) MUST have the CLI write
    /// its final result there and check it in `headless_succeeded`; an
    /// implementation with a trustworthy stdout signal (claude) may ignore it.
    fn headless(
        &self,
        ws: &Workspace,
        prompt: &str,
        ctx: &LaunchCtx,
        out_file: &std::path::Path,
    ) -> anyhow::Result<Command>;

    /// Whether a finished headless run counts as success. Unreadable output —
    /// on stdout or, for agents that use it, `out_file` — is always a failure,
    /// never an assumed success. `out_file` is the same path passed to
    /// `headless`.
    fn headless_succeeded(&self, out: &std::process::Output, out_file: &std::path::Path) -> bool;
}

pub fn for_id(id: &str) -> anyhow::Result<Box<dyn Agent>> {
    match id {
        "claude" => Ok(Box::new(claude::ClaudeAgent)),
        "codex" => Ok(Box::new(codex::CodexAgent)),
        other => anyhow::bail!("unknown agent: {other} (ws supports claude and codex)"),
    }
}
