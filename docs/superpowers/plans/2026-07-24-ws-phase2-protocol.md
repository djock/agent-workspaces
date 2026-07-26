# ws Phase 2 (Protocol) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a `ws`-launched Claude session its protocol layer: a timeline event log, session-start context injection, first-prompt→README objective capture, a notebook-update reminder, a Bash audit log, the `/ws:summary /ws:wrap /ws:sweep /ws:rotate` prompt-commands, and a `ws setup` that installs it all into Claude Code.

**Architecture:** Hooks must not depend on `jq`/python (zero-dep promise), so each Claude hook is a 3-line POSIX shim that pipes stdin to `ws internal <handler>`; `ws` itself parses the hook JSON and emits the response JSON via `serde_json`. `ws setup` materializes the shims to `~/.config/ws/hooks/` (referencing the ws binary by absolute path), registers them in `~/.claude/settings.json` with a non-destructive additive merge (ws entries are identified by their command path, so cs's and the user's hooks survive), and installs the prompt files under `~/.claude/commands/ws/` (namespaced as `/ws:*` so they never collide with cs's `/summary` etc.). Timeline events are appended as JSON lines to `.ws/timeline.jsonl`.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `toml`, `anyhow`, `dirs`. All hook/prompt assets embedded via `include_str!`. Dev: `assert_cmd`, `predicates`, `tempfile`. Builds on the Phase 1 crate (config, registry, contract, actors, workspace, lock, context, agents, term modules already exist).

## Global Constraints

- **Rust 2021, single crate, binary `ws`.** cargo is NOT on the default PATH — prefix every cargo call with `. "$HOME/.cargo/env";` (Rust 1.97.1 via rustup).
- **Zero runtime deps beyond git + agents.** Hook scripts are POSIX `sh` with **no jq/python** — all JSON handling is done by calling the `ws` binary (`ws internal <handler>`). `serde_json` is already a dependency (Phase 1).
- **Hooks must never break the agent.** Every `ws internal` handler is best-effort: on any error it emits neutral output and exits 0. `main` must route `Cmd::Internal` so it can never exit non-zero.
- **Claude hook I/O contract (verified against Claude Code 2.1.218):**
  - `settings.json` shape: `{"hooks":{"<Event>":[{"matcher"?:"<M>","hooks":[{"type":"command","command":"<path>","timeout":N}]}]}}`.
  - Hook **input** arrives on stdin as JSON. Fields used: SessionStart `{session_id, cwd, source, agent_id?}` (`source` ∈ startup|resume|clear|compact); UserPromptSubmit `{prompt}`; PreToolUse `{tool_name, tool_input:{command}}`; SessionEnd `{reason?}`.
  - Context-injection **output** (SessionStart / UserPromptSubmit): stdout JSON `{"hookSpecificOutput":{"hookEventName":"<Event>","additionalContext":"<string>"}}`.
  - Stop **output**: `{"decision":"approve"}` (let the turn end) or `{"decision":"block","reason":"<string>"}` (send the agent back with the reason).
  - PreToolUse audit / SessionEnd: no stdout needed → exit 0.
- **Command namespacing (user decision):** install prompt files to `~/.claude/commands/ws/*.md` → invoked `/ws:summary`, `/ws:wrap`, `/ws:sweep`, `/ws:rotate`. Never write to `~/.claude/commands/*.md` (that is cs's space).
- **Non-destructive settings merge:** ws-managed hook entries are exactly those whose `command` path is under the ws hooks dir. `ws setup` removes only those before re-adding, preserving every other hook (cs, user).
- **Env gating:** the protocol is active only inside a ws launch. Handlers no-op unless `WS_WORKSPACE` is set; they locate the workspace via `WS_DIR` (exported by the launch flow) falling back to the current directory.
- **Paths:** hooks dir = `<ws_config_dir>/hooks` (co-located with config.toml/registry.toml, honoring XDG_CONFIG_HOME — reuse `config::ws_config_dir()`). Claude settings = `<home>/.claude/settings.json`; commands = `<home>/.claude/commands/ws/`. Use `dirs::home_dir()` (respects `$HOME`, so tests isolate via HOME override).
- **Test isolation:** `.cargo/config.toml` already pins `RUST_TEST_THREADS=1`; the full suite is the source of truth (`. "$HOME/.cargo/env"; cargo test`). Integration tests use the `Env` helper (isolated HOME/XDG_CONFIG_HOME/WS_ROOT).

---

## File Structure

```
src/
├── timeline.rs      # append JSON-line events to .ws/timeline.jsonl
├── hookio.rs        # HookInput (stdin JSON) + response-JSON emit helpers
├── readme.rs        # first-prompt → README ## Objective capture
├── internal.rs      # `ws internal <handler>` dispatch + all hook handlers + gating
├── hooksetup.rs     # HOOKS table, shim render, settings.json non-destructive merge
├── prompts.rs       # PROMPTS table (embedded), install to ~/.claude/commands/ws/
├── assets/prompts/{summary,wrap,sweep,rotate}.md   # embedded prompt bodies
├── commands.rs      # (modify) add `setup()`; record timeline "created" is in contract
├── contract.rs      # (modify) record timeline "created" on init
├── agents/claude.rs # (modify) export WS_DIR env on launch
├── workspace.rs     # (modify) add readme()/notebook_dir()/timeline()/session_log() path helpers
├── cli.rs           # (modify) add Cmd::Setup, Cmd::Internal(Vec<String>)
└── main.rs          # (modify) route Setup + Internal; declare new mods
tests/
├── internal.rs      # end-to-end `ws internal *` via adopt + stdin
└── setup.rs         # `ws setup` installs hooks (settings.json) + prompts, idempotently
```

Each module has one responsibility. `internal.rs` is the only place hook handlers live; `hooksetup.rs`/`prompts.rs` are install-only.

---

### Task 1: Timeline module + Workspace path helpers + "created" event

**Files:**
- Create: `src/timeline.rs`
- Modify: `src/workspace.rs` (add path helpers)
- Modify: `src/contract.rs` (record "created" in `init`)
- Modify: `src/main.rs` (`mod timeline;`)
- Test: unit tests in `timeline.rs`; extend nothing else

**Interfaces:**
- Consumes: `serde_json`, `crate::now_iso`, `crate::actors::actor_slug`.
- Produces:
  ```rust
  // timeline.rs
  /// Append one event as a JSON line to `timeline_path`. The line is an object
  /// with ts (ISO-8601 UTC), kind, actor, merged with the fields of `extra`
  /// (which must be a JSON object or Null). Best-effort creation of the parent dir.
  pub fn record(timeline_path: &std::path::Path, kind: &str, actor: &str, extra: serde_json::Value) -> anyhow::Result<()>;

  // workspace.rs (new methods on Workspace)
  pub fn readme(&self) -> PathBuf;         // ws_dir()/README.md
  pub fn notebook_dir(&self) -> PathBuf;   // ws_dir()/notebook
  pub fn timeline(&self) -> PathBuf;       // ws_dir()/timeline.jsonl
  pub fn session_log(&self) -> PathBuf;    // local_dir()/log/session.log
  ```

- [ ] **Step 1: Write the failing test**

In `src/timeline.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn appends_json_lines() {
        let d = TempDir::new().unwrap();
        let tl = d.path().join("timeline.jsonl");
        record(&tl, "created", "alice", serde_json::json!({"agent":"claude"})).unwrap();
        record(&tl, "opened", "alice", serde_json::Value::Null).unwrap();

        let body = std::fs::read_to_string(&tl).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["kind"], "created");
        assert_eq!(first["actor"], "alice");
        assert_eq!(first["agent"], "claude");
        assert!(first["ts"].as_str().unwrap().ends_with('Z'));

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["kind"], "opened");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test timeline`
Expected: FAIL — module/function undefined.

- [ ] **Step 3: Write timeline.rs**

`src/timeline.rs`:
```rust
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

pub fn record(timeline_path: &Path, kind: &str, actor: &str, extra: Value) -> Result<()> {
    let mut event = json!({
        "ts": crate::now_iso(),
        "kind": kind,
        "actor": actor,
    });
    if let Value::Object(extra_map) = extra {
        if let Value::Object(base) = &mut event {
            for (k, v) in extra_map {
                base.insert(k, v);
            }
        }
    }
    if let Some(dir) = timeline_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(&event)?;
    line.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(timeline_path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}
```

- [ ] **Step 4: Add Workspace path helpers**

In `src/workspace.rs`, inside `impl Workspace`, add (place after the existing `workspace_toml` method):
```rust
    pub fn readme(&self) -> PathBuf {
        self.ws_dir().join("README.md")
    }
    pub fn notebook_dir(&self) -> PathBuf {
        self.ws_dir().join("notebook")
    }
    pub fn timeline(&self) -> PathBuf {
        self.ws_dir().join("timeline.jsonl")
    }
    pub fn session_log(&self) -> PathBuf {
        self.local_dir().join("log").join("session.log")
    }
```

- [ ] **Step 5: Record "created" in contract::init**

In `src/contract.rs`, at the END of `init` (just before `Ok(())`, after `registry::register`), add a best-effort timeline event so both create and adopt log it:
```rust
    // Best-effort: record workspace creation on the timeline.
    let _ = crate::timeline::record(
        &root.join(".ws").join("timeline.jsonl"),
        "created",
        &actor,
        serde_json::json!({ "agent": agent }),
    );
```
(`actor` and `agent` are already in scope in `init`.)

Add `mod timeline;` to `src/main.rs`.

- [ ] **Step 6: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS. The Task-4 (Phase 1) `init_creates_layout` test still passes and now a `timeline.jsonl` is also written (it doesn't assert against it, so no change needed).

- [ ] **Step 7: Commit**

```bash
git add src/timeline.rs src/workspace.rs src/contract.rs src/main.rs
git commit -m "feat: timeline event log + workspace path helpers + created event"
```

---

### Task 2: Hook I/O module

**Files:**
- Create: `src/hookio.rs`
- Modify: `src/main.rs` (`mod hookio;`)
- Test: unit tests in `hookio.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Default, serde::Deserialize)]
  pub struct HookInput {
      #[serde(default)] pub session_id: String,
      #[serde(default)] pub cwd: String,
      #[serde(default)] pub source: String,
      #[serde(default)] pub agent_id: Option<String>,
      #[serde(default)] pub prompt: String,
      #[serde(default)] pub tool_name: String,
      #[serde(default)] pub tool_input: ToolInput,
      #[serde(default)] pub reason: String,
  }
  #[derive(Debug, Default, serde::Deserialize)]
  pub struct ToolInput { #[serde(default)] pub command: String }

  /// Parse a hook JSON payload; returns HookInput::default() on any error.
  pub fn parse(raw: &str) -> HookInput;
  /// Read all of stdin and parse it.
  pub fn read_stdin() -> HookInput;
  /// The SessionStart/UserPromptSubmit additionalContext response JSON string.
  pub fn additional_context(event: &str, context: &str) -> String;
  /// Stop-hook responses.
  pub fn decision_approve() -> String;
  pub fn decision_block(reason: &str) -> String;
  ```

- [ ] **Step 1: Write the failing test**

In `src/hookio.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_fields_and_defaults() {
        let h = parse(r#"{"source":"startup","cwd":"/x","tool_name":"Bash","tool_input":{"command":"ls -la"}}"#);
        assert_eq!(h.source, "startup");
        assert_eq!(h.cwd, "/x");
        assert_eq!(h.tool_name, "Bash");
        assert_eq!(h.tool_input.command, "ls -la");
        assert_eq!(h.prompt, ""); // default
        assert!(h.agent_id.is_none());
    }

    #[test]
    fn parse_garbage_is_default() {
        let h = parse("not json");
        assert_eq!(h.source, "");
        assert_eq!(h.prompt, "");
    }

    #[test]
    fn additional_context_shape_and_escaping() {
        let s = additional_context("SessionStart", "line1\n\"quoted\"");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "line1\n\"quoted\"");
    }

    #[test]
    fn decisions() {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decision_approve()).unwrap()["decision"],
            "approve"
        );
        let b: serde_json::Value = serde_json::from_str(&decision_block("do X")).unwrap();
        assert_eq!(b["decision"], "block");
        assert_eq!(b["reason"], "do X");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test hookio`
Expected: FAIL — module undefined.

- [ ] **Step 3: Write hookio.rs**

`src/hookio.rs`:
```rust
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: ToolInput,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ToolInput {
    #[serde(default)]
    pub command: String,
}

pub fn parse(raw: &str) -> HookInput {
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn read_stdin() -> HookInput {
    match std::io::read_to_string(std::io::stdin()) {
        Ok(s) => parse(&s),
        Err(_) => HookInput::default(),
    }
}

pub fn additional_context(event: &str, context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
    .to_string()
}

pub fn decision_approve() -> String {
    json!({ "decision": "approve" }).to_string()
}

pub fn decision_block(reason: &str) -> String {
    json!({ "decision": "block", "reason": reason }).to_string()
}
```

Add `mod hookio;` to `src/main.rs`.

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test hookio`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hookio.rs src/main.rs
git commit -m "feat: hook I/O (stdin parse + response JSON emit)"
```

---

### Task 3: README objective capture

**Files:**
- Create: `src/readme.rs`
- Modify: `src/main.rs` (`mod readme;`)
- Test: unit tests in `readme.rs`

**Interfaces:**
- Produces:
  ```rust
  /// Replace the placeholder line inside the `## Objective` section with `objective`,
  /// but only while it is still a placeholder (a whole line that is `_(...)_` or
  /// `[...]`). First real prompt wins; a real objective is never overwritten.
  /// Returns Ok(true) if it wrote a change, Ok(false) if nothing to do.
  pub fn capture_objective(readme_path: &std::path::Path, objective: &str) -> anyhow::Result<bool>;
  ```
- Note: `objective` is trimmed to its first line and capped at 200 chars before insertion.

- [ ] **Step 1: Write the failing test**

In `src/readme.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TEMPLATE: &str = "# proj\n\n## Objective\n\n_(captured from the first prompt)_\n\n## Outcome\n\n";

    #[test]
    fn replaces_placeholder_only_within_objective() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, TEMPLATE).unwrap();

        let wrote = capture_objective(&f, "Build the thing\nsecond line ignored").unwrap();
        assert!(wrote);
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("Build the thing"));
        assert!(!s.contains("_(captured from the first prompt)_"));
        // Outcome section placeholder-free area untouched, headings intact
        assert!(s.contains("## Objective"));
        assert!(s.contains("## Outcome"));
        // only first line used
        assert!(!s.contains("second line ignored"));
    }

    #[test]
    fn does_not_overwrite_real_objective() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, "# p\n\n## Objective\n\nAlready written by hand.\n\n## Outcome\n").unwrap();

        let wrote = capture_objective(&f, "New prompt").unwrap();
        assert!(!wrote);
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("Already written by hand."));
        assert!(!s.contains("New prompt"));
    }

    #[test]
    fn bracket_placeholder_also_matches() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, "# p\n\n## Objective\n\n[To be filled]\n\n## Outcome\n").unwrap();
        assert!(capture_objective(&f, "Real goal").unwrap());
        assert!(std::fs::read_to_string(&f).unwrap().contains("Real goal"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test readme`
Expected: FAIL — module undefined.

- [ ] **Step 3: Write readme.rs**

`src/readme.rs`:
```rust
use anyhow::Result;
use std::path::Path;

fn is_placeholder(line: &str) -> bool {
    let t = line.trim();
    (t.starts_with("_(") && t.ends_with(")_")) || (t.starts_with('[') && t.ends_with(']'))
}

pub fn capture_objective(readme_path: &Path, objective: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(readme_path) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };
    let value = objective.lines().next().unwrap_or("").trim();
    let value: String = value.chars().take(200).collect();
    if value.is_empty() {
        return Ok(false);
    }

    let mut out = String::with_capacity(text.len() + value.len());
    let mut in_objective = false;
    let mut replaced = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            in_objective = line.starts_with("## Objective");
        }
        if in_objective && !replaced && is_placeholder(line) {
            out.push_str(&value);
            out.push('\n');
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    if replaced {
        std::fs::write(readme_path, out)?;
    }
    Ok(replaced)
}
```
Add `mod readme;` to `src/main.rs`.

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test readme`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/readme.rs src/main.rs
git commit -m "feat: first-prompt README objective capture"
```

---

### Task 4: `ws internal` dispatch + gating + CLI wiring

**Files:**
- Create: `src/internal.rs`
- Modify: `src/cli.rs` (add `Cmd::Setup`, `Cmd::Internal(Vec<String>)`; parse "setup"/"internal")
- Modify: `src/main.rs` (`mod internal;`; route the two new variants)
- Test: unit tests in `cli.rs` (parse); a small integration test in `tests/internal.rs`

**Interfaces:**
- Consumes: `hookio`, `workspace::Workspace`.
- Produces:
  ```rust
  // internal.rs
  /// Best-effort dispatch for `ws internal <handler> [args]`. NEVER returns Err
  /// in a way that breaks the agent: unknown/failed handlers still exit cleanly.
  pub fn run(args: Vec<String>) -> anyhow::Result<()>;
  /// The workspace the hook is running inside, or None when not in a ws launch.
  pub fn current_ws() -> Option<Workspace>;   // WS_WORKSPACE + (WS_DIR | cwd)

  // cli.rs
  pub enum Cmd { /* …existing… */ Setup, Internal(Vec<String>) }
  ```
- This task wires dispatch with only a `hook-payload` handler implemented; Tasks 5–8 add the real handlers.

- [ ] **Step 1: Write the failing tests**

Add to `src/cli.rs` `#[cfg(test)] mod tests`:
```rust
    #[test]
    fn parses_setup_and_internal() {
        assert_eq!(p(&["setup"]), Cmd::Setup);
        assert_eq!(
            p(&["internal", "session-start"]),
            Cmd::Internal(vec!["session-start".into()])
        );
        assert_eq!(
            p(&["internal", "hook-payload", "source"]),
            Cmd::Internal(vec!["hook-payload".into(), "source".into()])
        );
    }
```

`tests/internal.rs`:
```rust
mod common;
use common::Env;

#[test]
fn hook_payload_extracts_field() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "hook-payload", "source"])
        .write_stdin(r#"{"source":"startup"}"#)
        .assert()
        .success()
        .stdout(predicates::str::diff("startup\n"));
}

#[test]
fn unknown_internal_handler_is_silent_success() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "does-not-exist"])
        .write_stdin("{}")
        .assert()
        .success();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env"; cargo test --test internal; cargo test cli`
Expected: FAIL — `internal` subcommand hits the launch arm / variant missing.

- [ ] **Step 3: Add the CLI variants**

In `src/cli.rs`, extend the `Cmd` enum:
```rust
    Setup,
    Internal(Vec<String>),
```
In `parse`, add these arms **before** the `other if other.starts_with('-')` / bare-name(launch) handling (alongside the existing `"config"` arm):
```rust
        "setup" => Ok(Cmd::Setup),
        "internal" => Ok(Cmd::Internal(it.collect())),
```

- [ ] **Step 4: Write internal.rs**

`src/internal.rs`:
```rust
use anyhow::Result;
use std::path::PathBuf;

use crate::hookio;
use crate::workspace::Workspace;

/// The workspace a hook is running inside, or None when not in a ws launch.
pub fn current_ws() -> Option<Workspace> {
    let name = std::env::var("WS_WORKSPACE").ok().filter(|s| !s.is_empty())?;
    let root = std::env::var("WS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    Some(Workspace { name, root })
}

pub fn run(args: Vec<String>) -> Result<()> {
    let handler = args.first().map(String::as_str).unwrap_or("");
    match handler {
        "hook-payload" => hook_payload(args.get(1).map(String::as_str).unwrap_or("")),
        // real handlers are added in later tasks:
        _ => {} // unknown → silent no-op (never break the agent)
    }
    Ok(())
}

/// `ws internal hook-payload <field>` — print one field of the stdin hook JSON.
fn hook_payload(field: &str) {
    let h = hookio::read_stdin();
    let value = match field {
        "session_id" => h.session_id,
        "cwd" => h.cwd,
        "source" => h.source,
        "prompt" => h.prompt,
        "tool_name" => h.tool_name,
        "command" => h.tool_input.command,
        "agent_id" => h.agent_id.unwrap_or_default(),
        _ => String::new(),
    };
    println!("{value}");
}
```

- [ ] **Step 5: Route the variants in main.rs**

Add `mod internal;` to `src/main.rs`. In `run()`'s match add:
```rust
        Cmd::Setup => commands::setup()?,
        Cmd::Internal(args) => internal::run(args)?,
```
`commands::setup` does not exist yet — to keep this task compiling, add a temporary stub to `src/commands.rs`:
```rust
pub fn setup() -> Result<()> {
    anyhow::bail!("ws setup is implemented in a later task");
}
```
(Task 11 replaces this stub with the real implementation.)

- [ ] **Step 6: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/internal.rs src/cli.rs src/main.rs src/commands.rs tests/internal.rs
git commit -m "feat: ws internal dispatch + gating + hook-payload"
```

---

### Task 5: `session-start` handler

**Files:**
- Modify: `src/internal.rs` (add `session_start`)
- Test: extend `tests/internal.rs`

**Interfaces:**
- Consumes: `internal::current_ws`, `hookio`, `timeline`, `actors`.
- Behavior: no-op (exit 0, no stdout) when not in a ws workspace or when `agent_id` is set (subagent). Otherwise: append a start line to `session_log`; if `source` ∈ {startup, resume} record a timeline `opened` event; build a context string from the README objective + a list of notebook files; print the SessionStart `additionalContext` JSON.

- [ ] **Step 1: Write the failing test**

Add to `tests/internal.rs`:
```rust
fn adopt_ws(env: &Env, name: &str) -> std::path::PathBuf {
    let proj = env.home.path().join(name);
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", name]).assert().success();
    proj
}

#[test]
fn session_start_injects_context_and_logs_opened() {
    let env = Env::new();
    let proj = adopt_ws(&env, "proj");

    env.cmd()
        .env("WS_WORKSPACE", "proj")
        .env("WS_DIR", &proj)
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup","cwd":"x"}"#)
        .assert()
        .success()
        .stdout(predicates::str::contains("hookSpecificOutput"))
        .stdout(predicates::str::contains("proj"));

    // timeline recorded an "opened" event (plus the "created" from adopt)
    let tl = std::fs::read_to_string(proj.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"opened\""));
}

