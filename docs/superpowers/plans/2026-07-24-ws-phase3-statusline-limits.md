# ws Phase 3 (Statusline + Limits) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ws` sense Claude's rate-limit windows via its own statusline, show them (`ws statusline`, `ws -limits`, a live subagent statusline), and warn-and-save before the wall via a threshold-triggered Stop directive.

**Architecture:** Claude Code invokes a registered `statusLine` command once per refresh, piping a JSON blob (model, context %, `rate_limits`, cost, cwd) on stdin. `ws statusline` parses it, renders a single line (git branch · workspace · ctx% · 5h% + reset · cost), and best-effort captures `rate_limits` to `.ws/local/limits.json` + a global copy. A parallel `subagentStatusLine` command receives `{columns, tasks[]}` and emits one `{id, content}` row per running subagent (model · name · task · ctx% · elapsed). The Stop hook (from Phase 2) reads `limits.json`; once a threshold is crossed it drops a guard marker and blocks with a "finish this step, write a handoff, then stop" directive (plus a macOS notification); afterwards the UserPromptSubmit hook prefixes a one-line "limit guard active" notice. `ws setup` registers both statuslines, recording any prior command as a backup.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `anyhow`, `dirs`. Git branch via system `git`. Builds on the Phase 1+2 crate. Dev: `assert_cmd`, `predicates`, `tempfile`.

## Global Constraints

- **Rust 2021, single crate, binary `ws`.** cargo is NOT on the default PATH — prefix every cargo call with `. "$HOME/.cargo/env";` (Rust 1.97.1). serde/serde_json/dirs already dependencies.
- **Zero runtime deps beyond git + agents.** The statusline commands are Rust (`ws statusline`, `ws subagent-statusline`) — no jq/python.
- **Statusline runs every refresh (≈1s) in every Claude session** → it must be FAST and NEVER error: parse failures render a minimal line, capture is best-effort, exit 0 always. `Cmd::Statusline`/`Cmd::SubagentStatusline` must never exit non-zero.
- **Verified statusline JSON contract (Claude Code 2.1.218):** stdin JSON has `session_name`, `model.display_name`, `effort.level`, `context_window.used_percentage` (number), `rate_limits.five_hour.used_percentage` (number), `rate_limits.five_hour.resets_at` (**Unix epoch seconds**), `rate_limits.seven_day.used_percentage`, `rate_limits.seven_day.resets_at`, `cost.total_cost_usd` (number), `workspace.current_dir` (fallback `cwd`).
- **Verified subagentStatusLine contract:** stdin JSON `{columns: number, tasks: [{id, model, name|type, description, tokenCount, contextWindowSize, start}]}`; output is **one JSON object per line** `{"id": "<task id>", "content": "<rendered row>"}`.
- **Statusline rendered content (user decision):** `⎇ <git-branch> · <ws-workspace-name> · ctx <n>% · 5h <n>% (resets in <Xh Ym>) · $<cost>`. Workspace name only shown when `WS_WORKSPACE` is set. Weekly window is captured + shown in `ws -limits` but not in the one-line render.
- **Config:** `limit_warn_5h` (default 85), `limit_warn_week` (default 90) already exist. This phase ADDS `limit_action = "handoff-stop" | "warn"` (default `"handoff-stop"`).
- **Limit-guard marker:** `<ws>/.ws/local/limit-guard` — written when the handoff directive is issued; cleared automatically when the crossed window drops back below its threshold (a reset).
- **Statusline registration is NOT delegation.** `ws setup` records any pre-existing `statusLine`/`subagentStatusLine` command into `<ws_config_dir>/statusline-backup.json` (so cs-statusline is recoverable), then registers `ws statusline` / `ws subagent-statusline`. All other settings.json keys and hook entries are preserved (reuse the Phase-2 non-destructive read/guard).
- **Never overwrite an unparseable settings.json** — reuse the Phase-2 behavior (bail without writing).
- **Env gating:** limit capture and the workspace-name segment require a ws launch (`WS_WORKSPACE` + `WS_DIR`/cwd, via `internal::current_ws()`); the statusline still renders (branch/ctx/limits/cost from the JSON) in non-ws sessions.
- **Test isolation:** `.cargo/config.toml` pins `RUST_TEST_THREADS=1`; full suite is source of truth (`. "$HOME/.cargo/env"; cargo test`). Integration tests use the `Env` helper (isolated HOME/XDG_CONFIG_HOME/WS_ROOT) and `write_stdin`.

---

## File Structure

```
src/
├── limits.rs         # LimitsSnapshot model, capture/read, threshold check, countdown fmt
├── statusline.rs     # StatuslineInput + render + `ws statusline`; SubagentInput + `ws subagent-statusline`
├── config.rs         # (modify) add limit_action field
├── internal.rs       # (modify) stop() threshold check + guard; user_prompt() guard notice
├── hooksetup.rs      # (modify) register_statuslines() + backup; called by setup
├── commands.rs       # (modify) `ws -limits` (limits listing); setup() also registers statuslines
├── cli.rs            # (modify) Cmd::Statusline, Cmd::SubagentStatusline, Cmd::Limits
└── main.rs           # (modify) route the three new variants; mod limits; mod statusline;
tests/
├── statusline.rs     # ws statusline render + limits capture; subagent-statusline rows
└── limits.rs         # ws -limits listing; threshold Stop directive + guard; guard notice
```

---

### Task 1: Limits module

