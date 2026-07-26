# ws Phase 1 (Core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a working `ws` binary that can create, resume, list, adopt, and remove workspaces and launch Claude Code inside one — usable as a daily driver for Claude.

**Architecture:** One Rust binary. A `Config` (global TOML + per-workspace override) resolves a `sessions_root`; a name resolves to a workspace directory via a registry file (so adopted-in-place dirs work alongside created ones). Each workspace is a git repo holding a `.ws/` contract. Launching = resolve/create → lock → regenerate the agent context file → resolve/record a Claude session id in `state.toml` → build a `std::process::Command` via the Claude adapter → set the terminal tab title/color → `exec`. The adapter builds the command purely (testable via `Command::get_args`/`get_envs`); an integration test drives a fake `claude` shim through the whole flow.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `toml`, `anyhow`, `dirs`, `uuid` (v4). Git via the system `git` (shell-out). Dev: `assert_cmd`, `predicates`, `tempfile`.

## Global Constraints

- **Rust 2021, single crate, no workspace.** Binary name `ws`.
- **Zero runtime deps beyond git and the agents.** No python/jq. Git operations shell out to system `git`.
- **cargo is not on the default PATH.** Every `cargo`/`rustc` invocation in this plan must be run in a shell that has sourced the toolchain: prefix with `. "$HOME/.cargo/env";` (e.g. `. "$HOME/.cargo/env"; cargo test`). This is assumed in every "Run:" line below.
- **Concept vocabulary:** "workspace" never "session"; metadata dir is `.ws/`; default root `~/.agent-workspaces` (overridable by config `sessions_root` and env `WS_ROOT`, `WS_ROOT` wins).
- **Contract version:** `contract_version = 1` in `workspace.toml`.
- **Silent by default.** No interactive prompt at launch (`prompt_on_launch` defaults `false`). Interactivity only for user-initiated destructive actions (`-rm` confirm) and only when stdin is a TTY; non-TTY requires `--force`.
- **Claude launch flags (verified against Claude Code 2.1.218):** fresh launch uses `--session-id <uuid>` (pre-seeds a valid v4 UUID); resume uses `--resume <uuid>`. Memory redirect env var is `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE` pointing at `.ws/memory/`.
- **Hooks/error rule (applies later phases):** never break the agent. In Phase 1 this only means launch failures print a clear `anyhow` message and exit non-zero.
- **Every workspace context file uses sentinel-marked managed blocks:** `<!-- ws:begin -->` … `<!-- ws:end -->` so user content around them survives regeneration.
- **Test seam:** the Claude adapter resolves its program name from env `WS_CLAUDE_BIN` if set, else `"claude"`. This lets integration tests substitute a fake shim without changing the CLI surface.

---

## File Structure

```
Cargo.toml
src/
├── main.rs          # entry: parse args, dispatch, map errors → exit code
├── cli.rs           # manual arg dispatcher → Cmd enum (matches `ws -list` single-dash surface)
├── config.rs        # Config struct, global load/get/set/list, sessions_root resolution
├── registry.rs      # name → path index (~/.config/ws/registry.toml)
├── actors.rs        # actor slug from git user.email (fallback whoami)
├── workspace.rs     # Workspace struct: path helpers, resolve, open_or_create
├── contract.rs      # .ws/ scaffolding, workspace.toml, git init, state.toml session-id helpers
├── context.rs       # embedded template → context file managed block
├── lock.rs          # PID+heartbeat lock: acquire/release/stale/force
├── term.rs          # terminal tab title + color (OSC), NO_COLOR/tty aware
├── commands.rs      # command implementations (list, adopt, rm, config, launch)
├── agents/
│   ├── mod.rs       # trait Agent, LaunchMode, LaunchCtx, for_id()
│   └── claude.rs    # ClaudeAgent adapter
└── assets/
    └── context-template.md   # embedded via include_str!
tests/
├── common/mod.rs    # test helpers: temp WS_ROOT, fake-claude shim writer
├── config.rs        # `ws config` integration
├── workspace.rs     # create/list/adopt/rm integration
└── launch.rs        # end-to-end launch through fake claude shim
```

Module responsibilities are single-purpose; `commands.rs` is the only place CLI verbs are wired to modules, keeping `main.rs`/`cli.rs` thin.

---

### Task 1: Project scaffold + CLI dispatcher skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/cli.rs`
- Create: `.gitignore` (repo root — for the `ws` source repo, `/target`)
- Test: `tests/common/mod.rs`, `tests/smoke.rs`

**Interfaces:**
- Produces: `cli::parse(args: Vec<String>) -> anyhow::Result<cli::Cmd>` where
  ```rust
  pub enum Cmd {
      Launch { name: String, agent: Option<String>, fresh: bool, force: bool },
      List,
      Adopt { name: Option<String> },
      Rm { names: Vec<String>, force: bool },
      Config(ConfigCmd),
      Version,
      Help,
  }
  pub enum ConfigCmd { List, Get(String), Set { key: String, value: String, workspace: bool } }
  ```
- Produces: binary `ws` whose `--version`/`-V` prints `ws <pkg-version>`.

- [ ] **Step 1: Write the failing test**

`tests/smoke.rs`:
```rust
use assert_cmd::Command;

#[test]
fn prints_version() {
    Command::cargo_bin("ws")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::starts_with("ws "));
}

#[test]
fn unknown_dash_command_errors() {
    Command::cargo_bin("ws")
        .unwrap()
        .arg("-nonsense")
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown command"));
}
```

`tests/common/mod.rs` (used by later tasks; create now so the module exists):
```rust
#![allow(dead_code)]
use std::path::PathBuf;
use tempfile::TempDir;

/// A temp HOME + WS_ROOT so tests never touch the real config/registry.
pub struct Env {
    pub home: TempDir,
    pub root: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let home = TempDir::new().unwrap();
        let root = home.path().join("agent-workspaces");
        std::fs::create_dir_all(&root).unwrap();
        Env { home, root }
    }

    /// Build a `ws` command with isolated HOME + WS_ROOT env.
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::cargo_bin("ws").unwrap();
        c.env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("WS_ROOT", &self.root)
            .env_remove("NO_COLOR");
        c
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test smoke`
Expected: FAIL — no `Cargo.toml`/binary yet (compile error or "no bin target").

- [ ] **Step 3: Write Cargo.toml**

`Cargo.toml`:
```toml
[package]
name = "ws"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ws"
path = "src/main.rs"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
dirs = "5"
uuid = { version = "1", features = ["v4"] }

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

- [ ] **Step 4: Write the CLI dispatcher**

`src/cli.rs`:
```rust
use anyhow::{bail, Result};

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Launch { name: String, agent: Option<String>, fresh: bool, force: bool },
    List,
    Adopt { name: Option<String> },
    Rm { names: Vec<String>, force: bool },
    Config(ConfigCmd),
    Version,
    Help,
}

#[derive(Debug, PartialEq)]
pub enum ConfigCmd {
    List,
    Get(String),
    Set { key: String, value: String, workspace: bool },
}

/// Parse argv (excluding the program name) into a Cmd.
/// Top-level pseudo-subcommands use a single leading dash (`-list`) to match
/// the spec's CLI surface; `config` is a bare-word subcommand.
pub fn parse(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter().peekable();
    let first = match it.next() {
        None => return Ok(Cmd::List), // no args → friendly list
        Some(a) => a,
    };

    match first.as_str() {
        "-V" | "--version" => Ok(Cmd::Version),
        "-h" | "--help" => Ok(Cmd::Help),
        "-list" | "-ls" => Ok(Cmd::List),
        "-adopt" => {
            let name = it.next();
            Ok(Cmd::Adopt { name })
        }
        "-rm" => {
            let mut names = Vec::new();
            let mut force = false;
            for a in it {
                match a.as_str() {
                    "--force" | "-f" => force = true,
                    _ => names.push(a),
                }
            }
            if names.is_empty() {
                bail!("usage: ws -rm <name>... [--force]");
            }
            Ok(Cmd::Rm { names, force })
        }
        "config" => parse_config(it.collect()),
        other if other.starts_with('-') => {
            bail!("unknown command: {other}\ntry: ws -list | ws -adopt | ws -rm | ws config | ws <name>");
        }
        name => {
            // launch: ws <name> [--agent X] [--fresh] [--force]
            let mut agent = None;
            let mut fresh = false;
            let mut force = false;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--agent" => agent = it.next(),
                    "--fresh" => fresh = true,
                    "--force" => force = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Launch { name: name.to_string(), agent, fresh, force })
        }
    }
}

