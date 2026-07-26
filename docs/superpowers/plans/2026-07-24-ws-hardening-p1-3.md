# ws Phase 1–3 Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Close the deferred hardening backlog from Phases 1–3 — safe/atomic writes to shared config files, honest destructive-op reporting, a clear "claude not found" preflight, de-duplication, README robustness, and the remaining test-coverage/never-error/CLI-polish nits.

**Architecture:** These are targeted edits to existing modules (config, contract, commands, internal, readme, statusline, workspace, cli), each with a regression test. No new modules, no new dependencies. The whole-branch reviews of Phases 1–3 identified every item here; this plan is the consolidated fix pass.

**Tech Stack:** Rust 2021, existing deps only. Dev: assert_cmd, predicates, tempfile.

## Global Constraints

- **Rust 2021, single crate, binary `ws`.** cargo NOT on PATH — prefix every cargo call with `. "$HOME/.cargo/env";` (Rust 1.97.1).
- **Zero new dependencies.** No serial_test, no libc, etc. Test isolation already relies on the crate-pinned `RUST_TEST_THREADS=1` in `.cargo/config.toml` — keep it; do NOT add a serialization-guard crate.
- **These are edits to existing code — read the current function before changing it** and preserve surrounding behavior and style. Every change ships with a test proving the new behavior.
- **Atomic write pattern (used throughout this plan):** write to a sibling temp file in the same directory, then `std::fs::rename(tmp, target)` (atomic on one filesystem). `registry::save` and `limits::write` already do this — match their style.
- **Never break the agent / never destroy shared state:** `~/.claude/settings.json` and `~/.config/ws/config.toml` are shared with cs and the user. A corrupt existing file must never be silently replaced (bail loudly, matching the Phase-2/3 `register_*` guards).
- **Full suite is the source of truth:** `. "$HOME/.cargo/env"; cargo test` must be all-green before each commit. All existing tests must keep passing.

---

## File Structure (modified)

```
src/config.rs      # set(): refuse to overwrite an unparseable config; atomic write
src/contract.rs    # write_session_id atomic; ancestor-repo detection in init
src/hooksetup.rs   # register_settings + register_statuslines: atomic settings.json write
src/commands.rs    # rm(): report real deletion failure; -limits print_snap helper; single-key toml reader
src/internal.rs    # objective_of → delegate to readme; (uses readme's exact-heading logic)
src/readme.rs      # capture_objective: exact "## Objective" heading, preserve line endings; expose objective_of
src/statusline.rs  # BrokenPipe-safe writeln! for the rendered line(s)
src/cli.rs         # -adopt trailing-arg error; drop the unused peekable / undocumented -f alias
```

---

### Task 1: Safe & atomic writes to shared config/state/settings files

**Files:** `src/config.rs`, `src/contract.rs`, `src/hooksetup.rs`; tests in each + `tests/config.rs`.

**What to change:**
1. **`config::set`** — currently `load()`s (which silently returns defaults on a corrupt `config.toml`) then overwrites, so a single bad line loses ALL the user's other settings. Fix: read the raw file; if it exists but does not parse, `bail!` with a clear message and do NOT write. If absent, start from `Config::default()`. Then apply the change and write **atomically** (temp + rename). (`load()` for reads may keep its permissive `unwrap_or_default` — only the read-modify-write path must be safe.)
2. **`contract::write_session_id`** — currently `std::fs::write` truncate-in-place. Make it atomic (temp + rename).
3. **`hooksetup::register_settings` and `register_statuslines`** — both currently `std::fs::write` the merged settings.json in place. Make both atomic (temp + rename). Keep the existing bail-on-unparseable guard exactly as-is.

**Interfaces:** signatures unchanged; behavior becomes safe/atomic.

- [ ] **Step 1: Write the failing tests**