#[test]
fn session_start_noop_outside_workspace() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup"}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn session_start_noop_in_subagent() {
    let env = Env::new();
    let proj = adopt_ws(&env, "sub");
    env.cmd()
        .env("WS_WORKSPACE", "sub")
        .env("WS_DIR", &proj)
        .args(["internal", "session-start"])
        .write_stdin(r#"{"source":"startup","agent_id":"abc"}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test internal session_start`
Expected: FAIL — handler not dispatched (unknown → silent, so stdout is empty and the "hookSpecificOutput" assertion fails).

- [ ] **Step 3: Implement the handler**

In `src/internal.rs`, add the dispatch arm inside `run`'s match (before the `_ =>`):
```rust
        "session-start" => session_start(),
```
Then add:
```rust
use crate::actors;
use crate::timeline;

fn session_start() {
    let h = hookio::read_stdin();
    // subagents don't drive session lifecycle
    if h.agent_id.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
        return;
    }
    let ws = match current_ws() {
        Some(w) => w,
        None => return, // not a ws launch → no context, exit 0
    };

    // audit log
    let _ = append_log(&ws, &format!("session started (source: {})", h.source));

    // timeline: opened (only on a real start/resume, not clear/compact)
    if h.source == "startup" || h.source == "resume" {
        let _ = timeline::record(&ws.timeline(), "opened", &actors::actor_slug(), serde_json::json!({}));
    }

    // build injected context
    let context = build_context(&ws);
    println!("{}", hookio::additional_context("SessionStart", &context));
}

fn build_context(ws: &Workspace) -> String {
    let mut s = format!("# ws workspace: {}\n\n", ws.name);

    if let Ok(readme) = std::fs::read_to_string(ws.readme()) {
        if let Some(obj) = objective_of(&readme) {
            s.push_str(&format!("Objective: {obj}\n\n"));
        }
    }

    // list notebook files so the agent knows where to read/append
    if let Ok(rd) = std::fs::read_dir(ws.notebook_dir()) {
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("notebook.") && n.ends_with(".md"))
            .collect();
        names.sort();
        if !names.is_empty() {
            s.push_str("Notebook files (append findings to your own): ");
            s.push_str(&names.join(", "));
            s.push_str("\n\n");
        }
    }

    s.push_str(
        "Protocol: read .ws/README.md and .ws/notebook/ on start; append findings to \
         your own notebook (ws -whoami for your actor); write a handoff to .ws/handoffs/ \
         on rotate or agent switch; store secrets via ws -secrets, never in files.",
    );
    s
}

/// Extract the first non-empty, non-placeholder line of the README Objective section.
fn objective_of(readme: &str) -> Option<String> {
    let mut in_obj = false;
    for line in readme.lines() {
        if line.starts_with("## ") {
            in_obj = line.starts_with("## Objective");
            continue;
        }
        if in_obj {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let placeholder = (t.starts_with("_(") && t.ends_with(")_"))
                || (t.starts_with('[') && t.ends_with(']'));
            if placeholder {
                return None;
            }
            return Some(t.to_string());
        }
    }
    None
}

fn append_log(ws: &Workspace, msg: &str) -> std::io::Result<()> {
    let path = ws.session_log();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "[{}] {}", crate::now_iso(), msg)
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test internal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs tests/internal.rs
git commit -m "feat: session-start hook handler (context injection + opened event)"
```

---

### Task 6: `user-prompt` handler (objective capture)

**Files:**
- Modify: `src/internal.rs` (add `user_prompt`)
- Test: extend `tests/internal.rs`

**Interfaces:**
- Consumes: `internal::current_ws`, `hookio`, `readme::capture_objective`.
- Behavior: no-op when not in a ws workspace. Otherwise capture the prompt as the README objective (best-effort). Emits no stdout (Phase 2 UserPromptSubmit is capture-only — the slimmed scope grounding from the spec is intentionally omitted).

- [ ] **Step 1: Write the failing test**

Add to `tests/internal.rs`:
```rust
#[test]
fn user_prompt_captures_objective() {
    let env = Env::new();
    let proj = adopt_ws(&env, "obj");

    // README starts with the placeholder
    let readme = proj.join(".ws/README.md");
    assert!(std::fs::read_to_string(&readme).unwrap().contains("_(captured from the first prompt)_"));

    env.cmd()
        .env("WS_WORKSPACE", "obj")
        .env("WS_DIR", &proj)
        .args(["internal", "user-prompt"])
        .write_stdin(r#"{"prompt":"Implement the widget parser"}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    let after = std::fs::read_to_string(&readme).unwrap();
    assert!(after.contains("Implement the widget parser"));
    assert!(!after.contains("_(captured from the first prompt)_"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test internal user_prompt`
Expected: FAIL — objective unchanged (handler not implemented).

- [ ] **Step 3: Implement the handler**

In `src/internal.rs`, add the dispatch arm:
```rust
        "user-prompt" => user_prompt(),
```
and:
```rust
use crate::readme;

fn user_prompt() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if h.prompt.trim().is_empty() {
        return;
    }
    let _ = readme::capture_objective(&ws.readme(), &h.prompt);
    // Phase 2: capture only, no context injection → no stdout.
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test internal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs tests/internal.rs
git commit -m "feat: user-prompt hook handler (README objective capture)"
```

---

### Task 7: `stop` handler (notebook reminder with cooldown)

**Files:**
- Modify: `src/internal.rs` (add `stop`)
- Test: extend `tests/internal.rs`

**Interfaces:**
- Consumes: `internal::current_ws`, `hookio`.
- Behavior: no-op → `decision:approve` when not in a ws workspace. Otherwise, cooldown-gated at most once per 300s via a stamp file `local/notebook-reminder.stamp`:
  - if no notebook file exists yet → approve;
  - if the newest notebook file was modified within the cooldown → approve (recently updated);
  - if the stamp was written within the cooldown → approve;
  - else → write the stamp (now) and emit `decision:block` with a reminder to update the notebook.

- [ ] **Step 1: Write the failing test**

Add to `tests/internal.rs`:
```rust
#[test]
fn stop_reminds_then_cools_down() {
    let env = Env::new();
    let proj = adopt_ws(&env, "nb");

    // Age the notebook file well past the cooldown so the reminder fires.
    // (Set mtime to the epoch via `touch -t`.)
    let nb = std::fs::read_dir(proj.join(".ws/notebook"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("notebook."))
        .unwrap();
    std::process::Command::new("touch").args(["-t", "200001010000"]).arg(&nb).status().unwrap();

    // First stop → block with a reminder
    env.cmd()
        .env("WS_WORKSPACE", "nb").env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"decision\":\"block\""))
        .stdout(predicates::str::contains("notebook"));

    // Second stop immediately after → cooldown holds → approve
    env.cmd()
        .env("WS_WORKSPACE", "nb").env("WS_DIR", &proj)
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"decision\":\"approve\""));
}

#[test]
fn stop_approves_outside_workspace() {
    let env = Env::new();
    env.cmd()
        .args(["internal", "stop"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("\"decision\":\"approve\""));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test internal stop`
Expected: FAIL — handler unknown → prints nothing, so the `decision` assertions fail.

- [ ] **Step 3: Implement the handler**

In `src/internal.rs`, add the dispatch arm:
```rust
        "stop" => stop(),
```
and:
```rust
const COOLDOWN_SECS: u64 = 300;

fn stop() {
    let ws = match current_ws() {
        Some(w) => w,
        None => {
            println!("{}", hookio::decision_approve());
            return;
        }
    };
    let _ = hookio::read_stdin(); // drain stdin; Stop payload is unused

    let newest_nb = newest_mtime_secs(&ws.notebook_dir());
    let stamp = ws.local_dir().join("notebook-reminder.stamp");
    let stamp_age = age_secs(&stamp);

    let approve = match newest_nb {
        None => true, // nothing to nag about yet
        Some(nb_age) if nb_age < COOLDOWN_SECS => true, // just updated
        _ => stamp_age.map(|a| a < COOLDOWN_SECS).unwrap_or(false), // cooled down recently
    };

    if approve {
        println!("{}", hookio::decision_approve());
        return;
    }

    // touch the stamp and remind
    let _ = std::fs::create_dir_all(ws.local_dir());
    let _ = std::fs::write(&stamp, crate::now_iso());
    let reason = "Notebook check. Append any new findings to your own notebook \
        (.ws/notebook/notebook.<actor>.md — run `ws -whoami` if unsure which actor \
        you are; never edit a teammate's). If a prior note was disproven by your recent \
        work, correct it. If nothing needs changing, say so in one line and stop.";
    println!("{}", hookio::decision_block(reason));
}

/// Age in seconds of the newest `notebook.*.md` in `dir`, or None if none exist.
fn newest_mtime_secs(dir: &std::path::Path) -> Option<u64> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut newest: Option<std::time::SystemTime> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("notebook.") && name.ends_with(".md")) {
            continue;
        }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            newest = Some(newest.map_or(m, |n| n.max(m)));
        }
    }
    newest.map(system_time_age_secs)
}

fn age_secs(path: &std::path::Path) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(system_time_age_secs(m))
}

fn system_time_age_secs(t: std::time::SystemTime) -> u64 {
    std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test internal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs tests/internal.rs
git commit -m "feat: stop hook handler (notebook reminder, 5-min cooldown)"
```

---

### Task 8: `bash-audit` + `session-end` handlers

**Files:**
- Modify: `src/internal.rs` (add `bash_audit`, `session_end`)
- Test: extend `tests/internal.rs`

**Interfaces:**
- `bash-audit` (PreToolUse/Bash): no-op unless in a ws workspace and `tool_name == "Bash"` with a non-empty command; append `[ts] BASH: <command truncated to 200 chars>` to `session_log`; no stdout.
- `session-end` (SessionEnd): no-op unless in a ws workspace; record a timeline `closed` event; no stdout.

- [ ] **Step 1: Write the failing test**

Add to `tests/internal.rs`:
```rust
#[test]
fn bash_audit_logs_command() {
    let env = Env::new();
    let proj = adopt_ws(&env, "aud");
    env.cmd()
        .env("WS_WORKSPACE", "aud").env("WS_DIR", &proj)
        .args(["internal", "bash-audit"])
        .write_stdin(r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());

    let log = std::fs::read_to_string(proj.join(".ws/local/log/session.log")).unwrap();
    assert!(log.contains("BASH: echo hi"));
}

#[test]
fn bash_audit_ignores_non_bash() {
    let env = Env::new();
    let proj = adopt_ws(&env, "aud2");
    env.cmd()
        .env("WS_WORKSPACE", "aud2").env("WS_DIR", &proj)
        .args(["internal", "bash-audit"])
        .write_stdin(r#"{"tool_name":"Edit","tool_input":{"command":""}}"#)
        .assert()
        .success();
    assert!(!proj.join(".ws/local/log/session.log").exists()
        || !std::fs::read_to_string(proj.join(".ws/local/log/session.log")).unwrap().contains("BASH"));
}

#[test]
fn session_end_records_closed() {
    let env = Env::new();
    let proj = adopt_ws(&env, "end");
    env.cmd()
        .env("WS_WORKSPACE", "end").env("WS_DIR", &proj)
        .args(["internal", "session-end"])
        .write_stdin(r#"{"reason":"exit"}"#)
        .assert()
        .success();
    let tl = std::fs::read_to_string(proj.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"closed\""));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test internal bash_audit session_end`
Expected: FAIL — handlers not dispatched.

- [ ] **Step 3: Implement the handlers**

In `src/internal.rs`, add the dispatch arms:
```rust
        "bash-audit" => bash_audit(),
        "session-end" => session_end(),
```
and:
```rust
fn bash_audit() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if h.tool_name != "Bash" {
        return;
    }
    let mut cmd = h.tool_input.command;
    if cmd.is_empty() {
        return;
    }
    if cmd.chars().count() > 200 {
        cmd = format!("{}...", cmd.chars().take(200).collect::<String>());
    }
    let _ = append_log(&ws, &format!("BASH: {cmd}"));
}

fn session_end() {
    let _ = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    let _ = timeline::record(&ws.timeline(), "closed", &actors::actor_slug(), serde_json::json!({}));
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test internal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs tests/internal.rs
git commit -m "feat: bash-audit + session-end hook handlers"
```

---

### Task 9: Hook setup (shim render + settings.json merge)

**Files:**
- Create: `src/hooksetup.rs`
- Modify: `src/main.rs` (`mod hooksetup;`)
- Test: unit tests in `hooksetup.rs`

**Interfaces:**
- Consumes: `config::ws_config_dir`.
- Produces:
  ```rust
  pub struct HookSpec { pub event: &'static str, pub matcher: Option<&'static str>, pub handler: &'static str, pub script: &'static str }
  pub const HOOKS: &[HookSpec];   // the 5 Phase-2 hooks
  pub fn hooks_dir() -> std::path::PathBuf;          // ws_config_dir()/hooks
  pub fn claude_settings_path() -> std::path::PathBuf; // home/.claude/settings.json
  /// Materialize shim scripts (referencing `ws_bin`) and register them in
  /// settings.json. Idempotent; preserves all non-ws hook entries.
  pub fn install(ws_bin: &std::path::Path) -> anyhow::Result<usize>;   // returns #hooks installed
  ```
- Shim template (rendered per handler): `#!/bin/sh\n# ws hook — thin shim (no jq/python)\nexec "<ws_bin>" internal <handler>\n`.

- [ ] **Step 1: Write the failing test**

In `src/hooksetup.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn iso() -> TempDir {
        let d = TempDir::new().unwrap();
        std::env::set_var("HOME", d.path());
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        d
    }

    #[test]
    fn install_writes_scripts_and_registers_hooks() {
        let _d = iso();
        let ws_bin = std::path::Path::new("/opt/ws/ws");
        let n = install(ws_bin).unwrap();
        assert_eq!(n, HOOKS.len());

        // scripts exist and are executable, referencing the bin
        let s = std::fs::read_to_string(hooks_dir().join("session-start.sh")).unwrap();
        assert!(s.contains("/opt/ws/ws"));
        assert!(s.contains("internal session-start"));

        // settings.json has our hooks under the right events
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_settings_path()).unwrap()).unwrap();
        let ss = &settings["hooks"]["SessionStart"];
        assert!(ss.is_array());
        let cmd = ss[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.ends_with("session-start.sh"));
        // Bash matcher preserved
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    }

    #[test]
    fn install_is_idempotent_and_preserves_foreign_hooks() {
        let _d = iso();
        // pre-existing foreign (cs-style) hook must survive
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        std::fs::write(&sp, r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"~/.claude/hooks/cs/session-start.sh"}]}]}}"#).unwrap();

        install(std::path::Path::new("/opt/ws/ws")).unwrap();
        install(std::path::Path::new("/opt/ws/ws")).unwrap(); // twice

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
        let arr = settings["hooks"]["SessionStart"].as_array().unwrap();
        // exactly one foreign + exactly one ws entry (idempotent)
        let foreign = arr.iter().filter(|g| g["hooks"][0]["command"].as_str().unwrap().contains("/cs/")).count();
        let ours = arr.iter().filter(|g| g["hooks"][0]["command"].as_str().unwrap().ends_with("session-start.sh") && !g["hooks"][0]["command"].as_str().unwrap().contains("/cs/")).count();
        assert_eq!(foreign, 1);
        assert_eq!(ours, 1);
    }
}
```
Note: these tests mutate global env (`HOME`), so they rely on the crate-wide `RUST_TEST_THREADS=1`.

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test hooksetup`
Expected: FAIL — module undefined.

- [ ] **Step 3: Write hooksetup.rs**

`src/hooksetup.rs`:
```rust
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct HookSpec {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub handler: &'static str,
    pub script: &'static str,
}

pub const HOOKS: &[HookSpec] = &[
    HookSpec { event: "SessionStart", matcher: None, handler: "session-start", script: "session-start.sh" },
    HookSpec { event: "UserPromptSubmit", matcher: None, handler: "user-prompt", script: "user-prompt.sh" },
    HookSpec { event: "PreToolUse", matcher: Some("Bash"), handler: "bash-audit", script: "bash-audit.sh" },
    HookSpec { event: "Stop", matcher: None, handler: "stop", script: "stop.sh" },
    HookSpec { event: "SessionEnd", matcher: None, handler: "session-end", script: "session-end.sh" },
];

pub fn hooks_dir() -> PathBuf {
    crate::config::ws_config_dir().join("hooks")
}

pub fn claude_settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}