fn parse_config(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    match it.next().as_deref() {
        None | Some("list") => Ok(Cmd::Config(ConfigCmd::List)),
        Some("get") => {
            let key = it.next().ok_or_else(|| anyhow::anyhow!("usage: ws config get <key>"))?;
            Ok(Cmd::Config(ConfigCmd::Get(key)))
        }
        Some("set") => {
            let mut workspace = false;
            let mut rest: Vec<String> = Vec::new();
            for a in it {
                if a == "--workspace" {
                    workspace = true;
                } else {
                    rest.push(a);
                }
            }
            if rest.len() != 2 {
                bail!("usage: ws config set [--workspace] <key> <value>");
            }
            Ok(Cmd::Config(ConfigCmd::Set {
                key: rest[0].clone(),
                value: rest[1].clone(),
                workspace,
            }))
        }
        Some(other) => bail!("unknown config subcommand: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &[&str]) -> Cmd {
        parse(s.iter().map(|x| x.to_string()).collect()).unwrap()
    }

    #[test]
    fn launch_defaults() {
        assert_eq!(
            p(&["mywork"]),
            Cmd::Launch { name: "mywork".into(), agent: None, fresh: false, force: false }
        );
    }

    #[test]
    fn launch_flags() {
        assert_eq!(
            p(&["mywork", "--agent", "claude", "--fresh", "--force"]),
            Cmd::Launch { name: "mywork".into(), agent: Some("claude".into()), fresh: true, force: true }
        );
    }

    #[test]
    fn list_aliases_and_empty() {
        assert_eq!(p(&["-list"]), Cmd::List);
        assert_eq!(p(&["-ls"]), Cmd::List);
        assert_eq!(p(&[]), Cmd::List);
    }

    #[test]
    fn rm_collects_names_and_force() {
        assert_eq!(
            p(&["-rm", "a", "b", "--force"]),
            Cmd::Rm { names: vec!["a".into(), "b".into()], force: true }
        );
    }

    #[test]
    fn config_set_workspace() {
        assert_eq!(
            p(&["config", "set", "--workspace", "default_agent", "codex"]),
            Cmd::Config(ConfigCmd::Set { key: "default_agent".into(), value: "codex".into(), workspace: true })
        );
    }

    #[test]
    fn unknown_dash() {
        assert!(parse(vec!["-nope".into()]).is_err());
    }
}
```

`src/main.rs`:
```rust
mod cli;

use cli::Cmd;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(args) {
        eprintln!("ws: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> anyhow::Result<()> {
    match cli::parse(args)? {
        Cmd::Version => println!("ws {}", env!("CARGO_PKG_VERSION")),
        Cmd::Help => print_help(),
        // Remaining variants are wired up in later tasks.
        other => anyhow::bail!("not yet implemented: {other:?}"),
    }
    Ok(())
}

fn print_help() {
    println!(
        "ws — agent workspace manager\n\n\
         ws <name>            create or resume a workspace (launch Claude)\n\
         ws -list | -ls       list workspaces\n\
         ws -adopt [<name>]   adopt the current directory\n\
         ws -rm <name>...     remove workspace(s)\n\
         ws config ...        get/set/list config\n\
         ws --version"
    );
}
```

`.gitignore` (repo root):
```
/target
```

- [ ] **Step 5: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test smoke && cargo test --lib cli`
Expected: PASS (both the two smoke tests and the `cli::tests` unit tests). Note: `main.rs` is a binary crate, so `cargo test --lib cli` may report no lib target — run `. "$HOME/.cargo/env"; cargo test` to execute the inline `#[cfg(test)]` module in `cli.rs` as part of the bin test target. Expected overall: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/cli.rs .gitignore tests/
git commit -m "feat: project scaffold + CLI dispatcher"
```

---

### Task 2: Config module + `ws config`

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (declare `mod config;`, wire `Cmd::Config`)
- Create: `src/commands.rs` (add `config` handler; `mod commands;` in main)
- Test: `tests/config.rs`

**Interfaces:**
- Consumes: `cli::ConfigCmd` (Task 1).
- Produces:
  ```rust
  // config.rs
  pub struct Config {
      pub default_agent: String,     // "claude"
      pub prompt_on_launch: bool,    // false
      pub limit_warn_5h: u8,         // 85
      pub limit_warn_week: u8,       // 90
      pub theme: String,             // "auto"
      pub statusline: bool,          // true
      pub nerd_fonts: bool,          // false
      pub sessions_root: String,     // "~/.agent-workspaces"
  }
  pub fn config_path() -> std::path::PathBuf;               // <config>/ws/config.toml
  pub fn load() -> Config;                                   // defaults <- global file
  pub fn get(cfg: &Config, key: &str) -> anyhow::Result<String>;
  pub fn set(key: &str, value: &str) -> anyhow::Result<()>;  // persists global file
  pub fn list(cfg: &Config) -> Vec<(String, String)>;
  pub fn sessions_root(cfg: &Config) -> std::path::PathBuf;   // WS_ROOT > cfg > default, ~ expanded
  ```

- [ ] **Step 1: Write the failing test**

`tests/config.rs`:
```rust
mod common;
use common::Env;

#[test]
fn defaults_listed() {
    let env = Env::new();
    env.cmd()
        .args(["config", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("default_agent = claude"))
        .stdout(predicates::str::contains("prompt_on_launch = false"));
}

#[test]
fn set_then_get_roundtrips() {
    let env = Env::new();
    env.cmd().args(["config", "set", "default_agent", "codex"]).assert().success();
    env.cmd()
        .args(["config", "get", "default_agent"])
        .assert()
        .success()
        .stdout(predicates::str::diff("codex\n"));
}

#[test]
fn unknown_key_errors() {
    let env = Env::new();
    env.cmd().args(["config", "get", "bogus"]).assert().failure();
}
```

Also add unit tests inside `config.rs` (Step 3 includes them).

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test config`
Expected: FAIL — `config list` currently hits the "not yet implemented" arm.

- [ ] **Step 3: Write config.rs**

`src/config.rs`:
```rust
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_agent: String,
    pub prompt_on_launch: bool,
    pub limit_warn_5h: u8,
    pub limit_warn_week: u8,
    pub theme: String,
    pub statusline: bool,
    pub nerd_fonts: bool,
    pub sessions_root: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_agent: "claude".into(),
            prompt_on_launch: false,
            limit_warn_5h: 85,
            limit_warn_week: 90,
            theme: "auto".into(),
            statusline: true,
            nerd_fonts: false,
            sessions_root: "~/.agent-workspaces".into(),
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("ws")
        .join("config.toml")
}

pub fn load() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn list(cfg: &Config) -> Vec<(String, String)> {
    vec![
        ("default_agent".into(), cfg.default_agent.clone()),
        ("prompt_on_launch".into(), cfg.prompt_on_launch.to_string()),
        ("limit_warn_5h".into(), cfg.limit_warn_5h.to_string()),
        ("limit_warn_week".into(), cfg.limit_warn_week.to_string()),
        ("theme".into(), cfg.theme.clone()),
        ("statusline".into(), cfg.statusline.to_string()),
        ("nerd_fonts".into(), cfg.nerd_fonts.to_string()),
        ("sessions_root".into(), cfg.sessions_root.clone()),
    ]
}

pub fn get(cfg: &Config, key: &str) -> Result<String> {
    list(cfg)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or_else(|| anyhow::anyhow!("unknown config key: {key}"))
}

pub fn set(key: &str, value: &str) -> Result<()> {
    let mut cfg = load();
    match key {
        "default_agent" => cfg.default_agent = value.to_string(),
        "prompt_on_launch" => cfg.prompt_on_launch = parse_bool(value)?,
        "limit_warn_5h" => cfg.limit_warn_5h = value.parse()?,
        "limit_warn_week" => cfg.limit_warn_week = value.parse()?,
        "theme" => cfg.theme = value.to_string(),
        "statusline" => cfg.statusline = parse_bool(value)?,
        "nerd_fonts" => cfg.nerd_fonts = parse_bool(value)?,
        "sessions_root" => cfg.sessions_root = value.to_string(),
        other => bail!("unknown config key: {other}"),
    }
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string_pretty(&cfg)?)?;
    Ok(())
}

fn parse_bool(v: &str) -> Result<bool> {
    match v {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => bail!("expected a boolean, got: {other}"),
    }
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

pub fn sessions_root(cfg: &Config) -> PathBuf {
    if let Ok(env) = std::env::var("WS_ROOT") {
        if !env.is_empty() {
            return expand_tilde(&env);
        }
    }
    expand_tilde(&cfg.sessions_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_spec_values() {
        let c = Config::default();
        assert_eq!(c.default_agent, "claude");
        assert!(!c.prompt_on_launch);
        assert_eq!(c.limit_warn_5h, 85);
        assert_eq!(c.limit_warn_week, 90);
    }

    #[test]
    fn get_unknown_is_err() {
        assert!(get(&Config::default(), "nope").is_err());
    }

    #[test]
    fn ws_root_env_wins() {
        std::env::set_var("WS_ROOT", "/tmp/ws-test-root");
        let r = sessions_root(&Config::default());
        std::env::remove_var("WS_ROOT");
        assert_eq!(r, PathBuf::from("/tmp/ws-test-root"));
    }
}
```

- [ ] **Step 4: Wire into main + commands**

`src/commands.rs`:
```rust
use crate::cli::ConfigCmd;
use crate::config;
use anyhow::Result;

pub fn config(cmd: ConfigCmd) -> Result<()> {
    let cfg = config::load();
    match cmd {
        ConfigCmd::List => {
            for (k, v) in config::list(&cfg) {
                println!("{k} = {v}");
            }
        }
        ConfigCmd::Get(key) => {
            println!("{}", config::get(&cfg, &key)?);
        }
        ConfigCmd::Set { key, value, workspace } => {
            if workspace {
                anyhow::bail!("per-workspace config is added in a later task");
            }
            config::set(&key, &value)?;
        }
    }
    Ok(())
}
```

In `src/main.rs`, add module declarations and wire the arm:
```rust
mod cli;
mod commands;
mod config;
```
and in `run`:
```rust
        Cmd::Config(c) => commands::config(c)?,
```
(Remove `Cmd::Config` from falling into the catch-all.)

- [ ] **Step 5: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test config && cargo test`
Expected: PASS (integration + all unit tests).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/commands.rs src/main.rs tests/config.rs
git commit -m "feat: config load/get/set/list + sessions_root resolution"
```

---

### Task 3: Registry (name → path index)

**Files:**
- Create: `src/registry.rs`
- Modify: `src/main.rs` (`mod registry;`)
- Test: unit tests inside `registry.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn registry_path() -> std::path::PathBuf;               // <config>/ws/registry.toml
  pub fn register(name: &str, path: &std::path::Path) -> anyhow::Result<()>;
  pub fn unregister(name: &str) -> anyhow::Result<()>;
  pub fn lookup(name: &str) -> Option<std::path::PathBuf>;
  pub fn all() -> Vec<(String, std::path::PathBuf)>;          // sorted by name
  ```

- [ ] **Step 1: Write the failing test**

Inside `src/registry.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Isolate the config dir for each test via XDG_CONFIG_HOME.
    fn iso() -> TempDir {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path());
        d
    }

    #[test]
    fn register_lookup_unregister() {
        let _d = iso();
        register("alpha", std::path::Path::new("/x/alpha")).unwrap();
        assert_eq!(lookup("alpha"), Some(std::path::PathBuf::from("/x/alpha")));
        assert!(all().iter().any(|(n, _)| n == "alpha"));
        unregister("alpha").unwrap();
        assert_eq!(lookup("alpha"), None);
    }
}
```

Note: these tests mutate a process-global env var, so run them single-threaded (see Step 4 run line).

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test registry`
Expected: FAIL — module functions don't exist yet.

- [ ] **Step 3: Write registry.rs**

`src/registry.rs`:
```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    workspaces: BTreeMap<String, String>,
}

pub fn registry_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("ws")
        .join("registry.toml")
}

fn load() -> Registry {
    match std::fs::read_to_string(registry_path()) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => Registry::default(),
    }
}

