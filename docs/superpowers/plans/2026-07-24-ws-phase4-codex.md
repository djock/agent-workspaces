# ws Phase 4 (Codex Adapter + Interchange) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make `ws` a true multi-agent tool: a Codex adapter (launch/resume/exec, AGENTS.md, hooks + prompts install), a per-agent-generalized launch and install layer, the interchange flow (`ws <name> --agent codex`, handoff seeding, guard-clear on switch), and `ws -doctor`.

**Architecture:** Codex's hook + context contract is nearly identical to Claude's (verified: hooks live in `~/.codex/hooks.json` with the same `{"hooks":{...}}` shape; SessionStart/UserPromptSubmit inject via `hookSpecificOutput.additionalContext`; Stop returns `{"decision":"block","reason"}`; PreToolUse carries `tool_name`+`tool_input.command`; UserPromptSubmit carries `prompt`). So ws's existing `ws internal` handlers and shim scripts work on Codex UNCHANGED — Phase 4 only generalizes *where* they install. The `Agent` trait grows agent-specific install targets and owns its own fresh/resume launch decision, so `commands::launch` stays agent-agnostic. Codex resume has no `--session-id` pre-seed, so the Codex adapter tracks a per-workspace "launched" marker and resumes with `codex resume --last`.

**Tech Stack:** Rust 2021, existing deps only. Git via system git. Dev: assert_cmd, predicates, tempfile.

## Global Constraints