fn render_shim(ws_bin: &Path, handler: &str) -> String {
    format!(
        "#!/bin/sh\n# ws hook — thin shim (no jq/python); ws does the work.\nexec \"{}\" internal {}\n",
        ws_bin.display(),
        handler
    )
}

pub fn install(ws_bin: &Path) -> Result<usize> {
    let dir = hooks_dir();
    std::fs::create_dir_all(&dir)?;

    for spec in HOOKS {
        let path = dir.join(spec.script);
        std::fs::write(&path, render_shim(ws_bin, spec.handler))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&path)?.permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&path, p)?;
        }
    }

    register_settings(&claude_settings_path(), &dir)?;
    Ok(HOOKS.len())
}

fn register_settings(settings_path: &Path, hooks_dir: &Path) -> Result<()> {
    let mut root: Value = std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    let hooks_prefix = hooks_dir.to_string_lossy().to_string();

    let obj = root.as_object_mut().unwrap();
    let hooks_entry = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks_entry.is_object() {
        *hooks_entry = json!({});
    }
    let hooks_obj = hooks_entry.as_object_mut().unwrap();

    for spec in HOOKS {
        let arr_entry = hooks_obj.entry(spec.event).or_insert_with(|| json!([]));
        if !arr_entry.is_array() {
            *arr_entry = json!([]);
        }
        let arr = arr_entry.as_array_mut().unwrap();
        // drop stale ws entries (command under our hooks dir), keep everything else
        arr.retain(|group| !group_is_ws(group, &hooks_prefix));

        let command = hooks_dir.join(spec.script).to_string_lossy().to_string();
        let mut group = serde_json::Map::new();
        if let Some(m) = spec.matcher {
            group.insert("matcher".into(), json!(m));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": command, "timeout": 10 }]),
        );
        arr.push(Value::Object(group));
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn group_is_ws(group: &Value, hooks_prefix: &str) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.starts_with(hooks_prefix))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}
```
Add `mod hooksetup;` to `src/main.rs`.

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test hooksetup`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hooksetup.rs src/main.rs
git commit -m "feat: hook shim materialization + non-destructive settings.json merge"
```

---

### Task 10: Prompt-ware assets + installer

**Files:**
- Create: `src/assets/prompts/summary.md`, `wrap.md`, `sweep.md`, `rotate.md`
- Create: `src/prompts.rs`
- Modify: `src/main.rs` (`mod prompts;`)
- Test: unit tests in `prompts.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const PROMPTS: &[(&str, &str)];   // (filename, embedded body)
  pub fn commands_dir() -> std::path::PathBuf;   // home/.claude/commands/ws
  pub fn install() -> anyhow::Result<usize>;     // writes each prompt; returns count
  ```

- [ ] **Step 1: Write the prompt asset files**

`src/assets/prompts/summary.md`:
```markdown
---
model: claude-sonnet-5
---