Add to `src/config.rs` `#[cfg(test)] mod tests`:
```rust
    #[test]
    fn set_refuses_to_clobber_unparseable_config() {
        // isolate config dir
        let d = tempfile::TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path());
        let p = config_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "this = is not : valid toml ][").unwrap();

        let r = set("default_agent", "codex");
        assert!(r.is_err(), "set must refuse to overwrite an unparseable config");
        // original bytes untouched
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "this = is not : valid toml ][");
    }
```
Add to `tests/config.rs`:
```rust
#[test]
fn config_set_preserves_other_keys() {
    let env = Env::new();
    env.cmd().args(["config","set","default_agent","codex"]).assert().success();
    env.cmd().args(["config","set","theme","dark"]).assert().success();
    // first key survives the second write
    env.cmd().args(["config","get","default_agent"]).assert().success()
        .stdout(predicates::str::diff("codex\n"));
    env.cmd().args(["config","get","theme"]).assert().success()
        .stdout(predicates::str::diff("dark\n"));
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test set_refuses_to_clobber; cargo test --test config config_set_preserves`
Expected: the unparseable test FAILS (current code silently resets + overwrites).

- [ ] **Step 3: Implement**

In `src/config.rs` `set`, replace the `let mut cfg = load();` + final `std::fs::write(...)` with a safe read + atomic write:
```rust
pub fn set(key: &str, value: &str) -> Result<()> {
    let path = config_path();
    // Safe read: absent → defaults; present-but-unparseable → refuse (don't clobber).
    let mut cfg: Config = match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid TOML ({e}); refusing to overwrite it. Fix it or move it aside.",
                path.display()
            )
        })?,
        Err(_) => Config::default(),
    };
    match key {
        // …unchanged match arms…
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(&cfg)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
```
(Keep the exact match arms from the current code.)

In `src/contract.rs` `write_session_id`, change the final write to atomic:
```rust
    let tmp = state_toml.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(&t)?)?;
    std::fs::rename(&tmp, state_toml)?;
    Ok(())
```

In `src/hooksetup.rs`, in BOTH `register_settings` and `register_statuslines`, replace the final `std::fs::write(settings_path, …)?;` with:
```rust
    let tmp = settings_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&root)?)?;
    std::fs::rename(&tmp, settings_path)?;
```
(match the actual local variable name for the settings path in each function).

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS (new tests + the whole suite, including Phase-2/3 settings tests).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/contract.rs src/hooksetup.rs tests/config.rs
git commit -m "harden: atomic writes + refuse to clobber unparseable config/settings"
```

---

### Task 2: Honest destructive ops (rm + adopt-into-existing-repo)

**Files:** `src/commands.rs` (`rm`), `src/contract.rs` (`init`); tests in `tests/workspace.rs`.

**What to change:**
1. **`commands::rm`** — currently `std::fs::remove_dir_all(&path).ok()` then unconditionally `println!("removed {name}")` and unregisters. Fix: capture the `Result`; on failure, print a real error to stderr, do NOT print "removed", and do NOT unregister (the workspace still exists on disk). On success, unregister + print "removed {name}" as now.
2. **`contract::init`** — currently `if !root.join(".git").exists()` only detects a `.git` directly in `root`, so `ws -adopt` inside a subdirectory of an existing git repo would `git init` a nested repo. Fix: detect an enclosing repo with `git -C <root> rev-parse --is-inside-work-tree` (exit 0 + stdout "true") and skip `git init` when already inside a repo.

- [ ] **Step 1: Write the failing tests**

Add to `tests/workspace.rs`:
```rust
#[test]
fn adopt_inside_existing_repo_does_not_nested_init() {
    let env = Env::new();
    let repo = env.home.path().join("outer");
    std::fs::create_dir_all(&repo).unwrap();
    std::process::Command::new("git").args(["-C"]).arg(&repo).arg("init").arg("-q").status().unwrap();
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    env.cmd().current_dir(&sub).args(["-adopt","sub"]).assert().success();
    // no nested repo created in the subdirectory
    assert!(!sub.join(".git").exists(), "adopt must not init a nested repo inside an existing one");
}
```
(An `rm`-failure test is environment-dependent — a chmod-based read-only-dir test is flaky across CI. Instead assert the success path still unregisters and the message is printed only on success. Add:)
```rust
#[test]
fn rm_prints_removed_only_on_success() {
    let env = Env::new();
    let proj = env.home.path().join("rmok");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","rmok"]).assert().success();
    env.cmd().args(["-rm","rmok","--force"]).assert().success()
        .stdout(predicates::str::contains("removed rmok"));
    // unregistered
    env.cmd().arg("-list").assert().stdout(predicates::str::contains("rmok").not());
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test workspace adopt_inside_existing_repo`
Expected: FAIL — current code inits a nested `.git` in `sub`.

- [ ] **Step 3: Implement**

In `src/contract.rs` `init`, replace the git-init guard:
```rust
    // git init only if this dir is not already inside a repo (direct or ancestor)
    let inside = std::process::Command::new("git")
        .arg("-C").arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if !inside {
        run_git(root, &["init", "-q"])?;
    }
```

In `src/commands.rs` `rm`, replace the deletion + unconditional message:
```rust
        let result = if under_root {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_dir_all(path.join(".ws"))
        };
        if let Err(e) = result {
            eprintln!("ws: failed to remove {name}: {e}");
            continue; // keep the registry entry — it still exists on disk
        }
        crate::registry::unregister(&name)?;
        println!("removed {name}");
```
(Adapt `under_root`/`path` to the exact locals in the current `rm`.)

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS (incl. the Phase-1 `rm_created_workspace_deletes_dir` / `rm_adopted_external_keeps_project` tests — the success path is unchanged for them).

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs src/contract.rs tests/workspace.rs
git commit -m "harden: rm reports real failures; adopt detects an enclosing git repo"
```

---

### Task 3: Claude preflight — clear "claude not found" error

**Files:** `src/commands.rs` (`launch`); test in `tests/launch.rs`.

**What to change:** the launch flow currently execs `claude` directly; if it's not on PATH the user sees a raw OS error. Fix: after resolving the agent and before building/exec-ing the command, call `agent.is_installed()`; if false, `bail!` with a clear message naming the agent and its binary. Skip the check when `WS_CLAUDE_BIN`/`WS_NO_EXEC` test seams are set OR simply rely on `is_installed()` using the same `binary()` (which honors `WS_CLAUDE_BIN`) — the fake shim IS installed, so tests pass. Do the check only in the real path (it's cheap: `claude --version`).