- **Rust 2021, single crate, binary `ws`.** cargo NOT on PATH — prefix every cargo call with `. "$HOME/.cargo/env";` (Rust 1.97.1). No new dependencies.
- **Verified Codex CLI (0.145.0) facts** (durable ref: `.cs/memory/reference_codex-cli-contract.md`):
  - Fresh: `codex` (cwd = workspace). Resume: `codex resume --last` (cwd-filtered; no `--session-id` pre-seed). Headless: `codex exec`.
  - Context file: `AGENTS.md` (project-level in cwd), managed block.
  - Hooks: `~/.codex/hooks.json` (top-level `hooks` object, SAME JSON shape as Claude's settings.json). Events SessionStart/UserPromptSubmit/PreToolUse/Stop/SessionEnd (+more) match ws's set. Context injection = `hookSpecificOutput.additionalContext`. Stop = `{"decision":"block","reason"}`. UserPromptSubmit input field = `prompt`. → **ws's `hookio`/handlers/shims work on Codex unchanged.**
  - Codex hook TRUST: non-managed command hooks must be trusted (`/hooks` in Codex) before they run. ws installs non-managed; `ws setup` + `ws -doctor` must surface the trust step. Do NOT attempt enterprise `requirements.toml` managed install in this phase.
  - Custom prompts: `~/.codex/prompts/<name>.md` (YAML frontmatter `description`/`argument-hint`), invoked `/prompts:<name>`. ws installs `ws-summary.md` etc → `/prompts:ws-summary`.
  - `is_installed`: `codex --version` (exit 0). `codex doctor` exists.
- **Non-destructive installs:** the Codex hooks.json merge reuses the Phase-2 guard — preserve every foreign hook, bail (never overwrite) on an unparseable file, write atomically.
- **Env gating unchanged:** `ws internal` handlers gate on `WS_WORKSPACE` + `WS_DIR` — set by every ws launch (Claude and Codex).
- **Test seams:** `WS_CODEX_BIN` (mirrors `WS_CLAUDE_BIN`) overrides the codex binary path for a fake shim; `WS_NO_EXEC` keeps the launch flow from `exec`-ing.
- **Preserve all Phase 1–3 tests.** The launch refactor (Task 1) changes the Claude adapter's launch entry point — its unit tests get rewritten, but every launch *integration* test (fake shim) must keep passing.
- **Full suite is source of truth:** `. "$HOME/.cargo/env"; cargo test` all-green before each commit (RUST_TEST_THREADS=1 pinned).

---

## File Structure

```
src/agents/mod.rs      # trait grows: launch(->Result), install targets; LaunchCtx{fresh,handoff,sessions_root}; for_id(codex)
src/agents/claude.rs   # launch owns its uuid/resume decision + state write; install-target methods
src/agents/codex.rs    # NEW: Codex adapter (codex / codex resume --last / codex exec, AGENTS.md, marker)
src/hooksetup.rs       # generalize register into install_for(config_path, ws_bin); codex_hooks_path()
src/prompts.rs         # generalize install to a per-agent target (dir + filename scheme)
src/handoff.rs         # NEW: latest_handoff(ws) helper (find newest .ws/handoffs/*.md)
src/context.rs         # regenerate accepts an optional handoff pointer line
src/commands.rs        # launch: agent-agnostic (mode owned by adapter); switch → record agent + clear guard + handoff seed; setup installs for all installed agents; doctor()
src/cli.rs             # Cmd::Doctor (-doctor)
src/main.rs            # route Doctor; mod codex/handoff
tests/                 # codex adapter unit; interchange integration (fake codex shim); doctor; setup-codex
```

---

### Task 1: Launch refactor — adapter owns fresh/resume, `launch -> Result<Command>`, unified flag model

**Files:** `src/agents/mod.rs`, `src/agents/claude.rs`, `src/commands.rs`, `src/cli.rs`; update `src/agents/claude.rs` + `src/cli.rs` unit tests; preserve `tests/launch.rs`.

**Unified single-dash flag model (user decision):** the launch arm accepts, after the workspace name, both the existing long forms and new single-dash shorthands, so Claude and Codex share ONE command surface:
- `-claude` / `-codex` / `-gemini` → agent shorthand, equivalent to `--agent claude|codex|gemini`
- `-resume` → force resume (explicit form of the default) · `-fresh` / `--fresh` → force a new conversation
- `--agent <x>`, `--force`, `--handoff` keep working.
- `Cmd::Launch` gains `handoff: bool` here (Task 4 consumes it). Precedence if both an agent shorthand and `--agent` appear: last one wins. `-resume` and `-fresh` are mutually exclusive — if both appear, error.
- Examples that must parse identically: `ws proj -codex` == `ws proj --agent codex`; `ws proj -resume -codex`; `ws proj -fresh -claude`.

**Interfaces (new):**
```rust
// agents/mod.rs
pub struct LaunchCtx { pub fresh: bool, pub handoff: bool, pub sessions_root: std::path::PathBuf }
pub trait Agent {
    fn id(&self) -> &'static str;
    fn binary(&self) -> String;
    fn is_installed(&self) -> bool;
    fn context_file(&self) -> &'static str;
    /// Build the launch Command, deciding fresh vs resume itself and persisting any
    /// per-agent launch state (e.g. Claude's session-id, Codex's "launched" marker).
    fn launch(&self, ws: &crate::workspace::Workspace, ctx: &LaunchCtx) -> anyhow::Result<std::process::Command>;
    /// Whether this workspace has a prior session for this agent (drives resume).
    fn has_prior_session(&self, ws: &crate::workspace::Workspace) -> bool;
}
```
(The old `conversation_id` + `LaunchMode` + `{session_id,mode}` LaunchCtx are removed/absorbed.)

**Behavior:** `commands::launch` becomes agent-agnostic:
```
resolve agent id → for_id → is_installed preflight → open_or_create → lock →
regenerate context → clear-guard-on-switch (Task 4 adds this) → cmd = agent.launch(&ws, &ctx)? →
guard.keep() → exec / (WS_NO_EXEC) status
```
ClaudeAgent::launch internally: `if ctx.fresh || !has_prior_session → new uuid + write_session_id + "--session-id <id>"; else "--resume <recorded id>"`. Env identical to Phase 1 (memory redirect, WS_WORKSPACE, WS_DIR, WS_ROOT).

- [ ] **Step 0: Add the unified launch-flag parsing (cli.rs) + tests**

Extend `Cmd::Launch` to `{ name, agent: Option<String>, fresh: bool, force: bool, handoff: bool }` and rewrite the launch arm's flag loop in `src/cli.rs::parse` to accept the shorthands. Add these cli parse tests first (RED):
```rust
    #[test]
    fn agent_shorthand_flags() {
        assert_eq!(p(&["proj", "-codex"]), p(&["proj", "--agent", "codex"]));
        assert_eq!(p(&["proj", "-claude"]), p(&["proj", "--agent", "claude"]));
    }
    #[test]
    fn resume_and_fresh_and_handoff() {
        // -resume is the explicit default (fresh=false); -fresh sets fresh
        match p(&["proj", "-resume", "-codex"]) {
            Cmd::Launch { name, agent, fresh, handoff, .. } => {
                assert_eq!(name, "proj"); assert_eq!(agent.as_deref(), Some("codex"));
                assert!(!fresh); assert!(!handoff);
            }
            _ => panic!(),
        }
        match p(&["proj", "-fresh", "--handoff"]) {
            Cmd::Launch { fresh, handoff, .. } => { assert!(fresh); assert!(handoff); }
            _ => panic!(),
        }
    }
    #[test]
    fn resume_and_fresh_conflict_errors() {
        assert!(parse(vec!["proj".into(), "-resume".into(), "-fresh".into()]).is_err());
    }
```
Then implement the launch arm (replace the current `while let Some(a)` loop):
```rust
        name => {
            let mut agent = None;
            let mut fresh = false;
            let mut resume = false;
            let mut force = false;
            let mut handoff = false;
            let mut it = it; // remaining tokens
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--agent" => agent = it.next(),
                    "-claude" => agent = Some("claude".into()),
                    "-codex" => agent = Some("codex".into()),
                    "-gemini" => agent = Some("gemini".into()),
                    "--fresh" | "-fresh" => fresh = true,
                    "-resume" => resume = true,
                    "--force" => force = true,
                    "--handoff" => handoff = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            if fresh && resume {
                bail!("-fresh and -resume are mutually exclusive");
            }
            Ok(Cmd::Launch { name: name.to_string(), agent, fresh, force, handoff })
        }
```
(`-resume` only asserts the default resume behavior; it exists for symmetry/clarity and to conflict-check against `-fresh`.) Wire `handoff` through `commands::launch` (Task 1 passes it into `LaunchCtx`; Task 4 acts on it). Update `main.rs`'s `Cmd::Launch { name, agent, fresh, force, handoff } => commands::launch(name, agent, fresh, force, handoff)?` and the `commands::launch` signature to take `handoff: bool`.

Run: `. "$HOME/.cargo/env"; cargo test cli` → the three new tests pass (after implementation); existing cli tests still pass.

- [ ] **Step 1: Rewrite the Claude adapter unit tests to the new signature**

Replace the Phase-1 `agents/claude.rs` tests with ones that exercise the new `launch(ws, ctx) -> Result<Command>` (they now need a real on-disk workspace so state.toml can be written):
```rust
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
        let ctx = LaunchCtx { fresh: true, handoff: false, sessions_root: "/root".into() };
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
        let ctx = LaunchCtx { fresh: false, handoff: false, sessions_root: "/root".into() };
        let cmd = ClaudeAgent.launch(&ws, &ctx).unwrap();
        assert_eq!(args_of(&cmd), vec!["--resume", "uuid-xyz"]);
    }

    #[test]
    fn sets_memory_redirect_and_ws_env() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, handoff: false, sessions_root: "/root".into() };
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
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test agents::claude`
Expected: FAIL — trait/signature don't match yet.

- [ ] **Step 3: Update the trait + Claude adapter**

`src/agents/mod.rs` — replace `LaunchMode`/old `LaunchCtx`/`conversation_id` with:
```rust
use std::path::PathBuf;
use std::process::Command;
use crate::workspace::Workspace;

pub struct LaunchCtx {
    pub fresh: bool,
    pub handoff: bool,
    pub sessions_root: PathBuf,
}

pub trait Agent {
    fn id(&self) -> &'static str;
    fn binary(&self) -> String;
    fn is_installed(&self) -> bool;
    fn context_file(&self) -> &'static str;
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> anyhow::Result<Command>;
    fn has_prior_session(&self, ws: &Workspace) -> bool;
}

pub fn for_id(id: &str) -> anyhow::Result<Box<dyn Agent>> {
    match id {
        "claude" => Ok(Box::new(claude::ClaudeAgent)),
        "codex" => Ok(Box::new(codex::CodexAgent)),      // Task 2 adds the module
        "gemini" => anyhow::bail!("agent 'gemini' is not available yet (Phase 9)"),
        other => anyhow::bail!("unknown agent: {other}"),
    }
}
```
(Add `pub mod codex;` — a stub until Task 2. To keep Task 1 compiling standalone, temporarily keep `"codex" =>` bailing "not yet"; Task 2 flips it. Choose ONE: simplest is to have Task 1 leave `"codex" | "gemini" => bail!(...)` and Task 2 replace it. Do that.)

`src/agents/claude.rs` — rewrite `launch` to own the decision + state write:
```rust
impl Agent for ClaudeAgent {
    fn id(&self) -> &'static str { "claude" }
    fn binary(&self) -> String { std::env::var("WS_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()) }
    fn is_installed(&self) -> bool {
        std::process::Command::new(self.binary()).arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }
    fn context_file(&self) -> &'static str { "CLAUDE.local.md" }
    fn has_prior_session(&self, ws: &Workspace) -> bool {
        crate::contract::read_session_id(&ws.state_toml(), "claude").is_some()
    }
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> anyhow::Result<std::process::Command> {
        let mut cmd = std::process::Command::new(self.binary());
        if ctx.fresh || !self.has_prior_session(ws) {
            let id = uuid::Uuid::new_v4().to_string();
            crate::contract::write_session_id(&ws.state_toml(), "claude", &id)?;
            cmd.arg("--session-id").arg(&id);
        } else {
            let id = crate::contract::read_session_id(&ws.state_toml(), "claude").unwrap();
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
```

`src/commands.rs` `launch` — replace the session-id/mode block with agent-owned launch:
```rust
    // (after preflight, open_or_create, lock, context::regenerate)
    let ctx = agents::LaunchCtx { fresh, handoff, sessions_root: config::sessions_root(&cfg) };
    let mut cmd = agent.launch(&ws, &ctx)?;
    guard.keep();
    // …WS_NO_EXEC / exec unchanged…
```
Remove the old `uuid`/`write_session_id`/`LaunchMode` code from `commands::launch`. `handoff` comes from the parsed `--handoff` flag (Task 4 wires the flag; for Task 1 pass `false` if the flag isn't parsed yet — but the Phase-1 `Cmd::Launch` already has `fresh`; add `handoff: bool` to it in Task 4. For Task 1, hardcode `handoff: false`).

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS — new claude unit tests + all `tests/launch.rs` integration tests (first-launch fresh, second resumes, --fresh, unknown agent) still green, because the observable command args/state are unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/agents/mod.rs src/agents/claude.rs src/commands.rs src/cli.rs src/main.rs
git commit -m "refactor: adapters own fresh/resume launch; unified -claude/-codex/-resume/-fresh flags"
```

---

### Task 2: Codex adapter

**Files:** `src/agents/codex.rs` (new), `src/agents/mod.rs` (register in `for_id`); tests in `codex.rs` + `tests/launch.rs` (fake codex shim).

**Interfaces:**
```rust
pub struct CodexAgent;
// Agent impl: id "codex"; binary WS_CODEX_BIN||"codex"; is_installed (`codex --version`);
// context_file "AGENTS.md"; has_prior_session = state.toml [codex].launched == true;
// launch: fresh (ctx.fresh || !prior) → `codex`  + record marker; else `codex resume --last`.
// env: WS_WORKSPACE, WS_DIR, WS_ROOT (no memory redirect). cwd = ws.root.
```

- [ ] **Step 1: Write the failing test**

In `src/agents/codex.rs`:
```rust
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
        let ctx = LaunchCtx { fresh: true, handoff: false, sessions_root: "/root".into() };
        let cmd = CodexAgent.launch(&ws, &ctx).unwrap();
        assert!(args(&cmd).is_empty(), "fresh codex takes no resume args");
        assert!(CodexAgent.has_prior_session(&ws), "marker recorded after fresh launch");
    }

    #[test]
    fn resume_uses_resume_last() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        // simulate a prior launch
        CodexAgent.launch(&ws, &LaunchCtx { fresh: true, handoff: false, sessions_root: "/r".into() }).unwrap();
        let cmd = CodexAgent.launch(&ws, &LaunchCtx { fresh: false, handoff: false, sessions_root: "/r".into() }).unwrap();
        assert_eq!(args(&cmd), vec!["resume", "--last"]);
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
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test agents::codex`
Expected: FAIL — module missing.

- [ ] **Step 3: Write codex.rs**

`src/agents/codex.rs`:
```rust
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

fn record_marker(ws: &Workspace) -> Result<()> {
    let mut t: toml::Table = std::fs::read_to_string(ws.state_toml())
        .ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
    let mut e = toml::Table::new();
    e.insert("launched".into(), toml::Value::Boolean(true));
    t.insert("codex".into(), toml::Value::Table(e));
    if let Some(dir) = ws.state_toml().parent() { std::fs::create_dir_all(dir)?; }
    let tmp = ws.state_toml().with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(&t)?)?;
    std::fs::rename(&tmp, ws.state_toml())?;
    Ok(())
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
```
In `src/agents/mod.rs`, add `pub mod codex;` and flip `for_id`'s `"codex" =>` arm to `Ok(Box::new(codex::CodexAgent))`.

- [ ] **Step 4: Add a fake-codex integration test**

Extend `tests/common/mod.rs` with a `fake_codex()` shim (mirror `fake_claude`, logging argv to a codex-specific log), then in `tests/launch.rs`:
```rust
#[test]
fn launch_with_agent_codex_uses_fake_codex() {
    let env = Env::new();
    let shim = env.fake_codex();
    env.cmd()
        .env("WS_CODEX_BIN", &shim).env("WS_NO_EXEC", "1")
        .args(["cxproj", "--agent", "codex"])
        .assert().success();
    // fresh launch → no resume args logged; workspace scaffolded with AGENTS.md
    let root = env.root.join("cxproj");
    assert!(root.join("AGENTS.md").is_file(), "codex context file generated");
    // second launch resumes
    env.cmd()
        .env("WS_CODEX_BIN", &shim).env("WS_NO_EXEC", "1")
        .args(["cxproj", "--agent", "codex"])
        .assert().success();
    assert!(env.codex_argv_log().contains("resume --last"));
}
```
(`fake_codex`/`codex_argv_log` mirror the existing `fake_claude`/`argv_log`. The AGENTS.md assertion works because `context::regenerate` writes `agent.context_file()`; confirm `commands::launch` regenerates the resolved agent's context file — it does.)

- [ ] **Step 5: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS — codex unit tests + the interchange launch test + the whole suite (the Phase-1 `unknown_agent_errors_clearly` test used `--agent codex` expecting "Phase 1 is Claude-only"; UPDATE that test: `--agent codex` now succeeds via the fake shim, or point it at `gemini` for the not-available error. Change it to assert `--agent gemini` fails with "not available yet (Phase 9)").

- [ ] **Step 6: Commit**

```bash
git add src/agents/codex.rs src/agents/mod.rs tests/common/mod.rs tests/launch.rs
git commit -m "feat: Codex adapter (codex / codex resume --last, AGENTS.md, launched marker)"
```

---

### Task 3: Per-agent hook + prompt install

**Files:** `src/agents/mod.rs` (install-target trait methods), `src/agents/claude.rs`, `src/agents/codex.rs`, `src/hooksetup.rs`, `src/prompts.rs`, `src/commands.rs` (`setup`); tests in `tests/setup.rs`.

**What to change:** generalize the Phase-2/3 Claude-only install so it targets each installed agent.
- Add to the trait: `fn hooks_config_path(&self) -> std::path::PathBuf;` (Claude → `~/.claude/settings.json`; Codex → `~/.codex/hooks.json`), `fn prompts_dir(&self) -> std::path::PathBuf;` (Claude → `~/.claude/commands/ws`; Codex → `~/.codex/prompts`), `fn prompt_filename(&self, base: &str) -> String;` (Claude → `"{base}.md"` under the `ws` subdir → `/ws:{base}`; Codex → `"ws-{base}.md"` → `/prompts:ws-{base}`), `fn hook_trust_note(&self) -> Option<&'static str>;` (Claude → None; Codex → Some("Run `/hooks` in Codex to trust the ws hooks before they take effect.")).
- `hooksetup`: rename/add `install_hooks_for(config_path: &Path, ws_bin: &Path) -> Result<usize>` that materializes the shared shims (unchanged `hooks_dir()`) and runs the existing non-destructive `register` merge against `config_path` (the merge already only needs a JSON file with a top-level `hooks` object — works for both settings.json and hooks.json). Add `codex_hooks_path()`.
- `prompts`: `install_for(dir: &Path, filename_of: impl Fn(&str)->String) -> Result<usize>`.
- `commands::setup`: for each agent in `["claude","codex"]` that `is_installed()`, install hooks + prompts to that agent's targets; print a per-agent summary incl. the trust note when present. (Claude statusline registration from Phase 3 stays Claude-only.)

- [ ] **Step 1: Write the failing test**

Add to `tests/setup.rs`:
```rust
#[test]
fn setup_installs_codex_hooks_and_prompts_when_codex_present() {
    let env = Env::new();
    // Make codex "installed" by pointing WS_CODEX_BIN at a shim that exits 0 on --version.
    let shim = env.fake_codex();
    env.cmd().env("WS_CODEX_BIN", &shim).arg("setup").assert().success()
        .stdout(predicates::str::contains("codex"))
        .stdout(predicates::str::contains("/hooks")); // trust note surfaced

    // ~/.codex/hooks.json got the ws SessionStart hook
    let hooks = env.home.path().join(".codex/hooks.json");
    let body = std::fs::read_to_string(&hooks).unwrap();
    assert!(body.contains("session-start.sh"));
    // namespaced codex prompt installed
    assert!(env.home.path().join(".codex/prompts/ws-summary.md").is_file());
}
```
(Ensure the `fake_codex` shim exits 0 for `--version` so `is_installed()` is true, mirroring `fake_claude`.)

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test setup setup_installs_codex`
Expected: FAIL — setup is Claude-only.

- [ ] **Step 3: Implement**

Add the four trait methods (with default impls for `hook_trust_note` returning `None`). Implement per agent. In `hooksetup`, add `codex_hooks_path()` (`home/.codex/hooks.json`) and refactor `install(ws_bin)` into `install_hooks_for(config_path, ws_bin)` (materialize shims once + `register_settings(config_path, &hooks_dir())`); keep a thin `install(ws_bin)` calling it with `claude_settings_path()` for back-compat. In `prompts`, add `install_for(dir, filename_of)` and keep `install()` delegating with the Claude scheme. In `commands::setup`:
```rust
pub fn setup() -> Result<()> {
    let ws_bin = std::env::current_exe()?;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        if !agent.is_installed() { continue; }
        let nh = crate::hooksetup::install_hooks_for(&agent.hooks_config_path(), &ws_bin)?;
        let np = crate::prompts::install_for(&agent.prompts_dir(), |b| agent.prompt_filename(b))?;
        println!("ws setup [{}]: {nh} hook(s) + {np} prompt(s)", agent.id());
        if let Some(note) = agent.hook_trust_note() { println!("  note: {note}"); }
    }
    // Claude statusline registration (Phase 3) stays claude-only:
    crate::hooksetup::register_statuslines(&ws_bin)?;
    Ok(())
}
```

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS — codex install test + the Phase-2/3 Claude setup tests (they still pass; `for_id("claude").is_installed()` — in tests, is claude "installed"? The Env tests don't set WS_CLAUDE_BIN for `ws setup`, so real `claude` on PATH decides. To keep setup tests deterministic, gate the loop so a missing agent is simply skipped, and assert on the agent that IS present. The Phase-2/3 tests assert Claude artifacts — ensure `claude` is on PATH in the dev/CI env, OR set WS_CLAUDE_BIN to a shim in those tests. SAFEST: update the existing setup tests to set `WS_CLAUDE_BIN` to a `fake_claude` shim so Claude is deterministically "installed").

- [ ] **Step 5: Commit**

```bash
git add src/agents/mod.rs src/agents/claude.rs src/agents/codex.rs src/hooksetup.rs src/prompts.rs src/commands.rs tests/setup.rs
git commit -m "feat: per-agent hook + prompt install (Codex hooks.json + /prompts:ws-*)"
```

---

### Task 4: Handoff seeding + interchange (switch)

**Files:** `src/handoff.rs` (new), `src/context.rs`, `src/cli.rs` (add `handoff` to `Cmd::Launch`), `src/commands.rs` (`launch`), `src/internal.rs` (guard already cleared on reset — switch clearing is here); tests in `tests/launch.rs`.

**What to change:**
1. `src/handoff.rs`: `pub fn latest_handoff(ws: &Workspace) -> Option<PathBuf>` — newest `.ws/handoffs/*.md` by mtime.
2. `src/context.rs`: `regenerate` gains an optional `handoff_hint: Option<&str>` — when `Some(path)`, the managed block prepends a line: `START HERE: read the handoff .ws/handoffs/<file> first, then continue.` (agent-agnostic; both AGENTS.md and CLAUDE.local.md).
3. `src/cli.rs`: the `handoff: bool` field + `--handoff`/`-fresh`/`-resume`/`-claude`/`-codex` parsing were already added in Task 1 — nothing to do here.
4. `src/commands.rs` `launch`:
   - Determine `switching` = the resolved `agent_id` differs from the workspace's recorded `default_agent` (read from workspace.toml; None on first launch = not switching).
   - On `--handoff` OR `switching`: pass `latest_handoff(&ws)` as the context `handoff_hint` when regenerating.
   - On `switching`: clear the limit guard (`std::fs::remove_file(ws.limit_guard())`), and record the new `default_agent` in workspace.toml.
   - Record a timeline `agent-switch` event on switch.

- [ ] **Step 1: Write the failing test**

Add to `tests/launch.rs`:
```rust
#[test]
fn switching_agents_clears_guard_and_records_default() {
    let env = Env::new();
    let claude = env.fake_claude();
    let codex = env.fake_codex();

    // first launch with claude (default recorded = claude)
    env.cmd().env("WS_CLAUDE_BIN", &claude).env("WS_NO_EXEC","1")
        .arg("switchproj").assert().success();
    let root = env.root.join("switchproj");
    // plant a limit guard as if a threshold had been crossed
    std::fs::create_dir_all(root.join(".ws/local")).unwrap();
    std::fs::write(root.join(".ws/local/limit-guard"), "x").unwrap();

    // switch to codex
    env.cmd().env("WS_CODEX_BIN", &codex).env("WS_NO_EXEC","1")
        .args(["switchproj","--agent","codex"]).assert().success();

    // guard cleared on switch; default_agent now codex; AGENTS.md generated
    assert!(!root.join(".ws/local/limit-guard").exists(), "switch clears the limit guard");
    let wt = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
    assert!(wt.contains("default_agent = \"codex\""));
    assert!(root.join("AGENTS.md").is_file());
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test launch switching_agents`
Expected: FAIL — no switch handling / --handoff flag / default recording yet.

- [ ] **Step 3: Implement**

`src/handoff.rs`:
```rust
use std::path::{Path, PathBuf};
use crate::workspace::Workspace;

pub fn latest_handoff(ws: &Workspace) -> Option<PathBuf> {
    let dir = ws.ws_dir().join("handoffs");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") { continue; }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            if newest.as_ref().map_or(true, |(t, _)| m > *t) { newest = Some((m, p)); }
        }
    }
    newest.map(|(_, p)| p)
}
```
`src/context.rs` — change `regenerate` to accept `handoff_hint: Option<&Path>` and, when Some, prepend a `START HERE:` line inside the rendered block (before the template body). Update all callers (`commands::launch`) accordingly; a `None` keeps current behavior (so existing context tests pass — pass `None` there, or add an overload). Simplest: add a new `regenerate_with_handoff(path, name, Option<&Path>)` and have `regenerate` delegate with `None`, so existing tests are untouched.

`src/cli.rs` — add `handoff: bool` to `Cmd::Launch { .. }`; parse `--handoff` in the launch arm; default false.

`src/commands.rs` `launch` — add near the top (after resolving `agent_id`, before/around context regen):
```rust
    let recorded_default = workspace_toml_str(&workspace::resolve(&name, &cfg), "default_agent");
    let switching = recorded_default.as_deref().map_or(false, |d| d != agent_id);
    // …open_or_create, lock…
    let hint = if handoff || switching { crate::handoff::latest_handoff(&ws) } else { None };
    context::regenerate_with_handoff(&ws.root.join(agent.context_file()), &ws.name, hint.as_deref())?;
    if switching {
        let _ = std::fs::remove_file(ws.limit_guard());
        set_workspace_default_agent(&ws, agent.id())?;   // small helper: rewrite workspace.toml default_agent
        let _ = crate::timeline::record(&ws.timeline(), "agent-switch",
            &crate::actors::actor_slug(), serde_json::json!({ "to": agent.id() }));
    }
```
Add `set_workspace_default_agent(ws, id)` — read workspace.toml as a `toml::Table`, set `default_agent`, atomic write. Wire the parsed `handoff` into `LaunchCtx` too.

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/handoff.rs src/context.rs src/cli.rs src/commands.rs src/main.rs
git commit -m "feat: agent interchange — handoff seeding, switch clears guard + records default"
```

---

### Task 5: `ws -doctor`

**Files:** `src/cli.rs` (`Cmd::Doctor`), `src/commands.rs` (`doctor`), `src/main.rs` (route); tests in `tests/doctor.rs`.

**Interfaces:** `commands::doctor() -> anyhow::Result<()>` — prints a checklist and exits non-zero if any hard check fails.

**Checks (best-effort, each a ✓/✗ line):**
- For each agent (claude, codex): installed? (version string if yes); its context-file template present in any current workspace is out of scope — instead check the agent's hook config registration: Claude `~/.claude/settings.json` has a ws hook (command under the ws hooks dir) + statusline registered; Codex `~/.codex/hooks.json` has a ws hook, plus print the trust note.
- ws hooks dir exists with the shim scripts.
- Contract: nothing global to check here beyond the above.
- Exit non-zero if a REQUIRED check fails: no agent installed at all, or the ws hooks dir/shims missing after a setup. Soft items (Codex not installed, statusline not registered) are warnings, not failures.

- [ ] **Step 1: Write the failing test**

`tests/doctor.rs`:
```rust
mod common;
use common::Env;

#[test]
fn doctor_reports_agents_and_hook_state() {
    let env = Env::new();
    let claude = env.fake_claude();
    // run setup so there's something to check
    env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();

    env.cmd().env("WS_CLAUDE_BIN", &claude)
        .arg("-doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("claude"))
        .stdout(predicates::str::contains("hooks"));
}

#[test]
fn doctor_flags_no_agents_installed() {
    let env = Env::new();
    // point both agent bins at a nonexistent path → neither installed
    env.cmd()
        .env("WS_CLAUDE_BIN", "/nope/claude")
        .env("WS_CODEX_BIN", "/nope/codex")
        .arg("-doctor")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no agent").or(predicates::str::contains("not installed")));
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test doctor`
Expected: FAIL — `-doctor` unhandled.

- [ ] **Step 3: Implement**

`src/cli.rs` — add `"-doctor" => Ok(Cmd::Doctor)` in the leading-dash section + `Doctor,` variant. `src/main.rs` — `Cmd::Doctor => commands::doctor()?`.
`src/commands.rs`:
```rust
pub fn doctor() -> Result<()> {
    let mut any_agent = false;
    let mut hard_fail = false;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        if agent.is_installed() {
            any_agent = true;
            println!("✓ {id}: installed ({})", agent_version(&agent.binary()));
            let cfg_path = agent.hooks_config_path();
            let has_hook = std::fs::read_to_string(&cfg_path).ok()
                .map(|s| s.contains(&crate::hooksetup::hooks_dir().to_string_lossy().to_string()))
                .unwrap_or(false);
            println!("  {} ws hooks registered in {}", if has_hook { "✓" } else { "…" }, cfg_path.display());
            if let Some(note) = agent.hook_trust_note() { println!("  note: {note}"); }
        } else {
            println!("… {id}: not installed");
        }
    }
    // shims present?
    let shim = crate::hooksetup::hooks_dir().join("session-start.sh");
    if shim.exists() { println!("✓ ws hook scripts present"); }
    else { println!("… ws hook scripts missing — run `ws setup`"); }

    if !any_agent {
        eprintln!("ws: no agent installed (need claude or codex on PATH)");
        hard_fail = true;
    }
    if hard_fail { std::process::exit(1); }
    Ok(())
}

fn agent_version(bin: &str) -> String {
    std::process::Command::new(bin).arg("--version").output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string()).unwrap_or_default()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test doctor; cargo test`
Expected: PASS.

- [ ] **Step 5: Manual smoke**

```bash
. "$HOME/.cargo/env"; cargo build --release
HOME=$(mktemp -d) XDG_CONFIG_HOME=$(mktemp -d) ./target/release/ws -doctor
```
Expected: reports claude + codex install state and hook registration (both real agents ARE installed on this machine).

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/commands.rs src/main.rs tests/doctor.rs
git commit -m "feat: ws -doctor (agent install + hook registration checks)"
```

---

## Self-Review

**Spec coverage (§17.4):**
- Codex adapter launch/resume — Tasks 1, 2 ✓ (`codex` / `codex resume --last`, marker-driven)
- AGENTS.md — Task 2 (`context_file`) + Task 4 (handoff seeding) ✓
- hooks.json install — Task 3 ✓ (reuses the agent-compatible handlers/shims + non-destructive merge)
- prompt install — Task 3 ✓ (`~/.codex/prompts/ws-*.md` → `/prompts:ws-*`)
- interchange (`--agent`, handoff seeding) — Tasks 2, 4 ✓
- doctor for both agents — Task 5 ✓

**Corrected from initial mis-verification:** Codex DOES have full hooks + custom prompts (verified via `codex features list` + docs), so Phase 4 installs the real behaviors — not the context-file-only fallback I wrongly scoped. Codex-specific realities honored: no `--session-id` (marker + `resume --last`); hook TRUST step surfaced by `setup` + `doctor`; prompts at `~/.codex/prompts` invoked `/prompts:ws-*`.

**Deferred (correctly out of Phase 4):** Codex headless queue drain (`codex exec`) wiring into the queue is Phase 8; Codex machine-readable limits (display-only, spec §18); Gemini (Phase 9); managed/`requirements.toml` pre-trusted hook install (enterprise, out of scope); the limit-aware statusline pin to a switch call-to-action (needs the queue/notify work).

**Type consistency:** the new `LaunchCtx{fresh,handoff,sessions_root}` + `launch(ws,ctx)->Result<Command>` + `has_prior_session` are implemented by both `ClaudeAgent` and `CodexAgent` and consumed by `commands::launch`. New trait methods `hooks_config_path`/`prompts_dir`/`prompt_filename`/`hook_trust_note` are implemented by both. `hooksetup::install_hooks_for(config_path, ws_bin)` and `prompts::install_for(dir, filename_of)` are the generalized engines; the Phase-2/3 `install()`/`register_statuslines` remain for Claude specifics. `handoff::latest_handoff` + `context::regenerate_with_handoff` are consumed only by `commands::launch`.