fn save(r: &Registry) -> Result<()> {
    let path = registry_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string_pretty(r)?)?;
    Ok(())
}

pub fn register(name: &str, path: &Path) -> Result<()> {
    let mut r = load();
    r.workspaces
        .insert(name.to_string(), path.to_string_lossy().to_string());
    save(&r)
}

pub fn unregister(name: &str) -> Result<()> {
    let mut r = load();
    r.workspaces.remove(name);
    save(&r)
}

pub fn lookup(name: &str) -> Option<PathBuf> {
    load().workspaces.get(name).map(PathBuf::from)
}

pub fn all() -> Vec<(String, PathBuf)> {
    load()
        .workspaces
        .into_iter()
        .map(|(n, p)| (n, PathBuf::from(p)))
        .collect()
}
```

Add `mod registry;` to `src/main.rs`.

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test registry -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/registry.rs src/main.rs
git commit -m "feat: workspace registry index"
```

---

### Task 4: Actor slug + contract scaffolding

**Files:**
- Create: `src/actors.rs`
- Create: `src/contract.rs`
- Modify: `src/main.rs` (`mod actors; mod contract;`)
- Test: unit tests inside both modules

**Interfaces:**
- Consumes: `registry::register` (Task 3).
- Produces:
  ```rust
  // actors.rs
  pub fn actor_slug() -> String;   // git user.email slugified; fallback whoami; else "unknown"

  // contract.rs
  pub const CONTRACT_VERSION: u32 = 1;
  /// Scaffold `.ws/` at `root`, write workspace.toml, git-init if needed, register.
  /// `commit` = make an initial git commit of the scaffolding (true for created,
  /// false for adopt-in-place so we never touch an existing project's history).
  pub fn init(name: &str, root: &std::path::Path, agent: &str, commit: bool) -> anyhow::Result<()>;
  /// state.toml session-id helpers (per agent id).
  pub fn read_session_id(state_toml: &std::path::Path, agent: &str) -> Option<String>;
  pub fn write_session_id(state_toml: &std::path::Path, agent: &str, id: &str) -> anyhow::Result<()>;
  ```

- [ ] **Step 1: Write the failing tests**

In `src/actors.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slug_is_nonempty_and_lowercase() {
        let s = actor_slug();
        assert!(!s.is_empty());
        assert_eq!(s, s.to_lowercase());
        assert!(!s.contains(' '));
    }
    #[test]
    fn slugify_rules() {
        assert_eq!(slugify("Im.Ionut@Gmail.com"), "im-ionut-gmail-com");
        assert_eq!(slugify("a__b"), "a-b");
    }
}
```

In `src/contract.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_creates_layout() {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        let root = d.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        init("proj", &root, "claude", true).unwrap();

        assert!(root.join(".ws/workspace.toml").is_file());
        assert!(root.join(".ws/README.md").is_file());
        assert!(root.join(".ws/notebook/NOTES.md").is_file());
        assert!(root.join(".ws/memory").is_dir());
        assert!(root.join(".ws/handoffs").is_dir());
        assert!(root.join(".ws/local").is_dir());
        assert!(root.join(".ws/.gitignore").is_file());
        assert!(root.join(".git").is_dir());

        let toml = std::fs::read_to_string(root.join(".ws/workspace.toml")).unwrap();
        assert!(toml.contains("name = \"proj\""));
        assert!(toml.contains("contract_version = 1"));
    }

    #[test]
    fn session_id_roundtrip() {
        let d = TempDir::new().unwrap();
        let state = d.path().join("state.toml");
        assert_eq!(read_session_id(&state, "claude"), None);
        write_session_id(&state, "claude", "abc-123").unwrap();
        assert_eq!(read_session_id(&state, "claude"), Some("abc-123".into()));
        // second agent doesn't clobber the first
        write_session_id(&state, "codex", "xyz").unwrap();
        assert_eq!(read_session_id(&state, "claude"), Some("abc-123".into()));
        assert_eq!(read_session_id(&state, "codex"), Some("xyz".into()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env"; cargo test actors && cargo test contract`
Expected: FAIL — functions undefined.

- [ ] **Step 3: Write actors.rs**