- [ ] **Step 1: Write the failing test**

Add to `tests/launch.rs`:
```rust
#[test]
fn launch_errors_clearly_when_claude_missing() {
    let env = Env::new();
    env.cmd()
        .env("WS_CLAUDE_BIN", "/nonexistent/definitely-not-claude")
        .env("WS_NO_EXEC", "1")
        .arg("missingclaude")
        .assert()
        .failure()
        .stderr(predicates::str::contains("claude").and(predicates::str::contains("not")));
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test launch launch_errors_clearly`
Expected: FAIL — currently it tries to run the missing shim and the error isn't the clear preflight message (or it succeeds oddly).

- [ ] **Step 3: Implement**

In `src/commands.rs` `launch`, right after `let agent = agents::for_id(&agent_id)?;` and before `open_or_create`, add:
```rust
    if !agent.is_installed() {
        anyhow::bail!(
            "{} is not installed or not on PATH (looked for `{}`). Install it, or set WS_CLAUDE_BIN.",
            agent.id(),
            agent.binary()
        );
    }
```
(`is_installed()` runs `<binary> --version`; with `WS_CLAUDE_BIN` pointing at a real fake shim in the other launch tests, it succeeds — those tests keep passing. With a nonexistent path, it fails → clear bail.)

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS — the new test plus the existing launch tests (their fake shim exists and responds to `--version`? NOTE: the fake shim in `tests/common/mod.rs` currently only echoes argv; confirm it exits 0 for `--version`. It does — it ignores args and exits 0 — so `is_installed()` returns true. If any existing launch test fails because the shim errors on `--version`, adjust the shim to `exit 0` unconditionally, which it already does.)

- [ ] **Step 5: Commit**

```bash
git add src/commands.rs tests/launch.rs
git commit -m "harden: preflight is_installed() with a clear claude-not-found error"
```

---

### Task 4: De-duplication + README robustness