Generate an intelligent summary of this ws workspace by synthesizing its documentation.

You are working in a **ws** workspace. Write `.ws/summary.md` — a concise, high-signal overview a future session (or a different agent) can read first to get oriented.

## Sources (read these)
- `.ws/README.md` — objective and outcome
- `.ws/notebook/notebook.*.md` — per-actor lab notebooks (findings, decisions)
- `.ws/timeline.jsonl` — lifecycle events
- Recent git history of the workspace

## Write `.ws/summary.md` with
1. **What this workspace is for** (1–2 sentences from the objective).
2. **Current state** — what's done, what's in progress, what's blocked.
3. **Key decisions & findings** — distilled from the notebooks, with the *why*.
4. **Next steps** — concrete, actionable.

Keep it tight. Prefer specifics over generalities. Do not invent facts not present in the sources. Overwrite any existing `.ws/summary.md`.
```

`src/assets/prompts/sweep.md`:
```markdown
---
model: claude-sonnet-5
---

Distill this ws workspace into durable memory entries with a strict bar.

You are in a **ws** workspace. Claude's persistent memory is redirected to `.ws/memory/` (index `MEMORY.md` + `<bucket>_*.md` files). Your task: review the conversation and the workspace, then write only durable facts worth carrying into every future session.

## The bar
- **The memory buckets (`.ws/memory/`) are forever.** Bar: very strict. Default: write nothing.
- Save only what is (a) durable, (b) not already obvious from the code, git history, or README, and (c) useful to a future session.