`src/actors.rs`:
```rust
use std::process::Command;

/// Slugify an identifier: lowercase, non-alphanumerics → '-', collapse repeats, trim.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn actor_slug() -> String {
    // Prefer git user.email.
    if let Ok(o) = Command::new("git").args(["config", "user.email"]).output() {
        if o.status.success() {
            let email = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !email.is_empty() {
                return slugify(&email);
            }
        }
    }
    // Fallback: $USER / whoami.
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() {
            return slugify(&u);
        }
    }
    "unknown".to_string()
}
```

- [ ] **Step 4: Write contract.rs**

`src/contract.rs`:
```rust
use anyhow::Result;
use std::path::Path;
use std::process::Command;

use crate::actors;
use crate::registry;

pub const CONTRACT_VERSION: u32 = 1;

pub fn init(name: &str, root: &Path, agent: &str, commit: bool) -> Result<()> {
    let ws = root.join(".ws");
    for sub in ["notebook", "memory", "handoffs", "plans", "local"] {
        std::fs::create_dir_all(ws.join(sub))?;
    }

    let now = crate::now_iso();
    let actor = actors::actor_slug();

    // workspace.toml (identity)
    let workspace_toml = format!(
        "name = \"{name}\"\n\
         created = \"{now}\"\n\
         contract_version = {CONTRACT_VERSION}\n\
         default_agent = \"{agent}\"\n\
         archived = false\n\
         tags = []\n"
    );
    write_if_absent(&ws.join("workspace.toml"), &workspace_toml)?;

    // README.md
    write_if_absent(
        &ws.join("README.md"),
        &format!("# {name}\n\n## Objective\n\n_(captured from the first prompt)_\n\n## Outcome\n\n"),
    )?;

    // notebook
    write_if_absent(
        &ws.join("notebook/NOTES.md"),
        "# Notebook index\n\nPer-actor lab notebooks live beside this file.\n",
    )?;
    write_if_absent(
        &ws.join(format!("notebook/notebook.{actor}.md")),
        &format!("# Notebook ({actor})\n\n"),
    )?;

    // memory keep-file (agent memory redirect target; keep dir under git)
    write_if_absent(&ws.join("memory/.gitkeep"), "")?;
    write_if_absent(&ws.join("handoffs/.gitkeep"), "")?;
    write_if_absent(&ws.join("plans/.gitkeep"), "")?;

    // .ws/.gitignore — never commit local/ or secrets
    write_if_absent(
        &ws.join(".gitignore"),
        "local/\n*.enc\n",
    )?;

    // .ws/.gitattributes — union-merge notebooks/timeline (used from Phase 2 on)
    write_if_absent(
        &ws.join(".gitattributes"),
        "notebook/*.md merge=union\ntimeline.jsonl merge=union\n",
    )?;

    // git init if this dir is not already inside a repo
    if !root.join(".git").exists() {
        run_git(root, &["init", "-q"])?;
    }
    if commit {
        run_git(root, &["add", "-A"])?;
        // Allow empty in case there is nothing new; ignore failure on "nothing to commit".
        let _ = run_git(root, &["commit", "-q", "-m", "chore: initialize ws workspace"]);
    }

    registry::register(name, root)?;
    Ok(())
}

fn write_if_absent(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").arg("-C").arg(root).args(args).status()?;
    if !status.success() {
        anyhow::bail!("git {:?} failed", args);
    }
    Ok(())
}

pub fn read_session_id(state_toml: &Path, agent: &str) -> Option<String> {
    let s = std::fs::read_to_string(state_toml).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get(agent)?.get("session_id")?.as_str().map(String::from)
}

pub fn write_session_id(state_toml: &Path, agent: &str, id: &str) -> Result<()> {
    let mut t: toml::Table = std::fs::read_to_string(state_toml)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    let mut entry = toml::Table::new();
    entry.insert("session_id".into(), toml::Value::String(id.to_string()));
    t.insert(agent.to_string(), toml::Value::Table(entry));
    if let Some(dir) = state_toml.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(state_toml, toml::to_string_pretty(&t)?)?;
    Ok(())
}
```

Add a shared timestamp helper in `src/main.rs` (used by contract and later timeline). Since we avoid `chrono` in Phase 1, format from `SystemTime` as epoch seconds is unfriendly; instead shell out to `date -u`:
```rust
/// ISO-8601 UTC timestamp, e.g. 2026-07-24T10:43:12Z. Shells out to `date`.
pub fn now_iso() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
```
Add `mod actors; mod contract;` to `main.rs`.

- [ ] **Step 5: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test actors && cargo test contract -- --test-threads=1`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/actors.rs src/contract.rs src/main.rs
git commit -m "feat: actor slug + .ws contract scaffolding"
```

---

### Task 5: Workspace resolve / open_or_create

**Files:**
- Create: `src/workspace.rs`
- Modify: `src/main.rs` (`mod workspace;`)
- Test: unit tests inside `workspace.rs`

**Interfaces:**
- Consumes: `config::sessions_root`, `registry::lookup`, `contract::init`.
- Produces:
  ```rust
  pub struct Workspace {
      pub name: String,
      pub root: std::path::PathBuf,   // agent cwd
  }
  impl Workspace {
      pub fn ws_dir(&self) -> PathBuf;       // root/.ws
      pub fn memory_dir(&self) -> PathBuf;   // root/.ws/memory
      pub fn local_dir(&self) -> PathBuf;    // root/.ws/local
      pub fn state_toml(&self) -> PathBuf;   // root/.ws/local/state.toml
      pub fn lock_file(&self) -> PathBuf;    // root/.ws/local/lock
      pub fn workspace_toml(&self) -> PathBuf;
      pub fn exists(&self) -> bool;          // .ws dir present
  }
  /// Resolve a name to a path: registry first, else <sessions_root>/<name>.
  pub fn resolve(name: &str, cfg: &config::Config) -> Workspace;
  /// Resolve; create the contract if the workspace doesn't exist yet.
  /// Returns (workspace, created_now).
  pub fn open_or_create(name: &str, agent: &str, cfg: &config::Config) -> anyhow::Result<(Workspace, bool)>;
  ```

- [ ] **Step 1: Write the failing test**

In `src/workspace.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::TempDir;

    fn iso_cfg() -> (TempDir, Config) {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("cfg"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let mut cfg = Config::default();
        cfg.sessions_root = d.path().join("root").to_string_lossy().to_string();
        (d, cfg)
    }

    #[test]
    fn create_then_resolve_via_registry() {
        let (_d, cfg) = iso_cfg();
        let (ws, created) = open_or_create("proj", "claude", &cfg).unwrap();
        assert!(created);
        assert!(ws.exists());
        assert_eq!(ws.root, resolve("proj", &cfg).root);

        // Second open does not recreate.
        let (_ws2, created2) = open_or_create("proj", "claude", &cfg).unwrap();
        assert!(!created2);
    }

    #[test]
    fn path_helpers() {
        let (_d, cfg) = iso_cfg();
        let (ws, _) = open_or_create("p", "claude", &cfg).unwrap();
        assert_eq!(ws.state_toml(), ws.root.join(".ws/local/state.toml"));
        assert_eq!(ws.memory_dir(), ws.root.join(".ws/memory"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test workspace -- --test-threads=1`
Expected: FAIL — type undefined.

- [ ] **Step 3: Write workspace.rs**

`src/workspace.rs`:
```rust
use anyhow::Result;
use std::path::PathBuf;

use crate::config::{self, Config};
use crate::contract;
use crate::registry;

pub struct Workspace {
    pub name: String,
    pub root: PathBuf,
}

impl Workspace {
    pub fn ws_dir(&self) -> PathBuf {
        self.root.join(".ws")
    }
    pub fn memory_dir(&self) -> PathBuf {
        self.ws_dir().join("memory")
    }
    pub fn local_dir(&self) -> PathBuf {
        self.ws_dir().join("local")
    }
    pub fn state_toml(&self) -> PathBuf {
        self.local_dir().join("state.toml")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.local_dir().join("lock")
    }
    pub fn workspace_toml(&self) -> PathBuf {
        self.ws_dir().join("workspace.toml")
    }
    pub fn exists(&self) -> bool {
        self.ws_dir().is_dir()
    }
}

pub fn resolve(name: &str, cfg: &Config) -> Workspace {
    let root = registry::lookup(name)
        .unwrap_or_else(|| config::sessions_root(cfg).join(name));
    Workspace {
        name: name.to_string(),
        root,
    }
}

pub fn open_or_create(name: &str, agent: &str, cfg: &Config) -> Result<(Workspace, bool)> {
    validate_name(name)?;
    let ws = resolve(name, cfg);
    if ws.exists() {
        return Ok((ws, false));
    }
    std::fs::create_dir_all(&ws.root)?;
    contract::init(name, &ws.root, agent, /* commit */ true)?;
    Ok((ws, true))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains("..")
        || name.starts_with('-')
    {
        anyhow::bail!("invalid workspace name: {name:?}");
    }
    Ok(())
}
```