**Files:**
- Create: `src/limits.rs`
- Modify: `src/main.rs` (`mod limits;`)
- Test: unit tests in `limits.rs`

**Interfaces:**
- Consumes: `serde_json`, `config::ws_config_dir`, `workspace::Workspace`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
  pub struct Window { pub used_pct: f64, pub resets_at: i64 } // resets_at = epoch secs, 0 = unknown
  #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
  pub struct LimitsSnapshot {
      pub agent: String,       // "claude"
      pub five_hour: Window,
      pub seven_day: Window,
      pub stamped_at: i64,     // epoch secs at capture
  }
  pub fn global_path() -> std::path::PathBuf;                 // ws_config_dir()/limits.json
  pub fn write(path: &std::path::Path, snap: &LimitsSnapshot) -> anyhow::Result<()>; // atomic
  pub fn read(path: &std::path::Path) -> Option<LimitsSnapshot>;
  /// Which window (if any) is at/over its warn threshold. Returns "5h" | "week" | None.
  pub fn over_threshold(snap: &LimitsSnapshot, warn_5h: u8, warn_week: u8) -> Option<&'static str>;
  /// "1h20m" style countdown from now→resets_at; "0m" if already passed or unknown.
  pub fn countdown(resets_at: i64, now: i64) -> String;
  pub fn now_epoch() -> i64;
  ```

- [ ] **Step 1: Write the failing test**

In `src/limits.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn snap(five: f64, week: f64) -> LimitsSnapshot {
        LimitsSnapshot {
            agent: "claude".into(),
            five_hour: Window { used_pct: five, resets_at: 1_000_000 },
            seven_day: Window { used_pct: week, resets_at: 2_000_000 },
            stamped_at: 500_000,
        }
    }

    #[test]
    fn write_then_read_roundtrips() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("limits.json");
        let s = snap(43.0, 61.0);
        write(&p, &s).unwrap();
        let back = read(&p).unwrap();
        assert_eq!(back.five_hour.used_pct, 43.0);
        assert_eq!(back.seven_day.resets_at, 2_000_000);
        assert_eq!(back.agent, "claude");
    }

    #[test]
    fn threshold_detection() {
        assert_eq!(over_threshold(&snap(50.0, 50.0), 85, 90), None);
        assert_eq!(over_threshold(&snap(85.0, 50.0), 85, 90), Some("5h"));   // at threshold counts
        assert_eq!(over_threshold(&snap(50.0, 95.0), 85, 90), Some("week"));
        // 5h takes priority when both cross
        assert_eq!(over_threshold(&snap(90.0, 95.0), 85, 90), Some("5h"));
    }

    #[test]
    fn countdown_formats() {
        assert_eq!(countdown(1_000_000, 1_000_000 - 4800), "1h20m"); // 80 min
        assert_eq!(countdown(1_000_000, 1_000_000 - 45), "0h0m");
        assert_eq!(countdown(1_000_000, 1_000_000 + 10), "0m");      // already passed
        assert_eq!(countdown(0, 1_000_000), "0m");                    // unknown
    }

    #[test]
    fn read_missing_is_none() {
        assert!(read(std::path::Path::new("/no/such/limits.json")).is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test limits`
Expected: FAIL — module undefined.

- [ ] **Step 3: Write limits.rs**

`src/limits.rs`:
```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Window {
    pub used_pct: f64,
    pub resets_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LimitsSnapshot {
    pub agent: String,
    pub five_hour: Window,
    pub seven_day: Window,
    pub stamped_at: i64,
}

pub fn global_path() -> PathBuf {
    crate::config::ws_config_dir().join("limits.json")
}

pub fn write(path: &Path, snap: &LimitsSnapshot) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(snap)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read(path: &Path) -> Option<LimitsSnapshot> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn over_threshold(snap: &LimitsSnapshot, warn_5h: u8, warn_week: u8) -> Option<&'static str> {
    if snap.five_hour.used_pct >= warn_5h as f64 {
        return Some("5h");
    }
    if snap.seven_day.used_pct >= warn_week as f64 {
        return Some("week");
    }
    None
}

pub fn countdown(resets_at: i64, now: i64) -> String {
    if resets_at <= 0 || resets_at <= now {
        return if resets_at > 0 && resets_at <= now {
            // passed
            "0m".to_string()
        } else {
            "0m".to_string()
        };
    }
    let secs = resets_at - now;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    format!("{h}h{m}m")
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```
Add `mod limits;` to `src/main.rs`.

Note on `countdown`: the test expects `"1h20m"` for 80 minutes, `"0h0m"` for 45s remaining (under a minute → 0h0m), `"0m"` for a passed/unknown reset. The implementation returns `"0m"` when `resets_at<=0 || resets_at<=now`, else `"{h}h{m}m"`. For 45s: `h=0,m=0` → `"0h0m"`. Correct.

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test limits`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/limits.rs src/main.rs
git commit -m "feat: limits snapshot model (capture/read, threshold, countdown)"
```

---

### Task 2: `ws statusline` — input parse, limit capture, render, wiring

**Files:**
- Create: `src/statusline.rs`
- Modify: `src/cli.rs` (add `Cmd::Statusline`), `src/main.rs` (`mod statusline;`, route)
- Test: `tests/statusline.rs`

**Interfaces:**
- Consumes: `limits`, `internal::current_ws`, `serde_json`.
- Produces:
  ```rust
  #[derive(Debug, Default, serde::Deserialize)]
  pub struct StatuslineInput {
      #[serde(default)] pub session_name: String,
      #[serde(default)] pub model: ModelInfo,
      #[serde(default)] pub context_window: CtxInfo,
      #[serde(default)] pub rate_limits: RateLimits,
      #[serde(default)] pub cost: CostInfo,
      #[serde(default)] pub workspace: WorkspaceInfo,
      #[serde(default)] pub cwd: String,
  }
  // ModelInfo{display_name}, CtxInfo{used_percentage:f64}, CostInfo{total_cost_usd:f64},
  // WorkspaceInfo{current_dir}, RateLimits{five_hour:LimitWindow, seven_day:LimitWindow},
  // LimitWindow{used_percentage:f64, resets_at:i64}
  pub fn to_snapshot(input: &StatuslineInput) -> limits::LimitsSnapshot;
  pub fn render(input: &StatuslineInput, workspace_name: Option<&str>, no_color: bool) -> String;
  pub fn run();   // read stdin, capture (best-effort), render, print; never errors
  ```
- `render` produces: `⎇ <branch> · <workspace> · ctx <n>% · 5h <n>% (resets in <cd>) · $<cost>` (workspace omitted when None; branch omitted when not a git repo).

- [ ] **Step 1: Write the failing test**

`tests/statusline.rs`:
```rust
mod common;
use common::Env;

const SAMPLE: &str = r#"{
  "session_name":"demo",
  "model":{"display_name":"Opus 4.8"},
  "context_window":{"used_percentage":12.4},
  "rate_limits":{
    "five_hour":{"used_percentage":73.0,"resets_at":9999999999},
    "seven_day":{"used_percentage":10.0,"resets_at":9999999999}
  },
  "cost":{"total_cost_usd":1.23},
  "workspace":{"current_dir":"/tmp/x"}
}"#;

#[test]
fn statusline_renders_and_captures() {
    let env = Env::new();
    // create a ws workspace so capture has a home
    let proj = env.home.path().join("sl");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","sl"]).assert().success();

    env.cmd()
        .env("WS_WORKSPACE","sl").env("WS_DIR",&proj).env("NO_COLOR","1")
        .arg("statusline")
        .write_stdin(SAMPLE)
        .assert()
        .success()
        .stdout(predicates::str::contains("sl"))          // workspace name
        .stdout(predicates::str::contains("ctx 12%"))
        .stdout(predicates::str::contains("5h 73%"))
        .stdout(predicates::str::contains("$1.23"));

    // limits.json captured
    let lj = proj.join(".ws/local/limits.json");
    assert!(lj.is_file());
    let body = std::fs::read_to_string(lj).unwrap();
    assert!(body.contains("\"used_pct\": 73"));
}

#[test]
fn statusline_survives_garbage_stdin() {
    let env = Env::new();
    env.cmd()
        .env("NO_COLOR","1")
        .arg("statusline")
        .write_stdin("not json")
        .assert()
        .success(); // never errors
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test statusline`
Expected: FAIL — `statusline` hits the launch arm / variant missing.

- [ ] **Step 3: Add the CLI variant**

In `src/cli.rs` `Cmd` enum add `Statusline,`. In `parse`, alongside the `"setup"`/`"internal"` arms add:
```rust
        "statusline" => Ok(Cmd::Statusline),
```

- [ ] **Step 4: Write statusline.rs**

`src/statusline.rs`:
```rust
use serde::Deserialize;

use crate::limits::{self, LimitsSnapshot, Window};

#[derive(Debug, Default, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub display_name: String,
}
#[derive(Debug, Default, Deserialize)]
pub struct CtxInfo {
    #[serde(default)]
    pub used_percentage: f64,
}
#[derive(Debug, Default, Deserialize)]
pub struct CostInfo {
    #[serde(default)]
    pub total_cost_usd: f64,
}
#[derive(Debug, Default, Deserialize)]
pub struct WorkspaceInfo {
    #[serde(default)]
    pub current_dir: String,
}
#[derive(Debug, Default, Deserialize)]
pub struct LimitWindow {
    #[serde(default)]
    pub used_percentage: f64,
    #[serde(default)]
    pub resets_at: i64,
}
#[derive(Debug, Default, Deserialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: LimitWindow,
    #[serde(default)]
    pub seven_day: LimitWindow,
}
#[derive(Debug, Default, Deserialize)]
pub struct StatuslineInput {
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub model: ModelInfo,
    #[serde(default)]
    pub context_window: CtxInfo,
    #[serde(default)]
    pub rate_limits: RateLimits,
    #[serde(default)]
    pub cost: CostInfo,
    #[serde(default)]
    pub workspace: WorkspaceInfo,
    #[serde(default)]
    pub cwd: String,
}

pub fn to_snapshot(input: &StatuslineInput) -> LimitsSnapshot {
    LimitsSnapshot {
        agent: "claude".into(),
        five_hour: Window {
            used_pct: input.rate_limits.five_hour.used_percentage,
            resets_at: input.rate_limits.five_hour.resets_at,
        },
        seven_day: Window {
            used_pct: input.rate_limits.seven_day.used_percentage,
            resets_at: input.rate_limits.seven_day.resets_at,
        },
        stamped_at: limits::now_epoch(),
    }
}

fn git_branch(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let out = std::process::Command::new("git")
        .args(["-C", cwd, "--no-optional-locks", "branch", "--show-current"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b.is_empty() {
        None
    } else {
        Some(b)
    }
}

pub fn render(input: &StatuslineInput, workspace_name: Option<&str>, no_color: bool) -> String {
    let cwd = if !input.workspace.current_dir.is_empty() {
        input.workspace.current_dir.as_str()
    } else {
        input.cwd.as_str()
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = git_branch(cwd) {
        parts.push(format!("\u{2387} {b}")); // ⎇ branch
    }
    if let Some(w) = workspace_name {
        parts.push(w.to_string());
    }
    parts.push(format!("ctx {}%", input.context_window.used_percentage.round() as i64));

    let five = input.rate_limits.five_hour.used_percentage.round() as i64;
    let cd = limits::countdown(input.rate_limits.five_hour.resets_at, limits::now_epoch());
    let five_seg = format!("5h {five}% (resets in {cd})");
    parts.push(colorize(five_seg, five, no_color));

    parts.push(format!("${:.2}", input.cost.total_cost_usd));
    parts.join(" \u{b7} ") // middot separator
}

/// Escalate the 5h segment color at 85/95 unless NO_COLOR.
fn colorize(seg: String, pct: i64, no_color: bool) -> String {
    if no_color {
        return seg;
    }
    let code = if pct >= 95 {
        "31" // red
    } else if pct >= 85 {
        "33" // yellow
    } else {
        return seg;
    };
    format!("\x1b[{code}m{seg}\x1b[0m")
}

pub fn run() {
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let input: StatuslineInput = serde_json::from_str(&raw).unwrap_or_default();

    // Best-effort limit capture: workspace copy (if in a ws launch) + global copy.
    let snap = to_snapshot(&input);
    let _ = limits::write(&limits::global_path(), &snap);
    let ws_name = std::env::var("WS_WORKSPACE").ok().filter(|s| !s.is_empty());
    if let Some(ws) = crate::internal::current_ws() {
        let _ = limits::write(&ws.local_dir().join("limits.json"), &snap);
    }

    let no_color = std::env::var_os("NO_COLOR").is_some();
    println!("{}", render(&input, ws_name.as_deref(), no_color));
}
```

- [ ] **Step 5: Route in main.rs**

Add `mod statusline;` to `src/main.rs`. In `run()`'s match add:
```rust
        Cmd::Statusline => statusline::run(),
```
(`statusline::run` returns `()`, best-effort — it cannot error, satisfying the never-exit-non-zero rule.)

- [ ] **Step 6: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test statusline; cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/statusline.rs src/cli.rs src/main.rs tests/statusline.rs
git commit -m "feat: ws statusline (render branch/workspace/ctx/5h/cost + limits capture)"
```

---

### Task 3: `ws subagent-statusline`

**Files:**
- Modify: `src/statusline.rs` (add subagent parsing + render + `run_subagent`)
- Modify: `src/cli.rs` (`Cmd::SubagentStatusline`), `src/main.rs` (route)
- Test: extend `tests/statusline.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Default, serde::Deserialize)]
  pub struct SubagentInput { #[serde(default)] pub tasks: Vec<Task> }
  #[derive(Debug, Default, serde::Deserialize)]
  pub struct Task {
      #[serde(default)] pub id: String,
      #[serde(default)] pub model: String,
      #[serde(default)] pub name: String,
      #[serde(default, rename = "type")] pub type_: String,
      #[serde(default)] pub description: String,
      #[serde(default)] pub tokenCount: i64,
      #[serde(default)] pub contextWindowSize: i64,
      #[serde(default)] pub start: i64,  // epoch ms
  }
  pub fn subagent_row(t: &Task, now_ms: i64) -> String;   // "↷ Sonnet 5  local · task… · ctx 3% · 0m10s"
  pub fn run_subagent();   // read {tasks[]}, print one {"id","content"} JSON line per task
  ```

- [ ] **Step 1: Write the failing test**

Add to `tests/statusline.rs`:
```rust
#[test]
fn subagent_statusline_emits_row_per_task() {
    let env = Env::new();
    let now_ms = 1_000_000_000i64;
    let start_ms = now_ms - 10_000; // 10s ago
    let payload = format!(
        r#"{{"columns":120,"tasks":[
          {{"id":"t1","model":"Sonnet 5","name":"local","description":"Implement Task 1","tokenCount":3000,"contextWindowSize":100000,"start":{start_ms}}}
        ]}}"#
    );
    env.cmd()
        .env("NO_COLOR","1")
        .env("WS_SUBAGENT_NOW_MS", now_ms.to_string())
        .arg("subagent-statusline")
        .write_stdin(payload)
        .assert()
        .success()
        .stdout(predicates::str::contains("\"id\":\"t1\""))
        .stdout(predicates::str::contains("Sonnet 5"))
        .stdout(predicates::str::contains("Implement Task 1"))
        .stdout(predicates::str::contains("ctx 3%"))
        .stdout(predicates::str::contains("0m10s"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test statusline subagent`
Expected: FAIL — variant missing.

- [ ] **Step 3: Add CLI variant + implementation**

In `src/cli.rs` add `SubagentStatusline,` to `Cmd` and the parse arm:
```rust
        "subagent-statusline" => Ok(Cmd::SubagentStatusline),
```
In `src/statusline.rs` add:
```rust
#[derive(Debug, Default, Deserialize)]
pub struct SubagentInput {
    #[serde(default)]
    pub tasks: Vec<Task>,
}
#[derive(Debug, Default, Deserialize)]
#[allow(non_snake_case)]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tokenCount: i64,
    #[serde(default)]
    pub contextWindowSize: i64,
    #[serde(default)]
    pub start: i64,
}

fn elapsed(start_ms: i64, now_ms: i64) -> String {
    if start_ms <= 0 || now_ms <= start_ms {
        return "0m0s".to_string();
    }
    let secs = (now_ms - start_ms) / 1000;
    format!("{}m{}s", secs / 60, secs % 60)
}

pub fn subagent_row(t: &Task, now_ms: i64) -> String {
    let name = if !t.name.is_empty() { &t.name } else { &t.type_ };
    let ctx = if t.contextWindowSize > 0 {
        (t.tokenCount * 100 / t.contextWindowSize) as i64
    } else {
        0
    };
    format!(
        "\u{21b7} {}  {} \u{b7} {} \u{b7} ctx {}% \u{b7} {}",
        t.model,
        name,
        t.description,
        ctx,
        elapsed(t.start, now_ms)
    )
}

pub fn run_subagent() {
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let input: SubagentInput = serde_json::from_str(&raw).unwrap_or_default();
    let now_ms = std::env::var("WS_SUBAGENT_NOW_MS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| limits::now_epoch() * 1000);
    for t in &input.tasks {
        let row = serde_json::json!({ "id": t.id, "content": subagent_row(t, now_ms) });
        println!("{row}");
    }
}
```
Route in `main.rs`:
```rust
        Cmd::SubagentStatusline => statusline::run_subagent(),
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test statusline`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/statusline.rs src/cli.rs src/main.rs tests/statusline.rs
git commit -m "feat: ws subagent-statusline (one row per running subagent)"
```

---

### Task 4: `ws -limits`

**Files:**
- Modify: `src/cli.rs` (`Cmd::Limits`), `src/commands.rs` (`limits()`), `src/main.rs` (route)
- Test: `tests/limits.rs`

**Interfaces:**
- Consumes: `registry::all`, `limits::{read, countdown, now_epoch}`, `workspace`.
- Produces: `commands::limits() -> anyhow::Result<()>`.
- Behavior: for each registered workspace with a `.ws/local/limits.json`, print `name  5h <n>% (resets in <cd>)  wk <n>% (resets in <cd>)`; if none, print the global snapshot if present, else "no limit data yet (run a ws session so the statusline can sense them)".

- [ ] **Step 1: Write the failing test**

`tests/limits.rs`:
```rust
mod common;
use common::Env;

#[test]
fn limits_lists_captured_windows() {
    let env = Env::new();
    let proj = env.home.path().join("lw");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","lw"]).assert().success();

    // Feed the statusline once to capture limits.
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":88.0,"resets_at":9999999999},"seven_day":{"used_percentage":40.0,"resets_at":9999999999}},"workspace":{"current_dir":"x"}}"#;
    env.cmd().env("WS_WORKSPACE","lw").env("WS_DIR",&proj).env("NO_COLOR","1")
        .arg("statusline").write_stdin(sample).assert().success();

    env.cmd()
        .arg("-limits")
        .assert()
        .success()
        .stdout(predicates::str::contains("lw"))
        .stdout(predicates::str::contains("5h 88%"))
        .stdout(predicates::str::contains("wk 40%"));
}

#[test]
fn limits_empty_message() {
    let env = Env::new();
    env.cmd().arg("-limits").assert().success()
        .stdout(predicates::str::contains("no limit data"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test limits`
Expected: FAIL — `-limits` unhandled.

- [ ] **Step 3: Add CLI variant + command**

In `src/cli.rs`, in the leading-dash section (next to `-list`/`-adopt`), add:
```rust
        "-limits" => Ok(Cmd::Limits),
```
and add `Limits,` to the `Cmd` enum.

Add to `src/commands.rs`:
```rust
use crate::limits;

pub fn limits() -> Result<()> {
    let now = limits::now_epoch();
    let mut shown = 0;
    for (name, path) in crate::registry::all() {
        let lp = path.join(".ws/local/limits.json");
        if let Some(snap) = limits::read(&lp) {
            println!(
                "{name}\t5h {}% (resets in {})\twk {}% (resets in {})",
                snap.five_hour.used_pct.round() as i64,
                limits::countdown(snap.five_hour.resets_at, now),
                snap.seven_day.used_pct.round() as i64,
                limits::countdown(snap.seven_day.resets_at, now),
            );
            shown += 1;
        }
    }
    if shown == 0 {
        if let Some(snap) = limits::read(&limits::global_path()) {
            println!(
                "(global)\t5h {}% (resets in {})\twk {}% (resets in {})",
                snap.five_hour.used_pct.round() as i64,
                limits::countdown(snap.five_hour.resets_at, now),
                snap.seven_day.used_pct.round() as i64,
                limits::countdown(snap.seven_day.resets_at, now),
            );
        } else {
            println!("no limit data yet (run a ws session so the statusline can sense them)");
        }
    }
    Ok(())
}
```
Route in `main.rs`: `Cmd::Limits => commands::limits()?,`.

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test limits`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/commands.rs src/main.rs tests/limits.rs
git commit -m "feat: ws -limits (per-workspace windows + reset countdowns)"
```

---

### Task 5: Config `limit_action` + guard helpers

**Files:**
- Modify: `src/config.rs` (add `limit_action` field + list/get/set)
- Modify: `src/workspace.rs` (add `limit_guard()` path helper)
- Test: unit tests in `config.rs` (extend), `workspace.rs`

**Interfaces:**
- Produces: `Config.limit_action: String` (default `"handoff-stop"`); `Workspace::limit_guard() -> PathBuf` (= `local_dir()/limit-guard`).

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs` tests:
```rust
    #[test]
    fn limit_action_default_and_set() {
        assert_eq!(Config::default().limit_action, "handoff-stop");
        assert!(list(&Config::default()).iter().any(|(k,_)| k == "limit_action"));
    }
```
Add to `src/workspace.rs` tests (inside the existing `mod tests`):
```rust
    #[test]
    fn limit_guard_path() {
        let (_d, cfg) = iso_cfg();
        let (ws, _) = open_or_create("g", "claude", &cfg).unwrap();
        assert_eq!(ws.limit_guard(), ws.root.join(".ws/local/limit-guard"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test limit_action; cargo test limit_guard`
Expected: FAIL.

- [ ] **Step 3: Add the field + helper**

In `src/config.rs`: add the struct field `pub limit_action: String,`; in `Default` set `limit_action: "handoff-stop".into(),`; in `list` add `("limit_action".into(), cfg.limit_action.clone())`; in `set`'s match add `"limit_action" => cfg.limit_action = value.to_string(),`.

In `src/workspace.rs` `impl Workspace` add:
```rust
    pub fn limit_guard(&self) -> PathBuf {
        self.local_dir().join("limit-guard")
    }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS (config integration `defaults_listed` still passes — it checks specific keys, an extra key is fine).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/workspace.rs
git commit -m "feat: config limit_action + workspace limit_guard path"
```

---

### Task 6: Threshold warning hook (Stop handler extension)

**Files:**
- Modify: `src/internal.rs` (extend `stop()` with a limit check, before the notebook reminder)
- Test: extend `tests/limits.rs`

**Interfaces:**
- Consumes: `limits`, `config`, `Workspace::{limit_guard, local_dir}`.
- Behavior added at the TOP of `stop()` (after resolving `ws`, before the notebook-reminder logic): read `<ws>/.ws/local/limits.json`; if a window is over threshold:
  - if the guard marker is absent → write it, fire a best-effort macOS notification (`osascript`), and (when `limit_action != "warn"`) emit a `decision:block` with the handoff directive and RETURN;
  - if `limit_action == "warn"` → don't block on the limit (fall through to the notebook reminder) but still write the guard + notify once.
  - if a window is NOT over threshold but the guard exists → remove the guard (a reset happened).
  Then continue to the existing notebook-reminder logic.

- [ ] **Step 1: Write the failing test**

Add to `tests/limits.rs`:
```rust
#[test]
fn stop_blocks_with_handoff_directive_when_over_threshold() {
    let env = Env::new();
    let proj = env.home.path().join("hs");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","hs"]).assert().success();

    // Capture a 5h at 90% (over default 85).
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":90.0,"resets_at":9999999999},"seven_day":{"used_percentage":10.0,"resets_at":9999999999}}}"#;
    env.cmd().env("WS_WORKSPACE","hs").env("WS_DIR",&proj).env("NO_COLOR","1")
        .arg("statusline").write_stdin(sample).assert().success();

    // Stop now blocks with a handoff directive + sets the guard.
    env.cmd().env("WS_WORKSPACE","hs").env("WS_DIR",&proj)
        .args(["internal","stop"]).write_stdin("{}")
        .assert().success()
        .stdout(predicates::str::contains("\"decision\":\"block\""))
        .stdout(predicates::str::contains("handoff"));
    assert!(proj.join(".ws/local/limit-guard").exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test limits stop_blocks`
Expected: FAIL — stop still hits the notebook-reminder cooldown path (approves), no guard.

- [ ] **Step 3: Extend stop()**

In `src/internal.rs`, in `stop()`, immediately after the `let ws = match current_ws() { … }` block and the `let _ = hookio::read_stdin();` line, insert the limit check:
```rust
    // Limit-aware handoff: check before the notebook reminder.
    if let Some(directive) = limit_check(&ws) {
        println!("{}", hookio::decision_block(&directive));
        return;
    }
```
Then add these functions to `src/internal.rs`:
```rust
use crate::limits;

/// Returns Some(directive) when the Stop hook should block for a limit handoff.
/// Also manages the guard marker (write on first cross; clear on reset) and a
/// best-effort desktop notification. Returns None to fall through to the
/// notebook reminder (including in "warn" mode).
fn limit_check(ws: &crate::workspace::Workspace) -> Option<String> {
    let cfg = crate::config::load();
    let snap = limits::read(&ws.local_dir().join("limits.json"))?;
    let guard = ws.limit_guard();

    match limits::over_threshold(&cfg_snapshot_thresholds(&snap, &cfg)) {
        None => {
            // reset (or never crossed) → clear a stale guard
            let _ = std::fs::remove_file(&guard);
            None
        }
        Some(window) => {
            let first_time = !guard.exists();
            if first_time {
                let _ = std::fs::create_dir_all(ws.local_dir());
                let _ = std::fs::write(&guard, crate::now_iso());
                notify(&format!(
                    "ws: Claude {window} limit high in {} — work is being saved.",
                    ws.name
                ));
            }
            if cfg.limit_action == "warn" {
                return None; // warn-only: don't block
            }
            if !first_time {
                // already handed off once; let turns end normally afterwards
                return None;
            }
            Some(format!(
                "Rate-limit guard: the Claude {window} window is high. Finish only the \
                 current step — start no new work — update your notebook \
                 (.ws/notebook/notebook.<actor>.md), write a handoff to .ws/handoffs/, then \
                 stop and tell the user: work is saved, continue in a fresh window later or \
                 with another agent."
            ))
        }
    }
}

fn cfg_snapshot_thresholds<'a>(
    snap: &'a limits::LimitsSnapshot,
    cfg: &crate::config::Config,
) -> (&'a limits::LimitsSnapshot, u8, u8) {
    (snap, cfg.limit_warn_5h, cfg.limit_warn_week)
}

fn notify(msg: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification {:?} with title \"ws\"", msg);
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = msg;
    }
}
```
NOTE: `limits::over_threshold` takes `(&LimitsSnapshot, u8, u8)`. Adjust the call in `limit_check` to pass them directly rather than via the tuple helper if you prefer — the intent is `limits::over_threshold(&snap, cfg.limit_warn_5h, cfg.limit_warn_week)`. Replace the `match limits::over_threshold(&cfg_snapshot_thresholds(&snap, &cfg))` line with:
```rust
    match limits::over_threshold(&snap, cfg.limit_warn_5h, cfg.limit_warn_week) {
```
and delete the `cfg_snapshot_thresholds` helper (it exists only to make the intent explicit here — do not ship it).

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test limits; cargo test`
Expected: PASS. The Phase-2 `stop_reminds_then_cools_down`/`stop_approves_outside_workspace` tests still pass (no limits.json in those workspaces → `limit_check` returns None → falls through to the reminder).

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs tests/limits.rs
git commit -m "feat: limit-threshold handoff directive in Stop hook (+guard, notify)"
```

---

### Task 7: Guard-active notice on UserPromptSubmit

**Files:**
- Modify: `src/internal.rs` (extend `user_prompt()`)
- Test: extend `tests/limits.rs`

**Interfaces:**
- Behavior: after the objective capture, if the limit guard marker exists, emit a UserPromptSubmit `additionalContext` one-liner telling the agent/user the limit guard is active (so continued work is a conscious choice). Otherwise keep the Phase-2 no-stdout behavior.

- [ ] **Step 1: Write the failing test**

Add to `tests/limits.rs`:
```rust
#[test]
fn user_prompt_notes_active_limit_guard() {
    let env = Env::new();
    let proj = env.home.path().join("gd");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","gd"]).assert().success();

    // Manually set the guard marker.
    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(proj.join(".ws/local/limit-guard"), "x").unwrap();

    env.cmd().env("WS_WORKSPACE","gd").env("WS_DIR",&proj)
        .args(["internal","user-prompt"])
        .write_stdin(r#"{"prompt":"keep going"}"#)
        .assert().success()
        .stdout(predicates::str::contains("limit guard"))
        .stdout(predicates::str::contains("hookSpecificOutput"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test limits user_prompt_notes`
Expected: FAIL — user_prompt emits nothing.

- [ ] **Step 3: Extend user_prompt()**

In `src/internal.rs`, at the END of `user_prompt()` (after the `let _ = readme::capture_objective(...)` line), add:
```rust
    if ws.limit_guard().exists() {
        let notice = "Note: the ws rate-limit guard is active (a handoff was already saved). \
            Continuing spends more of the current budget — that's fine, but it's your call.";
        println!("{}", hookio::additional_context("UserPromptSubmit", notice));
    }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test limits; cargo test`
Expected: PASS. The Phase-2 `user_prompt_captures_objective` test (no guard marker) still asserts empty stdout — unaffected.

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs tests/limits.rs
git commit -m "feat: UserPromptSubmit notice while limit guard is active"
```

---

### Task 8: `ws setup` registers the statuslines (with backup)

**Files:**
- Modify: `src/hooksetup.rs` (add `register_statuslines`)
- Modify: `src/commands.rs` (`setup()` also calls it)
- Test: `tests/statusline.rs` (extend) or `tests/setup.rs`

**Interfaces:**
- Consumes: `config::ws_config_dir`, the Phase-2 settings read/guard helpers.
- Produces:
  ```rust
  /// Register `ws statusline` + `ws subagent-statusline` in settings.json, recording
  /// any pre-existing command into <ws_config_dir>/statusline-backup.json first.
  /// Preserves all other settings.json keys; refuses to overwrite an unparseable file.
  pub fn register_statuslines(ws_bin: &std::path::Path) -> anyhow::Result<()>;
  ```
- settings.json shape written:
  ```json
  "statusLine": { "type": "command", "command": "<ws_bin> statusline", "refreshInterval": 1 },
  "subagentStatusLine": { "type": "command", "command": "<ws_bin> subagent-statusline" }
  ```

- [ ] **Step 1: Write the failing test**

Add to `tests/setup.rs`:
```rust
#[test]
fn setup_registers_statuslines_and_backs_up_prior() {
    let env = Env::new();
    // pre-existing foreign statusline (cs) must be backed up, not lost
    let sp = env.home.path().join(".claude/settings.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(&sp, r#"{"statusLine":{"type":"command","command":"/opt/cs/cs-statusline"}}"#).unwrap();

    env.cmd().arg("setup").assert().success();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    let cmd = settings["statusLine"]["command"].as_str().unwrap();
    assert!(cmd.ends_with(" statusline"), "statusLine should be ws, got {cmd}");
    assert!(settings["subagentStatusLine"]["command"].as_str().unwrap().ends_with(" subagent-statusline"));

    // the prior cs command was recorded to the backup file
    let backup = std::fs::read_to_string(
        env.home.path().join(".config/ws/statusline-backup.json")
    ).unwrap_or_default();
    assert!(backup.contains("cs-statusline"), "prior statusline must be backed up");
}
```
(Note: on macOS `dirs::config_dir()` differs, but `config::ws_config_dir()` honors `XDG_CONFIG_HOME`, which the `Env` helper sets to `<home>/.config` — so the backup lands at `.config/ws/statusline-backup.json`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test setup setup_registers_statuslines`
Expected: FAIL — statuslines not registered.

- [ ] **Step 3: Implement register_statuslines**

In `src/hooksetup.rs` add:
```rust
pub fn register_statuslines(ws_bin: &Path) -> Result<()> {
    let settings_path = claude_settings_path();
    let mut root: Value = match std::fs::read_to_string(&settings_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); refusing to overwrite it. \
                 Fix it or move it aside, then re-run `ws setup`.",
                settings_path.display()
            )
        })?,
        Err(_) => json!({}),
    };
    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object; refusing to overwrite it.", settings_path.display());
    }

    // back up any prior commands (so cs-statusline is recoverable)
    let mut backup = serde_json::Map::new();
    for key in ["statusLine", "subagentStatusLine"] {
        if let Some(cmd) = root.get(key).and_then(|v| v.get("command")).and_then(|c| c.as_str()) {
            let bin = ws_bin.to_string_lossy();
            if !cmd.starts_with(bin.as_ref()) {
                backup.insert(key.to_string(), json!(cmd));
            }
        }
    }
    if !backup.is_empty() {
        let bpath = crate::config::ws_config_dir().join("statusline-backup.json");
        if let Some(dir) = bpath.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&bpath, serde_json::to_string_pretty(&Value::Object(backup))?)?;
    }

    let obj = root.as_object_mut().unwrap();
    obj.insert(
        "statusLine".into(),
        json!({ "type": "command", "command": format!("{} statusline", ws_bin.display()), "refreshInterval": 1 }),
    );
    obj.insert(
        "subagentStatusLine".into(),
        json!({ "type": "command", "command": format!("{} subagent-statusline", ws_bin.display()) }),
    );

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}
```

In `src/commands.rs` `setup()`, after `prompts::install()?`, add:
```rust
    crate::hooksetup::register_statuslines(&ws_bin)?;
```
and extend the printed summary line, e.g.:
```rust
    println!("            registered ws statusline + subagent-statusline");
```

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test setup; cargo test`
Expected: PASS.

- [ ] **Step 5: Manual smoke (isolated HOME)**

```bash
. "$HOME/.cargo/env"; cargo build --release
H=$(mktemp -d); C=$(mktemp -d)
printf '{"context_window":{"used_percentage":12},"rate_limits":{"five_hour":{"used_percentage":73,"resets_at":9999999999}},"cost":{"total_cost_usd":0.5}}' \
  | HOME=$H XDG_CONFIG_HOME=$C WS_WORKSPACE=demo WS_DIR=$H ./target/release/ws statusline
HOME=$H XDG_CONFIG_HOME=$C ./target/release/ws setup
```
Expected: the statusline prints one line (`ctx 12% · 5h 73% (resets in …) · $0.50`); setup reports hooks + prompts + statusline registration.

- [ ] **Step 6: Commit**

```bash
git add src/hooksetup.rs src/commands.rs tests/setup.rs
git commit -m "feat: ws setup registers ws statusline + subagent-statusline (backs up prior)"
```

---

## Self-Review

**1. Spec coverage (§17.3 + §10 + user requests):**
- `ws statusline` (sense + show) — Tasks 1, 2 ✓
- `limits.json` capture (workspace + global) — Task 2 ✓
- `ws -limits` — Task 4 ✓
- threshold warning hook (save-and-stop directive + guard + notify) — Task 6 ✓; guard-active notice — Task 7 ✓; `limit_action` config — Task 5 ✓
- statusline content per user (branch · workspace · ctx · 5h+reset · cost) — Task 2 ✓
- subagent surfacing (user request; model · name · task · ctx · elapsed, one row per subagent) — Task 3 ✓
- statusline registration with cs backup (user decision) — Task 8 ✓

**2. Placeholder scan:** every step has complete code. The one editing subtlety (the `cfg_snapshot_thresholds` helper in Task 6) is called out explicitly with the exact final form to use.

**3. Type consistency:** `limits::{LimitsSnapshot, Window, over_threshold(&snap,u8,u8), countdown(i64,i64), read, write, global_path, now_epoch}` (Task 1) are used with matching signatures in Tasks 2, 4, 6. `statusline::to_snapshot`/`render`/`run` (Task 2) and `run_subagent` (Task 3) match their `main.rs` routes. `internal::current_ws()` and `Workspace::{local_dir, limit_guard}` are the Phase-1/Task-5 APIs. `Cmd::{Statusline, SubagentStatusline, Limits}` (Tasks 2–4) are routed where added.

**Deferred (correctly out of Phase 3):** the agent-switch flow (`ws <name> --agent codex`) and its guard-clear-on-switch (Phase 4); Codex/Gemini limit sensors (best-effort, later phases); the TUI limit columns and the TUI subagent panel (Phase 7); pinning the statusline to a `LIMIT · ws <name> --agent codex` call-to-action (needs the switch flow — Phase 4).

**Known simplifications (intentional):** `render` shells out to `git` once per refresh (fast, `--no-optional-locks`); the weekly window is captured and shown by `-limits` but omitted from the one-line render per the user's content spec; the guard clears on a sensed reset (5h/wk back under threshold) rather than tracking `resets_at` crossings precisely — adequate for a human-scale guard.