## Buckets
- **user** — who the user is (role, expertise, durable preferences).
- **feedback** — how you should work here (a correction or confirmed approach). Include the *why*.
- **project** — ongoing goals/constraints not derivable from the code.
- **reference** — pointers to external resources (URLs, tickets, dashboards).

## Steps
1. Review the whole conversation, not just the last turn.
2. For each candidate fact, check it clears the bar and isn't a duplicate of an existing entry — update the existing file rather than adding a near-duplicate.
3. Write each as one file with frontmatter (`name`, `description`, `metadata.type`), then add a one-line pointer to `.ws/memory/MEMORY.md`.
4. Delete any entry you now know to be wrong.

If nothing clears the bar, say so in one line and write nothing.
```

`src/assets/prompts/wrap.md`:
```markdown
---
model: claude-sonnet-5
---

Wrap up this ws workspace: distill durable memory, then write a summary.

Run two passes in sequence:

1. **Memory pass** — follow the `/ws:sweep` instructions: distill durable facts into `.ws/memory/` with a strict bar (default: write nothing).
2. **Summary pass** — follow the `/ws:summary` instructions: synthesize `.ws/summary.md` from the README, notebooks, timeline, and git history.

Then complete the **Outcome** section of `.ws/README.md`: a few sentences on what this workspace accomplished, its final state, and anything left for next time.