Add `mod workspace;` to `main.rs`.

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test workspace -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs src/main.rs
git commit -m "feat: workspace resolve + open_or_create"
```

---

### Task 6: `ws -list`

**Files:**
- Modify: `src/commands.rs` (add `list`)
- Modify: `src/main.rs` (wire `Cmd::List`)
- Test: `tests/workspace.rs` (new integration file)

**Interfaces:**
- Consumes: `registry::all`, `config::load`.
- Produces: `commands::list() -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing test**

`tests/workspace.rs`:
```rust
mod common;
use common::Env;

#[test]
fn list_empty_says_none() {
    let env = Env::new();
    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("no workspaces"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace`
Expected: FAIL — `-list` hits "not yet implemented".

- [ ] **Step 3: Implement list**

Add to `src/commands.rs`:
```rust
use crate::registry;

pub fn list() -> Result<()> {
    let all = registry::all();
    if all.is_empty() {
        println!("no workspaces yet — create one with: ws <name>");
        return Ok(());
    }
    for (name, path) in all {
        let live = if path.join(".ws").is_dir() { "" } else { "  (missing)" };
        println!("{name}\t{}{live}", path.display());
    }
    Ok(())
}
```

Wire in `src/main.rs` `run`:
```rust
        Cmd::List => commands::list()?,
```

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs src/main.rs tests/workspace.rs
git commit -m "feat: ws -list"
```

---

### Task 7: `ws -adopt`

**Files:**
- Modify: `src/commands.rs` (add `adopt`)
- Modify: `src/main.rs` (wire `Cmd::Adopt`)
- Test: `tests/workspace.rs` (extend)

**Interfaces:**
- Consumes: `contract::init` (with `commit=false`), `registry`.
- Produces: `commands::adopt(name: Option<String>) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to `tests/workspace.rs`:
```rust
#[test]
fn adopt_current_dir() {
    let env = Env::new();
    let proj = env.home.path().join("myproj");
    std::fs::create_dir_all(&proj).unwrap();

    env.cmd()
        .current_dir(&proj)
        .arg("-adopt")
        .assert()
        .success()
        .stdout(predicates::str::contains("adopted"));

    assert!(proj.join(".ws/workspace.toml").is_file());

    // Now listed by name (dir basename).
    env.cmd()
        .arg("-list")
        .assert()
        .success()
        .stdout(predicates::str::contains("myproj"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace adopt_current_dir`
Expected: FAIL — adopt not implemented.

- [ ] **Step 3: Implement adopt**

Add to `src/commands.rs`:
```rust
use crate::config;
use crate::contract;

pub fn adopt(name: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let name = match name {
        Some(n) => n,
        None => cwd
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("cannot derive a workspace name from {}", cwd.display()))?,
    };
    if cwd.join(".ws").is_dir() {
        // Already a workspace: just (re)register.
        crate::registry::register(&name, &cwd)?;
        println!("re-registered existing workspace: {name}");
        return Ok(());
    }
    let cfg = config::load();
    let agent = cfg.default_agent.clone();
    contract::init(&name, &cwd, &agent, /* commit */ false)?;
    println!("adopted {name} at {}", cwd.display());
    Ok(())
}
```

Wire in `main.rs`:
```rust
        Cmd::Adopt { name } => commands::adopt(name)?,
```

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace`
Expected: PASS (all workspace integration tests).

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs src/main.rs tests/workspace.rs
git commit -m "feat: ws -adopt current directory"
```

---

### Task 8: `ws -rm`

**Files:**
- Modify: `src/commands.rs` (add `rm`)
- Modify: `src/main.rs` (wire `Cmd::Rm`)
- Test: `tests/workspace.rs` (extend)

**Interfaces:**
- Consumes: `registry`, `config::sessions_root`.
- Produces: `commands::rm(names: Vec<String>, force: bool) -> anyhow::Result<()>`.
- Safety rule: delete the workspace directory **only** when it lives under `sessions_root` (ws created it). For adopted-in-place dirs, remove only `.ws/` and unregister — never the user's project. Destructive: require `--force` when stdin is not a TTY; otherwise prompt `y/N`.

- [ ] **Step 1: Write the failing test**

Add to `tests/workspace.rs`:
```rust
#[test]
fn rm_created_workspace_deletes_dir() {
    let env = Env::new();
    // create under sessions_root
    let root_ws = env.root.join("throwaway");
    // create via launch path is heavy; instead adopt a dir *inside* sessions_root
    std::fs::create_dir_all(&root_ws).unwrap();
    env.cmd().current_dir(&root_ws).args(["-adopt", "throwaway"]).assert().success();
    assert!(root_ws.join(".ws").is_dir());

    env.cmd()
        .args(["-rm", "throwaway", "--force"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed throwaway"));

    // dir under sessions_root is gone; no longer listed
    assert!(!root_ws.exists());
    env.cmd().arg("-list").assert().stdout(predicates::str::contains("throwaway").not());
}

#[test]
fn rm_adopted_external_keeps_project() {
    let env = Env::new();
    let proj = env.home.path().join("external");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("keepme.txt"), "data").unwrap();
    env.cmd().current_dir(&proj).arg("-adopt").assert().success();

    env.cmd().args(["-rm", "external", "--force"]).assert().success();

    // project dir and file survive; only .ws removed + unregistered
    assert!(proj.join("keepme.txt").is_file());
    assert!(!proj.join(".ws").exists());
}
```

Add `use predicates::prelude::*;` at the top of `tests/workspace.rs` for `.not()`.

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace rm_`
Expected: FAIL — rm not implemented.

- [ ] **Step 3: Implement rm**

Add to `src/commands.rs`:
```rust
use std::io::IsTerminal;

pub fn rm(names: Vec<String>, force: bool) -> Result<()> {
    let cfg = config::load();
    let root = config::sessions_root(&cfg);
    for name in names {
        let path = match crate::registry::lookup(&name) {
            Some(p) => p,
            None => {
                eprintln!("ws: no such workspace: {name}");
                continue;
            }
        };
        if !force {
            if !std::io::stdin().is_terminal() {
                anyhow::bail!("refusing to remove {name} without --force (no TTY to confirm)");
            }
            eprint!("Remove workspace {name} at {}? [y/N] ", path.display());
            use std::io::Write;
            std::io::stderr().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if !matches!(line.trim(), "y" | "Y" | "yes") {
                println!("skipped {name}");
                continue;
            }
        }
        let under_root = path.starts_with(&root);
        if under_root {
            std::fs::remove_dir_all(&path).ok();
        } else {
            // adopted in place: remove only .ws, keep the project
            std::fs::remove_dir_all(path.join(".ws")).ok();
        }
        crate::registry::unregister(&name)?;
        println!("removed {name}");
    }
    Ok(())
}
```

Wire in `main.rs`:
```rust
        Cmd::Rm { names, force } => commands::rm(names, force)?,
```

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs src/main.rs tests/workspace.rs
git commit -m "feat: ws -rm with adopt-safe deletion"
```

---

### Task 9: Lock (PID + heartbeat)

**Files:**
- Create: `src/lock.rs`
- Modify: `src/main.rs` (`mod lock;`)
- Test: unit tests inside `lock.rs`

**Interfaces:**
- Consumes: `crate::now_iso`.
- Produces:
  ```rust
  pub struct LockGuard { path: std::path::PathBuf, released: bool }
  impl Drop for LockGuard { /* removes lock file unless already released */ }
  impl LockGuard { pub fn keep(self); }  // forget: leave the lock file in place (used before exec)
  /// Acquire the workspace lock. On a live collision, error naming the holder.
  /// `force` overrides any existing lock.
  pub fn acquire(lock_file: &std::path::Path, force: bool) -> anyhow::Result<LockGuard>;
  ```