**Files:** `src/commands.rs` (workspace single-key reader, `-limits` print helper), `src/readme.rs` (own `objective_of`, exact heading, preserve line endings), `src/internal.rs` (delegate to readme's `objective_of`).

**What to change:**
1. **`commands.rs`** — `workspace_default_agent` and `workspace_color` are near-identical single-key `workspace.toml` readers. Extract `fn workspace_toml_str(ws: &Workspace, key: &str) -> Option<String>` and have both call it. And the `-limits` per-workspace vs global blocks duplicate the row formatting — extract `fn print_limits_row(label: &str, snap: &limits::LimitsSnapshot, now: i64)`.
2. **`readme.rs`** — make `capture_objective` match the Objective heading EXACTLY (`line.trim() == "## Objective"`, not `starts_with`) and preserve the file's existing line endings / trailing newline (replace only the placeholder line in place rather than rebuilding via `lines()` + rejoin). Expose a `pub fn objective_of(readme: &str) -> Option<String>` (the "first real objective line, or None if placeholder" logic).
3. **`internal.rs`** — replace the private `objective_of` in `internal.rs` with a call to `readme::objective_of`, removing the duplicated placeholder logic.

- [ ] **Step 1: Write the failing tests**

Add to `src/readme.rs` tests:
```rust
    #[test]
    fn objective_heading_is_exact_not_prefix() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        // A different section whose heading merely starts with "Objective" must be ignored.
        std::fs::write(&f, "# p\n\n## Objectives archive\n\n[old]\n\n## Objective\n\n_(captured from the first prompt)_\n\n## Outcome\n").unwrap();
        assert!(capture_objective(&f, "Real goal").unwrap());
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("Real goal"));
        assert!(s.contains("[old]"), "the 'Objectives archive' section must be untouched");
    }

    #[test]
    fn preserves_crlf_untouched_lines() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, "# p\r\n\r\n## Objective\r\n\r\n_(captured from the first prompt)_\r\n\r\n## Outcome\r\n").unwrap();
        capture_objective(&f, "Goal").unwrap();
        let s = std::fs::read_to_string(&f).unwrap();
        // untouched lines keep their CRLF
        assert!(s.contains("## Outcome\r\n"), "existing CRLF lines must be preserved");
    }
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test readme`
Expected: FAIL — exact-heading and CRLF-preservation not yet implemented.

- [ ] **Step 3: Implement**

`src/readme.rs` — rewrite `capture_objective` to operate line-by-line preserving each original line's terminator, and expose `objective_of`:
```rust
pub fn objective_of(readme: &str) -> Option<String> {
    let mut in_obj = false;
    for line in readme.lines() {
        if line.starts_with("## ") {
            in_obj = line.trim() == "## Objective";
            continue;
        }
        if in_obj {
            let t = line.trim();
            if t.is_empty() { continue; }
            if is_placeholder(t) { return None; }
            return Some(t.to_string());
        }
    }
    None
}

pub fn capture_objective(readme_path: &Path, objective: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(readme_path) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };
    let value: String = objective.lines().next().unwrap_or("").trim().chars().take(200).collect();
    if value.is_empty() {
        return Ok(false);
    }
    // Split keeping terminators so untouched lines round-trip byte-for-byte.
    let mut out = String::with_capacity(text.len() + value.len());
    let mut in_obj = false;
    let mut replaced = false;
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let (line, term, next) = split_line(rest); // line without terminator, its terminator, remainder
        rest = next;
        if line.starts_with("## ") {
            in_obj = line.trim() == "## Objective";
        }
        if in_obj && !replaced && is_placeholder(line.trim()) {
            out.push_str(&value);
            out.push_str(term);
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push_str(term);
    }
    if replaced {
        std::fs::write(readme_path, out)?;
    }
    Ok(replaced)
}

/// Split `s` into (line-without-terminator, terminator, remainder). Terminator is
/// "\r\n", "\n", or "" at EOF.
fn split_line(s: &str) -> (&str, &str, &str) {
    match s.find('\n') {
        Some(i) => {
            if i > 0 && s.as_bytes()[i - 1] == b'\r' {
                (&s[..i - 1], "\r\n", &s[i + 1..])
            } else {
                (&s[..i], "\n", &s[i + 1..])
            }
        }
        None => (s, "", ""),
    }
}
```
Keep the existing `is_placeholder` helper (it already matches `_(...)_` and `[...]`).

`src/internal.rs` — delete the local `objective_of` fn and, in `build_context`, call `crate::readme::objective_of(&readme)` instead.

`src/commands.rs` — extract the helpers and call them:
```rust
fn workspace_toml_str(ws: &workspace::Workspace, key: &str) -> Option<String> {
    let s = std::fs::read_to_string(ws.workspace_toml()).ok()?;
    let t: toml::Table = toml::from_str(&s).ok()?;
    t.get(key)?.as_str().map(String::from)
}
```
and rewrite `workspace_default_agent`/`workspace_color` to call `workspace_toml_str(ws, "default_agent")` / `workspace_toml_str(ws, "color")`. For `-limits`, extract a `print_limits_row(label, &snap, now)` and call it from both the per-workspace loop and the global fallback.

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS — new readme tests, and every existing test (objective capture, session-start injection, -limits, launch) still green.

- [ ] **Step 5: Commit**

```bash
git add src/readme.rs src/internal.rs src/commands.rs
git commit -m "harden: dedup workspace/limits readers; exact Objective heading; preserve line endings"
```

---

### Task 5: Coverage + never-error + CLI polish

**Files:** `src/statusline.rs` (BrokenPipe-safe output), `src/cli.rs` (adopt trailing-arg error; drop unused peekable / undocumented `-f`), `tests/limits.rs` (warn-mode + reset-clears-guard), `tests/internal.rs` (strengthen bash-audit-ignores-non-bash).

**What to change:**
1. **`statusline.rs`** — `println!` panics on `BrokenPipe`. Replace the rendered-line output (both `run` and `run_subagent`) with `let _ = writeln!(std::io::stdout(), "{…}");` so a closed pipe can't panic (closing the last hole in "never exit non-zero").
2. **`cli.rs`** — `-adopt foo bar` silently drops `bar`; make it error on an unexpected extra arg (mirror the Launch arm). Remove the undocumented `-f` alias on `-rm` (keep `--force` only). Remove the unused `.peekable()` if present.
3. **Tests** — add a warn-mode test (config `limit_action=warn` → over-threshold Stop does NOT block but the guard is written) and a reset-clears-guard test (guard present, then a Stop with an under-threshold snapshot removes it). Strengthen `bash_audit_ignores_non_bash` to assert the log file's absence/lack of the command specifically for a non-Bash tool after a workspace exists.

- [ ] **Step 1: Write the failing tests**

Add to `tests/limits.rs`:
```rust
#[test]
fn warn_mode_does_not_block_but_sets_guard() {
    let env = Env::new();
    let proj = env.home.path().join("warn");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","warn"]).assert().success();
    env.cmd().args(["config","set","limit_action","warn"]).assert().success();

    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":95.0,"resets_at":9999999999},"seven_day":{"used_percentage":10.0,"resets_at":9999999999}}}"#;
    env.cmd().env("WS_WORKSPACE","warn").env("WS_DIR",&proj)
        .arg("statusline").write_stdin(sample).assert().success();

    // warn mode: Stop approves (or falls through to reminder) — never a limit BLOCK — but the guard is set
    env.cmd().env("WS_WORKSPACE","warn").env("WS_DIR",&proj)
        .args(["internal","stop"]).write_stdin("{}").assert().success()
        .stdout(predicates::str::contains("handoff").not());
    assert!(proj.join(".ws/local/limit-guard").exists());
}

#[test]
fn reset_clears_guard() {
    let env = Env::new();
    let proj = env.home.path().join("reset");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","reset"]).assert().success();
    // plant a guard, and capture an UNDER-threshold snapshot
    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(proj.join(".ws/local/limit-guard"), "x").unwrap();
    let sample = r#"{"rate_limits":{"five_hour":{"used_percentage":5.0,"resets_at":9999999999},"seven_day":{"used_percentage":5.0,"resets_at":9999999999}}}"#;
    env.cmd().env("WS_WORKSPACE","reset").env("WS_DIR",&proj)
        .arg("statusline").write_stdin(sample).assert().success();

    // a Stop now sees under-threshold → clears the guard
    env.cmd().env("WS_WORKSPACE","reset").env("WS_DIR",&proj)
        .args(["internal","stop"]).write_stdin("{}").assert().success();
    assert!(!proj.join(".ws/local/limit-guard").exists(), "guard should clear on reset");
}
```
Add a cli parse test to `src/cli.rs` tests:
```rust
    #[test]
    fn adopt_rejects_extra_args() {
        assert!(parse(vec!["-adopt".into(),"a".into(),"b".into()]).is_err());
    }
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test limits warn_mode; cargo test --test limits reset_clears; cargo test adopt_rejects_extra`
Expected: FAIL (reset-clears-guard passes only if Task 6 of Phase 3 already removes the guard on under-threshold — it does; but the guard here is planted manually and the snapshot is under-threshold, so it should clear; if the Stop path requires limits.json which the statusline just wrote, this verifies the clear path. The cli and warn tests fail against current code — `-adopt` drops the extra arg, warn-mode test needs the config set to take effect).

- [ ] **Step 3: Implement**

`src/statusline.rs` — add `use std::io::Write;` and change the final `println!(...)` in `run` to:
```rust
    let _ = writeln!(std::io::stdout(), "{}", render(&input, ws_name.as_deref(), no_color));
```
and in `run_subagent` change the per-task `println!("{row}")` to `let _ = writeln!(std::io::stdout(), "{row}");`.

`src/cli.rs` — in the `-adopt` arm, after taking the optional name, error if any further args remain:
```rust
        "-adopt" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -adopt [<name>]");
            }
            Ok(Cmd::Adopt { name })
        }
```
In the `-rm` arm, remove the `"-f"` match so only `--force` is accepted. Remove the unused `.peekable()` on the args iterator if it's still there (use a plain iterator).

`tests/internal.rs` — strengthen `bash_audit_ignores_non_bash` to first create a workspace, run a Bash command through bash-audit (so the log exists), then run a NON-Bash tool and assert the log does not gain a second BASH line for it (i.e. the log's BASH-line count stays at 1).

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS (full suite).

- [ ] **Step 5: Commit**

```bash
git add src/statusline.rs src/cli.rs tests/limits.rs tests/internal.rs
git commit -m "harden: BrokenPipe-safe statusline; strict -adopt args; warn/reset guard tests"
```

---

## Self-Review

**Backlog coverage:**
- Atomic + safe writes (config.set clobber, state.toml, settings.json ×2) — Task 1 ✓
- rm success-on-failure honesty — Task 2 ✓; adopt ancestor-repo detection — Task 2 ✓
- is_installed() wired for clear error — Task 3 ✓
- workspace reader dup, -limits dup, readme objective_of dup — Task 4 ✓
- readme exact heading + CRLF preservation — Task 4 ✓
- statusline BrokenPipe — Task 5 ✓; -adopt extra-arg + `-f` alias + unused peekable — Task 5 ✓
- warn-mode + reset-clears-guard + bash-audit-non-bash coverage — Task 5 ✓

**Intentionally left deferred (documented, low value / by design):**
- Stop hook emits `{"decision":"approve"}` — correct today (Claude acts only on `block`), matches cs; changing it risks behavior. Leave.
- Hardcoded hook `timeout: 10` — ample for these handlers. Leave.
- Test-isolation serialization guard — `RUST_TEST_THREADS=1` is pinned and works; a new crate is out of scope per Global Constraints. Leave.
- statusline `git` shell-out per refresh — matches cs, `--no-optional-locks` keeps it cheap. Leave.
- `render` prefers `workspace.current_dir` over `cwd` — reviewer confirmed non-issue (same repo → same branch from any subdir). Leave.

**Type consistency:** `workspace_toml_str`/`print_limits_row` (Task 4) are new private helpers in `commands.rs`; `readme::objective_of` (Task 4) is consumed by `internal::build_context`. No signature of a public/cross-module item changes. All edits preserve existing test expectations (verified per task).