Report back a one-paragraph recap: what you saved to memory (or that you saved nothing), and that the summary and outcome are written.
```

`src/assets/prompts/rotate.md`:
```markdown
---
model: claude-sonnet-5
---

Rotate this ws conversation: write a handoff so a fresh conversation can continue.

Context grows heavy or a work phase is done — capture the state so the next conversation starts clean.

## Write a handoff to `.ws/handoffs/<UTC-timestamp>.md` containing
1. **Objective** — what this workspace is for (one line).
2. **Where things stand** — done / in-progress / blocked, with file:line pointers.
3. **Next action** — the single most useful thing to do next, concretely.
4. **Watch out for** — traps, gotchas, decisions already made (and why) so they aren't relitigated.
5. **How to resume** — the exact command(s), and which files to read first (`.ws/README.md`, `.ws/notebook/`, this handoff).

Also append any fresh findings to your own notebook (`.ws/notebook/notebook.<actor>.md`; run `ws -whoami` for your actor). Keep the handoff self-contained: assume the next agent has zero prior context. Finish by telling the user the handoff path and how to reopen.
```

- [ ] **Step 2: Write the failing test**

In `src/prompts.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_writes_all_namespaced_prompts() {
        let d = TempDir::new().unwrap();
        std::env::set_var("HOME", d.path());
        let n = install().unwrap();
        assert_eq!(n, PROMPTS.len());
        for (name, _) in PROMPTS {
            let p = commands_dir().join(name);
            assert!(p.is_file(), "missing {name}");
        }
        // namespaced under commands/ws (so /ws:summary, never clobbering cs's /summary)
        assert!(commands_dir().ends_with("commands/ws"));
        assert!(std::fs::read_to_string(commands_dir().join("rotate.md")).unwrap().contains("handoff"));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test prompts`
Expected: FAIL — module undefined.

- [ ] **Step 4: Write prompts.rs**

`src/prompts.rs`:
```rust
use anyhow::Result;
use std::path::PathBuf;

pub const PROMPTS: &[(&str, &str)] = &[
    ("summary.md", include_str!("assets/prompts/summary.md")),
    ("wrap.md", include_str!("assets/prompts/wrap.md")),
    ("sweep.md", include_str!("assets/prompts/sweep.md")),
    ("rotate.md", include_str!("assets/prompts/rotate.md")),
];

pub fn commands_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("commands")
        .join("ws")
}