- Behavior: lock file holds `pid`, `host`, `tty`, `started`. Stale = the recorded PID is not alive (`kill -0`) → reclaim silently. Live PID + not force → error.

- [ ] **Step 1: Write the failing test**

`src/lock.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_then_release_allows_reacquire() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        {
            let _g = acquire(&lf, false).unwrap();
            assert!(lf.exists());
        } // dropped → released
        assert!(!lf.exists());
        let _g2 = acquire(&lf, false).unwrap();
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        // PID 999999 is (essentially certainly) not running.
        std::fs::write(&lf, "pid = 999999\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n").unwrap();
        let _g = acquire(&lf, false).expect("stale lock should be reclaimed");
    }

    #[test]
    fn live_lock_blocks_without_force() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        let mypid = std::process::id();
        std::fs::write(&lf, format!("pid = {mypid}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();
        assert!(acquire(&lf, false).is_err());
        // force overrides
        let _g = acquire(&lf, true).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test lock`
Expected: FAIL — module missing.

- [ ] **Step 3: Write lock.rs**

`src/lock.rs`:
```rust
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct LockGuard {
    path: PathBuf,
    released: bool,
}

impl LockGuard {
    /// Leave the lock file in place (do not remove on drop). Used before `exec`,
    /// where the launched agent inherits this PID and holds the lock until exit.
    pub fn keep(mut self) {
        self.released = true; // suppress Drop removal, but leave file on disk
        std::mem::forget(self);
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    // POSIX: `kill -0 <pid>` succeeds iff the process exists and is signalable.
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn read_pid(lock_file: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(lock_file).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get("pid")?.as_integer().map(|n| n as u32)
}

pub fn acquire(lock_file: &Path, force: bool) -> Result<LockGuard> {
    if lock_file.exists() && !force {
        if let Some(pid) = read_pid(lock_file) {
            if pid != std::process::id() && pid_alive(pid) {
                bail!(
                    "workspace is in use by pid {pid} (another terminal). \
                     Close it or re-run with --force."
                );
            }
        }
        // else: stale (dead pid / unreadable) → fall through and reclaim
    }
    if let Some(dir) = lock_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let host = hostname();
    let tty = std::env::var("TTY").unwrap_or_else(|_| "?".into());
    let body = format!(
        "pid = {}\nhost = \"{}\"\ntty = \"{}\"\nstarted = \"{}\"\n",
        std::process::id(),
        host,
        tty,
        crate::now_iso(),
    );
    std::fs::write(lock_file, body)?;
    Ok(LockGuard {
        path: lock_file.to_path_buf(),
        released: false,
    })
}

fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
```

Add `mod lock;` to `main.rs`.

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test lock`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lock.rs src/main.rs
git commit -m "feat: PID+heartbeat workspace lock"
```

---

### Task 10: Context-file generation

**Files:**
- Create: `src/assets/context-template.md`
- Create: `src/context.rs`
- Modify: `src/main.rs` (`mod context;`)
- Test: unit tests inside `context.rs`

**Interfaces:**
- Produces:
  ```rust
  /// Render the embedded template into `path`, inside <!-- ws:begin -->/<!-- ws:end -->.
  /// Preserves any user content outside the managed block; replaces the block if present.
  pub fn regenerate(path: &std::path::Path, workspace_name: &str) -> anyhow::Result<()>;
  ```
- Sentinels: `<!-- ws:begin -->` and `<!-- ws:end -->` (exact).

- [ ] **Step 1: Write the failing test**

`src/context.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_file_with_managed_block() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("CLAUDE.local.md");
        regenerate(&f, "proj").unwrap();
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains(BEGIN));
        assert!(s.contains(END));
        assert!(s.contains("proj"));
    }

    #[test]
    fn preserves_user_content_and_replaces_block() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("CLAUDE.local.md");
        std::fs::write(
            &f,
            format!("# my notes\nkeep me\n{BEGIN}\nOLD MANAGED\n{END}\ntrailing user text\n"),
        )
        .unwrap();
        regenerate(&f, "proj").unwrap();
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("keep me"));
        assert!(s.contains("trailing user text"));
        assert!(!s.contains("OLD MANAGED"));
        // exactly one managed block
        assert_eq!(s.matches(BEGIN).count(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test context`
Expected: FAIL — module missing.

- [ ] **Step 3: Write the template + context.rs**

`src/assets/context-template.md`:
```markdown
# Workspace protocol (ws)

You are working in a **ws** workspace named `{{name}}`. Durable state lives in `.ws/`.

- On start, read `.ws/README.md` (objective) and the notebooks in `.ws/notebook/`.
- Append findings to your own notebook: `.ws/notebook/notebook.<actor>.md`.
- On rotate or agent switch, write a handoff to `.ws/handoffs/`.
- Store secrets via `ws -secrets` — never write credentials into files.
- Memory (Claude) is redirected to `.ws/memory/`.
```

`src/context.rs`:
```rust
use anyhow::Result;
use std::path::Path;

pub const BEGIN: &str = "<!-- ws:begin -->";
pub const END: &str = "<!-- ws:end -->";

const TEMPLATE: &str = include_str!("assets/context-template.md");

fn render(workspace_name: &str) -> String {
    let body = TEMPLATE.replace("{{name}}", workspace_name);
    format!("{BEGIN}\n{body}\n{END}\n")
}

pub fn regenerate(path: &Path, workspace_name: &str) -> Result<()> {
    let block = render(workspace_name);
    let new_contents = match std::fs::read_to_string(path) {
        Ok(existing) => splice(&existing, &block),
        Err(_) => block,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, new_contents)?;
    Ok(())
}

/// Replace the region between BEGIN..END (inclusive) with `block`, or append
/// `block` if no managed region exists.
fn splice(existing: &str, block: &str) -> String {
    if let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        if e >= b {
            let end_idx = e + END.len();
            let mut out = String::new();
            out.push_str(&existing[..b]);
            out.push_str(block.trim_end());
            out.push_str(&existing[end_idx..]);
            return out;
        }
    }
    // No block: append, separated by a blank line.
    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block);
    out
}
```

Add `mod context;` to `main.rs`.

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test context`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/assets/context-template.md src/context.rs src/main.rs
git commit -m "feat: context-file generation with managed blocks"
```

---

### Task 11: Agent trait + Claude adapter

**Files:**
- Create: `src/agents/mod.rs`
- Create: `src/agents/claude.rs`
- Modify: `src/main.rs` (`mod agents;`)
- Test: unit tests inside `agents/claude.rs`

**Interfaces:**
- Consumes: `contract::{read_session_id, write_session_id}`, `workspace::Workspace`.
- Produces:
  ```rust
  // agents/mod.rs
  #[derive(Clone, Copy, PartialEq, Debug)]
  pub enum LaunchMode { Fresh, Resume }

  pub struct LaunchCtx {
      pub session_id: String,
      pub mode: LaunchMode,
      pub sessions_root: std::path::PathBuf,
  }

  pub trait Agent {
      fn id(&self) -> &'static str;
      fn binary(&self) -> String;                 // WS_CLAUDE_BIN override else "claude"
      fn is_installed(&self) -> bool;
      fn context_file(&self) -> &'static str;     // "CLAUDE.local.md"
      fn conversation_id(&self, ws: &crate::workspace::Workspace) -> Option<String>;
      fn launch(&self, ws: &crate::workspace::Workspace, ctx: &LaunchCtx) -> std::process::Command;
  }

  pub fn for_id(id: &str) -> anyhow::Result<Box<dyn Agent>>;  // "claude" only in Phase 1
  ```
- Claude command shapes (verified vs Claude Code 2.1.218):
  - Fresh: `<bin> --session-id <uuid>`
  - Resume: `<bin> --resume <uuid>`
  - Env on both: `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE=<ws.memory_dir>`, `WS_WORKSPACE=<name>`, `WS_ROOT=<sessions_root>`.
  - cwd: `ws.root`.

- [ ] **Step 1: Write the failing test**