pub fn install() -> Result<usize> {
    let dir = commands_dir();
    std::fs::create_dir_all(&dir)?;
    for (name, body) in PROMPTS {
        std::fs::write(dir.join(name), body)?;
    }
    Ok(PROMPTS.len())
}
```
Add `mod prompts;` to `src/main.rs`.

- [ ] **Step 5: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test prompts`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/assets/prompts src/prompts.rs src/main.rs
git commit -m "feat: /ws:* prompt-ware assets + installer"
```

---

### Task 11: `ws setup` command + WS_DIR export

**Files:**
- Modify: `src/commands.rs` (replace the `setup` stub with the real one)
- Modify: `src/agents/claude.rs` (export `WS_DIR` on launch)
- Test: `tests/setup.rs`

**Interfaces:**
- Consumes: `hooksetup::install`, `prompts::install`.
- Produces: real `commands::setup() -> Result<()>`.
- Adds `WS_DIR=<ws.root>` to the launch env so hooks locate the workspace even when `cwd` isn't in the payload.

- [ ] **Step 1: Write the failing test**

`tests/setup.rs`:
```rust
mod common;
use common::Env;

#[test]
fn setup_installs_hooks_and_prompts() {
    let env = Env::new();
    env.cmd()
        .arg("setup")
        .assert()
        .success()
        .stdout(predicates::str::contains("hook"))
        .stdout(predicates::str::contains("prompt"));

    // settings.json registered a ws SessionStart hook
    let settings = env.home.path().join(".claude/settings.json");
    let body = std::fs::read_to_string(&settings).unwrap();
    assert!(body.contains("session-start.sh"));

    // namespaced prompts installed
    assert!(env.home.path().join(".claude/commands/ws/summary.md").is_file());
    assert!(env.home.path().join(".claude/commands/ws/rotate.md").is_file());
}
```
Also add to `tests/internal.rs` a check that WS_DIR flows through the launch (extends the Phase 1 fake-shim launch test family):
```rust
// Add to tests/launch.rs (Phase 1 fake-shim harness) — verifies WS_DIR export.
#[test]
fn launch_exports_ws_dir() {
    let env = Env::new();
    let shim = env.fake_claude();
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", &shim).env("WS_NO_EXEC", "1");
    c.arg("wsdirtest").assert().success();
    // fake_claude logs WS_WORKSPACE; extend the shim to also log WS_DIR (see Step 3).
    assert!(env.argv_log().contains("WSDIR:"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test setup`
Expected: FAIL — `setup` still bails with the stub message.

- [ ] **Step 3: Implement setup + WS_DIR export**

Replace the stub in `src/commands.rs`:
```rust
pub fn setup() -> Result<()> {
    let ws_bin = std::env::current_exe()?;
    let n_hooks = crate::hooksetup::install(&ws_bin)?;
    let n_prompts = crate::prompts::install()?;
    println!(
        "ws setup: installed {n_hooks} hook(s) → {}\n            installed {n_prompts} prompt(s) → {}",
        crate::hooksetup::claude_settings_path().display(),
        crate::prompts::commands_dir().display(),
    );
    Ok(())
}
```

In `src/agents/claude.rs`, in `launch`, add the `WS_DIR` env alongside the existing envs:
```rust
        cmd.current_dir(&ws.root)
            .env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", ws.memory_dir())
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
```

Extend the fake-claude shim in `tests/common/mod.rs` to also log `WS_DIR` (add one line to the heredoc):
```
             echo \"WSDIR: $WS_DIR\"\n\
```
(insert next to the existing `WSW: $WS_WORKSPACE` line).

Add a unit assertion in `src/agents/claude.rs` tests (the Phase 1 `sets_memory_redirect_and_ws_env` test) is unaffected — it only checks specific keys; `WS_DIR` is additive. Optionally extend it:
```rust
        assert_eq!(env_of(&cmd, "WS_DIR"), Some("/tmp/ws/proj".into()));
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS (setup integration, WS_DIR export, and the full suite).

- [ ] **Step 5: Manual smoke (optional but recommended)**

Run against an isolated HOME so it doesn't touch your real Claude config:
```bash
. "$HOME/.cargo/env"
cargo build --release
HOME=$(mktemp -d) XDG_CONFIG_HOME=$(mktemp -d) ./target/release/ws setup
```
Expected: prints the hook + prompt install summary; `<home>/.claude/settings.json` and `<home>/.claude/commands/ws/*.md` exist.

- [ ] **Step 6: Commit**

```bash
git add src/commands.rs src/agents/claude.rs tests/setup.rs tests/common/mod.rs tests/launch.rs
git commit -m "feat: ws setup (install hooks + prompts) + WS_DIR launch env"
```

---

## Self-Review

**1. Spec coverage (§17 Phase 2 items):**
- prompts `/ws:summary /ws:wrap /ws:sweep /ws:rotate` — Task 10 ✓ (namespaced per the user decision)
- session-start hook (context injection) — Task 5 ✓
- objective hook (first-prompt → README) — Tasks 3 + 6 ✓
- notebook-reminder hook — Task 7 ✓
- audit hook (Bash) — Task 8 ✓
- timeline — Task 1 ✓ (created/opened/closed; rotated/agent-switch come with rotate/switch flows in later phases)
- README auto-objective — Tasks 3 + 6 ✓
- installer (`ws setup`) tying it together — Tasks 9, 10, 11 ✓
- `ws internal` helper mode (zero-dep JSON, spec §8) — Tasks 2, 4–8 ✓

**2. Placeholder scan:** every step carries complete code or complete prompt-file content — no "TBD"/"similar to"/"add error handling".

**3. Type consistency:** `HookInput`/`ToolInput` (Task 2) are consumed unchanged in Tasks 5–8. `hookio::{additional_context, decision_approve, decision_block}` signatures match their call sites. `internal::current_ws() -> Option<Workspace>` uses the Phase 1 `Workspace{name, root}` public fields and the new path helpers from Task 1 (`readme`, `notebook_dir`, `timeline`, `session_log`). `hooksetup::HOOKS[*].handler` strings (`session-start`, `user-prompt`, `bash-audit`, `stop`, `session-end`) exactly match the dispatch arms in `internal::run`. `Cmd::{Setup, Internal}` (Task 4) are routed in Task 4's main.rs edit; `commands::setup` is stubbed in Task 4 and finalized in Task 11.

**Deferred (correctly out of Phase 2 scope):** statusline + limits (Phase 3), Codex/Gemini hook installation (Phases 4/9), autosave/secret-redaction/prose-lint/artifact-tracking hooks (Phases 5/9), rotated/agent-switch timeline events (Phases 4+), `ws -conversations` (later). The scope-grounding half of the UserPromptSubmit hook is intentionally omitted per spec §8 ("deliberately slimmed to record first prompt as objective").

**Known simplifications (intentional, non-blocking):** timeline appends are not atomic (consistent with the Phase-1 write style; a hardening follow-up already tracks atomic writes); the `stop` cooldown uses file mtimes rather than a monotonic clock (fine for a 5-minute human-scale gate); `ws setup` does not yet register the statusline (Phase 3 extends it).