`src/agents/claude.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, LaunchCtx, LaunchMode};
    use crate::workspace::Workspace;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn ws() -> Workspace {
        Workspace { name: "proj".into(), root: PathBuf::from("/tmp/ws/proj") }
    }

    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    fn env_of<'a>(cmd: &'a std::process::Command, key: &str) -> Option<String> {
        cmd.get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().to_string())
    }

    #[test]
    fn fresh_uses_session_id_flag() {
        let a = ClaudeAgent;
        let ctx = LaunchCtx { session_id: "uuid-1".into(), mode: LaunchMode::Fresh, sessions_root: PathBuf::from("/root") };
        let cmd = a.launch(&ws(), &ctx);
        assert_eq!(args_of(&cmd), vec!["--session-id", "uuid-1"]);
    }

    #[test]
    fn resume_uses_resume_flag() {
        let a = ClaudeAgent;
        let ctx = LaunchCtx { session_id: "uuid-1".into(), mode: LaunchMode::Resume, sessions_root: PathBuf::from("/root") };
        let cmd = a.launch(&ws(), &ctx);
        assert_eq!(args_of(&cmd), vec!["--resume", "uuid-1"]);
    }

    #[test]
    fn sets_memory_redirect_and_ws_env() {
        let a = ClaudeAgent;
        let ctx = LaunchCtx { session_id: "u".into(), mode: LaunchMode::Fresh, sessions_root: PathBuf::from("/root") };
        let cmd = a.launch(&ws(), &ctx);
        assert_eq!(env_of(&cmd, "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"), Some("/tmp/ws/proj/.ws/memory".into()));
        assert_eq!(env_of(&cmd, "WS_WORKSPACE"), Some("proj".into()));
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

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test agents`
Expected: FAIL — modules missing.

- [ ] **Step 3: Write agents/mod.rs**

`src/agents/mod.rs`:
```rust
pub mod claude;

use std::path::PathBuf;
use std::process::Command;

use crate::workspace::Workspace;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LaunchMode {
    Fresh,
    Resume,
}

pub struct LaunchCtx {
    pub session_id: String,
    pub mode: LaunchMode,
    pub sessions_root: PathBuf,
}

pub trait Agent {
    fn id(&self) -> &'static str;
    fn binary(&self) -> String;
    fn is_installed(&self) -> bool;
    fn context_file(&self) -> &'static str;
    fn conversation_id(&self, ws: &Workspace) -> Option<String>;
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> Command;
}

pub fn for_id(id: &str) -> anyhow::Result<Box<dyn Agent>> {
    match id {
        "claude" => Ok(Box::new(claude::ClaudeAgent)),
        "codex" | "gemini" => {
            anyhow::bail!("agent '{id}' is not available in this build (Phase 1 is Claude-only)")
        }
        other => anyhow::bail!("unknown agent: {other}"),
    }
}
```

- [ ] **Step 4: Write agents/claude.rs**

`src/agents/claude.rs`:
```rust
use std::process::Command;

use crate::agents::{Agent, LaunchCtx, LaunchMode};
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

    fn conversation_id(&self, ws: &Workspace) -> Option<String> {
        contract::read_session_id(&ws.state_toml(), self.id())
    }

    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> Command {
        let mut cmd = Command::new(self.binary());
        match ctx.mode {
            LaunchMode::Fresh => {
                cmd.arg("--session-id").arg(&ctx.session_id);
            }
            LaunchMode::Resume => {
                cmd.arg("--resume").arg(&ctx.session_id);
            }
        }
        cmd.current_dir(&ws.root)
            .env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", ws.memory_dir())
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_ROOT", &ctx.sessions_root);
        cmd
    }
}
```

Add `mod agents;` to `main.rs`.

- [ ] **Step 5: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test agents`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/agents/ src/main.rs
git commit -m "feat: Agent trait + Claude adapter (launch/resume, memory redirect)"
```

---

### Task 12: Terminal tab title + color

**Files:**
- Create: `src/term.rs`
- Modify: `src/main.rs` (`mod term;`)
- Test: unit tests inside `term.rs`

**Interfaces:**
- Produces:
  ```rust
  /// OSC-2 title string (window/tab title). Pure — returns the escape or "".
  pub fn title_seq(title: &str) -> String;
  /// iTerm2 tab color escapes for a named color, or "" if unknown/None.
  pub fn color_seq(color: Option<&str>) -> String;
  /// Emit title (+ color) to stdout, honoring NO_COLOR (color) and TTY (both).
  pub fn set_tab(title: &str, color: Option<&str>);
  ```
- Palette (name → RGB): black, red, green, yellow, blue, magenta, cyan, white, orange, purple, grey/gray. Unknown → no color.

- [ ] **Step 1: Write the failing test**

`src/term.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_has_osc2() {
        let s = title_seq("proj");
        assert!(s.starts_with("\x1b]2;"));
        assert!(s.ends_with('\x07'));
        assert!(s.contains("proj"));
    }

    #[test]
    fn known_color_emits_three_channels() {
        let s = color_seq(Some("orange"));
        assert_eq!(s.matches("\x1b]6;1;bg;").count(), 3);
        assert!(s.contains("red;brightness"));
        assert!(s.contains("green;brightness"));
        assert!(s.contains("blue;brightness"));
    }

    #[test]
    fn unknown_or_none_color_is_empty() {
        assert_eq!(color_seq(Some("chartreuse-plaid")), "");
        assert_eq!(color_seq(None), "");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test term`
Expected: FAIL — module missing.

- [ ] **Step 3: Write term.rs**

`src/term.rs`:
```rust
use std::io::{IsTerminal, Write};

fn rgb(color: &str) -> Option<(u8, u8, u8)> {
    let c = match color.to_ascii_lowercase().as_str() {
        "black" => (0, 0, 0),
        "red" => (204, 0, 0),
        "green" => (0, 153, 0),
        "yellow" => (204, 204, 0),
        "blue" => (0, 102, 204),
        "magenta" | "purple" => (153, 0, 204),
        "cyan" => (0, 153, 204),
        "white" => (255, 255, 255),
        "orange" => (230, 126, 34),
        "grey" | "gray" => (128, 128, 128),
        _ => return None,
    };
    Some(c)
}

/// OSC 2: set window/tab title.
pub fn title_seq(title: &str) -> String {
    format!("\x1b]2;{title}\x07")
}

/// iTerm2 tab background color (three OSC-6 channel sequences).
pub fn color_seq(color: Option<&str>) -> String {
    let Some((r, g, b)) = color.and_then(rgb) else {
        return String::new();
    };
    format!(
        "\x1b]6;1;bg;red;brightness;{r}\x07\
         \x1b]6;1;bg;green;brightness;{g}\x07\
         \x1b]6;1;bg;blue;brightness;{b}\x07"
    )
}

/// Emit title and (unless NO_COLOR) tab color, only when stdout is a TTY.
pub fn set_tab(title: &str, color: Option<&str>) {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(title_seq(title).as_bytes());
    if std::env::var_os("NO_COLOR").is_none() {
        let _ = out.write_all(color_seq(color).as_bytes());
    }
    let _ = out.flush();
}
```

Add `mod term;` to `main.rs`.

- [ ] **Step 4: Run test to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test term`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/term.rs src/main.rs
git commit -m "feat: terminal tab title + color (OSC), NO_COLOR/TTY aware"
```

---

### Task 13: Launch flow (`ws <name>`) end-to-end

**Files:**
- Modify: `src/commands.rs` (add `launch`)
- Modify: `src/main.rs` (wire `Cmd::Launch`)
- Test: `tests/launch.rs`, extend `tests/common/mod.rs` with a fake-claude shim

**Interfaces:**
- Consumes: `workspace::open_or_create`, `lock::acquire`, `context::regenerate`,
  `contract::{read_session_id, write_session_id}`, `agents::{for_id, LaunchCtx, LaunchMode}`,
  `term::set_tab`, `config`.
- Produces: `commands::launch(name, agent_override, fresh, force) -> anyhow::Result<()>`.
- Flow:
  1. Load config; determine agent id (`--agent` > workspace.toml default_agent > config.default_agent). Phase 1: must resolve to `claude` (else `for_id` errors clearly).
  2. `open_or_create` the workspace.
  3. Acquire lock (`force`).
  4. Regenerate the agent's context file in `ws.root`.
  5. Resolve session id + mode: if `fresh` or no recorded id → new v4 UUID + `write_session_id` + `Fresh`; else recorded id + `Resume`.
  6. Read `color` from workspace.toml (best-effort); `set_tab(name, color)`.
  7. Build the `Command`; `keep()` the lock; `exec` (Unix) replacing the process. On non-Unix or when `WS_NO_EXEC=1` (test seam), `spawn().wait()` instead and return.

- [ ] **Step 1: Extend the test harness with a fake claude shim**

Add to `tests/common/mod.rs`:
```rust
use std::io::Write;

impl Env {
    /// Write a fake `claude` shim that appends its argv + selected env to `argv.log`
    /// and exits 0. Returns the shim path (point WS_CLAUDE_BIN at it).
    pub fn fake_claude(&self) -> PathBuf {
        let bin = self.home.path().join("fake-claude");
        let log = self.home.path().join("argv.log");
        let script = format!(
            "#!/bin/sh\n\
             {{\n\
             echo \"ARGS: $*\"\n\
             echo \"CWD: $(pwd)\"\n\
             echo \"MEM: $CLAUDE_COWORK_MEMORY_PATH_OVERRIDE\"\n\
             echo \"WSW: $WS_WORKSPACE\"\n\
             }} >> \"{}\"\n\
             exit 0\n",
            log.display()
        );
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&bin).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&bin, p).unwrap();
        }
        bin
    }

    pub fn argv_log(&self) -> String {
        std::fs::read_to_string(self.home.path().join("argv.log")).unwrap_or_default()
    }
}
```

`tests/launch.rs`:
```rust
mod common;
use common::Env;

fn launch_cmd(env: &Env, shim: &std::path::Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", shim).env("WS_NO_EXEC", "1");
    c
}

#[test]
fn first_launch_is_fresh_and_records_session_id() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();

    let log = env.argv_log();
    assert!(log.contains("--session-id"), "expected fresh launch, got: {log}");
    assert!(log.contains("WSW: proj"));
    assert!(log.contains(".ws/memory"));

    // state.toml recorded a session id
    let state = env.root.join("proj/.ws/local/state.toml");
    assert!(state.is_file());
    assert!(std::fs::read_to_string(state).unwrap().contains("session_id"));
}

#[test]
fn second_launch_resumes() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();
    launch_cmd(&env, &shim).arg("proj").assert().success();

    let log = env.argv_log();
    assert!(log.contains("--resume"), "second launch should resume, got: {log}");
}

#[test]
fn fresh_flag_forces_new_conversation() {
    let env = Env::new();
    let shim = env.fake_claude();

    launch_cmd(&env, &shim).arg("proj").assert().success();
    launch_cmd(&env, &shim).args(["proj", "--fresh"]).assert().success();

    // both launches used --session-id (fresh), never --resume
    let log = env.argv_log();
    assert!(!log.contains("--resume"), "got: {log}");
    assert_eq!(log.matches("--session-id").count(), 2);
}

#[test]
fn unknown_agent_errors_clearly() {
    let env = Env::new();
    let shim = env.fake_claude();
    launch_cmd(&env, &shim)
        .args(["proj", "--agent", "codex"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Phase 1 is Claude-only"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test launch`
Expected: FAIL — `Cmd::Launch` still hits "not yet implemented".

- [ ] **Step 3: Implement launch**

Add to `src/commands.rs`:
```rust
use crate::agents::{self, LaunchCtx, LaunchMode};
use crate::context;
use crate::lock;
use crate::term;
use crate::workspace;

pub fn launch(name: String, agent_override: Option<String>, fresh: bool, force: bool) -> Result<()> {
    let cfg = config::load();

    // 1. Resolve agent id: --agent > workspace default > config default.
    let ws_default = workspace_default_agent(&name, &cfg);
    let agent_id = agent_override
        .or(ws_default)
        .unwrap_or_else(|| cfg.default_agent.clone());
    let agent = agents::for_id(&agent_id)?;

    // 2. Create/resolve.
    let (ws, _created) = workspace::open_or_create(&name, agent.id(), &cfg)?;

    // 3. Lock.
    let guard = lock::acquire(&ws.lock_file(), force)?;

    // 4. Regenerate context file.
    context::regenerate(&ws.root.join(agent.context_file()), &ws.name)?;

    // 5. Session id + mode.
    let (session_id, mode) = if fresh {
        let id = uuid::Uuid::new_v4().to_string();
        crate::contract::write_session_id(&ws.state_toml(), agent.id(), &id)?;
        (id, LaunchMode::Fresh)
    } else if let Some(id) = agent.conversation_id(&ws) {
        (id, LaunchMode::Resume)
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        crate::contract::write_session_id(&ws.state_toml(), agent.id(), &id)?;
        (id, LaunchMode::Fresh)
    };

    // 6. Tab title + color.
    let color = workspace_color(&ws);
    term::set_tab(&ws.name, color.as_deref());

    // 7. Build + run.
    let ctx = LaunchCtx {
        session_id,
        mode,
        sessions_root: config::sessions_root(&cfg),
    };
    let mut cmd = agent.launch(&ws, &ctx);

    // Keep the lock file in place; the launched agent inherits our PID.
    guard.keep();

    if std::env::var_os("WS_NO_EXEC").is_some() {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }

    exec(cmd)
}

#[cfg(unix)]
fn exec(mut cmd: std::process::Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Err(cmd.exec().into()) // exec only returns on failure
}

#[cfg(not(unix))]
fn exec(mut cmd: std::process::Command) -> Result<()> {
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(0));
}

fn workspace_default_agent(name: &str, cfg: &config::Config) -> Option<String> {
    let ws = workspace::resolve(name, cfg);
    let s = std::fs::read_to_string(ws.workspace_toml()).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get("default_agent")?.as_str().map(String::from)
}

fn workspace_color(ws: &workspace::Workspace) -> Option<String> {
    let s = std::fs::read_to_string(ws.workspace_toml()).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get("color")?.as_str().map(String::from)
}
```

Wire in `main.rs`:
```rust
        Cmd::Launch { name, agent, fresh, force } => commands::launch(name, agent, fresh, force)?,
```
This replaces the catch-all `other => bail!` for `Launch`. Keep the catch-all only if any variant remains unhandled; after this task all variants are handled, so change `run` to an exhaustive match and drop the catch-all arm.

- [ ] **Step 4: Run tests to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test launch`
Expected: PASS (all four launch tests).

- [ ] **Step 5: Full suite + build**

Run: `. "$HOME/.cargo/env"; cargo test && cargo build --release`
Expected: PASS; release binary at `target/release/ws`.

- [ ] **Step 6: Commit**

```bash
git add src/commands.rs src/main.rs tests/common/mod.rs tests/launch.rs
git commit -m "feat: ws <name> launch flow (create/resume, lock, context, tab)"
```

---

## Self-Review

**1. Spec coverage (§17 Phase 1 items):**
- contract — Task 4 ✓
- config — Task 2 ✓ (per-workspace `--workspace` set is stubbed with a clear "later task" error; global config is complete — acceptable for Phase 1 since launch reads workspace.toml `default_agent` directly)
- create/resume — Tasks 5, 13 ✓
- list — Task 6 ✓
- rm — Task 8 ✓
- adopt — Task 7 ✓
- lock — Task 9 ✓
- context-file generation — Task 10 ✓
- Claude adapter (launch/resume, memory redirect) — Task 11 ✓, wired in Task 13 ✓
- tab title/color — Task 12 ✓, wired in Task 13 ✓

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N" — every step carries full code. ✓

**3. Type consistency:** `Workspace` fields (`name`, `root`) and helpers (`state_toml`, `memory_dir`, `lock_file`, `workspace_toml`) are used identically in Tasks 5, 11, 13. `LaunchCtx`/`LaunchMode` defined in Task 11 and consumed in Task 13 match. `contract::{read_session_id, write_session_id}` signatures (path-based) are consistent across Tasks 4, 11, 13. `crate::now_iso()` defined in Task 4, used by Task 9. ✓

**Deferred to later phases (correctly out of Phase 1 scope):** timeline events (Phase 2), README auto-objective (Phase 2), `prompt_on_launch` interactive `[Y/n/r/d]` (Phase 2/hooks — default off means silent resume works now), per-workspace `config set --workspace` write path, `--handoff` seeding (Phase 4), doctor/statusline/secrets/search/tags/archive/TUI/orchestration (Phases 3–9).

**Known Phase-1 simplifications (intentional, non-blocking):** the heartbeat lock uses PID liveness only (no background mtime updater — a daemon isn't in scope); `now_iso` shells out to `date` to avoid a `chrono` dependency in Phase 1.
