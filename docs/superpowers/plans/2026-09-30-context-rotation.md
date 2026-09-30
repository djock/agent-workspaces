# Context-aware rotation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rotating a conversation stays inside the agent (`/ws:rotate`, then `/clear`), and ws tells the agent when it is time to rotate.

**Architecture:** The Claude status line already renders every second with the conversation's context %. It will also stamp that reading, keyed by session id, into `.ws/local/context.json`. The Stop hook reads the reading for its own session id and blocks once per conversation past `rotate_nudge`. `ws -rotate` writes the handoff skeleton and arms `.ws/local/pending-handoff`. SessionStart consumes the marker on source `startup` or `clear` and points the fresh conversation at the handoff.

**Tech Stack:** Rust (edition as in `Cargo.toml`), serde/serde_json, assert_cmd + predicates for integration tests.

**Spec:** `docs/superpowers/specs/2026-09-30-cs-parity-roadmap.md` (Phase 1)

## Global Constraints

- `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo command. cargo is not on the agent shell's PATH.
- Gates before every commit: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. CI runs `cargo fmt --check`, and it failed v0.14.0 once.
- Hooks never fail the agent. Every error inside a hook reads as "nothing to do", so it returns early and exits 0.
- New config keys follow the `Config` doc rule: every field is read by something, listed in `config::list`, and settable in `set_locked`.
- Precondition: the uncommitted `ws -update` restyle on `main` (CHANGELOG.md, install.sh, src/commands.rs, src/update.rs, tests) is committed or shelved before Task 1. Do not mix it into these commits.
- Branch: `feat/context-rotation` off `main`.

## Review Focus

1. **A teammate or a second conversation in the same workspace.** Its status line writes a different session id. The Stop hook must only act on a reading whose `session_id` equals its own payload's `session_id`. The Task 3 test pins this.
2. **A stale reading.** If the status line stopped rendering hours ago, the old number must not trigger a nudge. Readings older than 600 s are ignored (Task 1 test).
3. **An armed marker that is never used.** The user rotates, then resumes the old conversation instead of clearing, and a `/clear` hours later picks up an out-of-date handoff. Markers older than 6 h are dropped, not consumed (Task 4 test).
4. **A marker naming a path.** `../../etc/passwd` or `a/b.md` must be refused, and the marker still removed (Task 4 test).
5. **`/compact`.** It keeps the session id and lowers context %. The nudge already fired for that id, so it must not fire again. A marker must not be consumed on source `compact` (Task 4 test for `compact`; the nudge stamp is keyed on session id, Task 3).

---

## File Structure

- Create `src/rotation.rs`: the context reading (`ContextReading`, `write_reading`, `reading_for`). One responsibility: "how full is this conversation".
- Modify `src/handoff.rs`: add the pending-handoff marker (`arm`, `take`), next to `latest_handoff`.
- Modify `src/statusline.rs`: add `session_id` to `StatuslineInput`, and call `rotation::write_reading` in `run()`.
- Modify `src/config.rs`: add `rotate_nudge: u8` (default 65, 0 = off).
- Modify `src/internal.rs`: `rotate_check` in `stop()`, and marker consumption in `session_start()`.
- Modify `src/commands.rs` (`rotate`): new template, arm the marker.
- Modify `src/assets/prompts/rotate.md`: cs's handoff quality rules, and "run `ws -rotate`, fill it, tell the user `/clear`".
- Modify `src/main.rs`: `mod rotation;`.
- Tests: `tests/statusline.rs`, `tests/internal.rs`, `tests/task_rotate.rs`, `tests/config.rs`, plus unit tests in `src/rotation.rs` and `src/handoff.rs`.

---

### Task 1: The status line records the context reading

**Files:**
- Create: `src/rotation.rs`
- Modify: `src/main.rs` (module list, after `mod rewrite;`)
- Modify: `src/statusline.rs:42-56` (struct), `src/statusline.rs:302-315` (`run`)
- Test: `src/rotation.rs` (unit), `tests/statusline.rs`

**Interfaces:**
- Produces: `rotation::write_reading(ws: &Workspace, session_id: &str, used_pct: f64) -> anyhow::Result<()>`, `rotation::reading_for(ws: &Workspace, session_id: &str, now: i64) -> Option<u8>`, `rotation::READING_FRESH_SECS: i64 = 600`.

- [ ] **Step 1: Write the failing unit tests** in a new `src/rotation.rs`:

```rust
//! How full the current conversation is, as the status line last saw it.
//!
//! Claude Code gives the context % only to the status line, which repaints
//! about once a second. The Stop hook needs the same number, so the status line
//! stamps it here, keyed by the conversation's session id. The key matters: a
//! teammate or a second conversation in the same workspace writes its own id,
//! and a reading for another conversation must never trigger this one's nudge.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::workspace::Workspace;

/// How old a reading may be before it is ignored. The status line renders
/// continuously while a conversation is open, so an older one describes a
/// conversation nobody is looking at.
pub const READING_FRESH_SECS: i64 = 600;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextReading {
    pub session_id: String,
    pub pct: u8,
    pub stamped_at: i64,
}

fn reading_path(ws: &Workspace) -> PathBuf {
    ws.local_dir().join("context.json")
}

pub fn write_reading(ws: &Workspace, session_id: &str, used_pct: f64) -> Result<()> {
    unimplemented!()
}

pub fn reading_for(ws: &Workspace, session_id: &str, now: i64) -> Option<u8> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(td: &TempDir) -> Workspace {
        Workspace { name: "t".into(), root: td.path().to_path_buf() }
    }

    #[test]
    fn a_reading_is_returned_for_its_own_session() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "abc", 66.6).unwrap();
        let now = crate::limits::now_epoch();
        assert_eq!(reading_for(&w, "abc", now), Some(67));
    }

    #[test]
    fn another_sessions_reading_is_not_this_ones() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "teammate", 90.0).unwrap();
        assert_eq!(reading_for(&w, "lead", crate::limits::now_epoch()), None);
    }

    #[test]
    fn an_empty_session_id_writes_nothing_and_matches_nothing() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "  ", 90.0).unwrap();
        assert!(!w.local_dir().join("context.json").exists());
        assert_eq!(reading_for(&w, "", crate::limits::now_epoch()), None);
    }

    #[test]
    fn a_stale_reading_is_ignored() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "abc", 90.0).unwrap();
        let later = crate::limits::now_epoch() + READING_FRESH_SECS + 1;
        assert_eq!(reading_for(&w, "abc", later), None);
    }

    #[test]
    fn out_of_range_percentages_are_clamped() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let now = crate::limits::now_epoch();
        write_reading(&w, "abc", 250.0).unwrap();
        assert_eq!(reading_for(&w, "abc", now), Some(100));
        write_reading(&w, "abc", f64::NAN).unwrap();
        assert_eq!(reading_for(&w, "abc", now), Some(0));
    }

    #[test]
    fn a_corrupt_reading_reads_as_none() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        std::fs::create_dir_all(w.local_dir()).unwrap();
        std::fs::write(w.local_dir().join("context.json"), "{not json").unwrap();
        assert_eq!(reading_for(&w, "abc", crate::limits::now_epoch()), None);
    }
}
```

Add `mod rotation;` to `src/main.rs` after `mod rewrite;`.

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test --bin ws rotation::`
Expected: FAIL, panicked at `not implemented`.

- [ ] **Step 3: Implement the two functions**

```rust
pub fn write_reading(ws: &Workspace, session_id: &str, used_pct: f64) -> Result<()> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Ok(()); // nothing to key it by; a reading for "anyone" is worse than none
    }
    // `as u8` saturates and maps NaN to 0; the clamp is for readability.
    let pct = used_pct.round().clamp(0.0, 100.0) as u8;
    let r = ContextReading {
        session_id: session_id.to_string(),
        pct,
        stamped_at: crate::limits::now_epoch(),
    };
    crate::atomic::atomic_write(&reading_path(ws), serde_json::to_string(&r)?)
}

pub fn reading_for(ws: &Workspace, session_id: &str, now: i64) -> Option<u8> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let raw = std::fs::read_to_string(reading_path(ws)).ok()?;
    let r: ContextReading = serde_json::from_str(&raw).ok()?;
    let fresh = now - r.stamped_at <= READING_FRESH_SECS;
    (r.session_id == session_id && fresh).then_some(r.pct)
}
```

If `atomic_write` does not create parent directories, add `std::fs::create_dir_all(ws.local_dir())?;` before it (check `src/atomic.rs:15`).

- [ ] **Step 4: Run the unit tests and confirm they pass**

Run: `cargo test --bin ws rotation::`
Expected: 6 passed.

- [ ] **Step 5: Write the failing integration test** in `tests/statusline.rs`:

```rust
#[test]
fn statusline_stamps_the_context_reading_for_its_session() {
    let env = Env::new();
    let proj = env.home.path().join("ctx");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "ctx"]).assert().success();

    env.cmd()
        .env("WS_WORKSPACE", "ctx")
        .env("WS_DIR", &proj)
        .env("NO_COLOR", "1")
        .arg("statusline")
        .write_stdin(r#"{"session_id":"sess-1","context_window":{"used_percentage":71.2}}"#)
        .assert()
        .success();

    let raw = std::fs::read_to_string(proj.join(".ws/local/context.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["session_id"], "sess-1");
    assert_eq!(v["pct"], 71);
}
```

- [ ] **Step 6: Run it and confirm it fails**

Run: `cargo test --test statusline statusline_stamps_the_context_reading_for_its_session`
Expected: FAIL, `No such file or directory` reading `context.json`.

- [ ] **Step 7: Wire it in.** In `src/statusline.rs`, add this to `StatuslineInput`:

```rust
    #[serde(default)]
    pub session_id: String,
```

In `run()`, inside the `current_ws().map(|ws| { ... })` closure, after the `limits::write(...)` line:

```rust
        let _ = crate::rotation::write_reading(
            &ws,
            &input.session_id,
            input.context_window.used_percentage,
        );
```

- [ ] **Step 8: Run the status line tests and confirm they pass**

Run: `cargo test --test statusline && cargo test --bin ws statusline::`
Expected: all pass.

- [ ] **Step 9: Gates and commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings
git add src/rotation.rs src/main.rs src/statusline.rs tests/statusline.rs
git commit -m "feat(rotation): status line stamps the conversation's context reading"
```

---

### Task 2: `rotate_nudge` config key

**Files:**
- Modify: `src/config.rs` (struct, `Default`, `list`, `set_locked`)
- Test: `tests/config.rs`

**Interfaces:**
- Produces: `Config::rotate_nudge: u8`, default `65`, `0` = off, values above 100 refused.

- [ ] **Step 1: Write the failing tests** in `tests/config.rs`:

```rust
#[test]
fn rotate_nudge_defaults_to_65_and_accepts_0_to_turn_it_off() {
    let env = Env::new();
    env.cmd()
        .args(["config", "get", "rotate_nudge"])
        .assert()
        .success()
        .stdout(predicates::str::contains("65"));
    env.cmd().args(["config", "set", "rotate_nudge", "0"]).assert().success();
    env.cmd()
        .args(["config", "get", "rotate_nudge"])
        .assert()
        .success()
        .stdout(predicates::str::contains("0"));
}

#[test]
fn rotate_nudge_refuses_a_percentage_over_100() {
    let env = Env::new();
    env.cmd()
        .args(["config", "set", "rotate_nudge", "101"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("0 and 100"));
}
```

(If `tests/config.rs` imports `predicate` instead of `predicates`, match what it uses.)

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test --test config rotate_nudge`
Expected: FAIL, `unknown config key: rotate_nudge`.

- [ ] **Step 3: Implement.** Add the field after `notebook_prompt` in `Config`:

```rust
    /// Context % at which the Stop hook asks the agent to rotate (write a
    /// handoff, then have the user `/clear`). Once per conversation. 0 turns
    /// it off. Claude only: Codex has no status line to read it from.
    pub rotate_nudge: u8,
```

Default: `rotate_nudge: 65,`. In `list`: `("rotate_nudge".into(), cfg.rotate_nudge.to_string()),` after `notebook_prompt`. In `set_locked`, after the `notebook_prompt` arm:

```rust
        "rotate_nudge" => {
            let v: u8 = value.parse()?;
            if v > 100 {
                bail!("rotate_nudge must be between 0 and 100 (0 turns it off)");
            }
            cfg.rotate_nudge = v;
        }
```

- [ ] **Step 4: Run the config tests and confirm they pass**

Run: `cargo test --test config`
Expected: all pass. If a test snapshots the full `config list` output, add `rotate_nudge = 65` to it.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs tests/config.rs
git commit -m "feat(config): rotate_nudge threshold"
```

---

### Task 3: The Stop hook nudges once per conversation

**Files:**
- Modify: `src/internal.rs` (`stop()` after the `limit_check` block; new `rotate_check`)
- Test: `tests/internal.rs`

**Interfaces:**
- Consumes: `rotation::reading_for` (Task 1), `Config::rotate_nudge` (Task 2), `HookInput::session_id`.
- Produces: `fn rotate_check(ws: &Workspace, h: &hookio::HookInput) -> Option<String>`. Stamp file `.ws/local/rotate-nudge.stamp` holds the session id already nudged.

- [ ] **Step 1: Write the failing tests** in `tests/internal.rs`. They use the existing `adopt_ws`.

```rust
fn write_reading(proj: &std::path::Path, session: &str, pct: u8) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(
        proj.join(".ws/local/context.json"),
        format!(r#"{{"session_id":"{session}","pct":{pct},"stamped_at":{now}}}"#),
    )
    .unwrap();
}

fn stop(env: &Env, name: &str, proj: &std::path::Path, session: &str) -> String {
    let out = env
        .cmd()
        .env("WS_WORKSPACE", name)
        .env("WS_DIR", proj)
        .env("WS_AGENT", "claude")
        .args(["internal", "stop"])
        .write_stdin(format!(r#"{{"session_id":"{session}"}}"#))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn stop_nudges_rotation_once_per_conversation_past_the_threshold() {
    let env = Env::new();
    let proj = adopt_ws(&env, "rot");
    write_reading(&proj, "s1", 70);

    let first = stop(&env, "rot", &proj, "s1");
    assert!(first.contains("\"decision\":\"block\""), "{first}");
    assert!(first.contains("70%"), "names the reading: {first}");
    assert!(first.contains("/clear"), "tells the user the step: {first}");

    let second = stop(&env, "rot", &proj, "s1");
    assert!(!second.contains("70%"), "once per conversation: {second}");

    // A fresh conversation after /clear gets its own nudge.
    write_reading(&proj, "s2", 70);
    assert!(stop(&env, "rot", &proj, "s2").contains("70%"));
}

#[test]
fn stop_ignores_another_conversations_reading() {
    let env = Env::new();
    let proj = adopt_ws(&env, "rot2");
    write_reading(&proj, "teammate", 95);
    assert!(!stop(&env, "rot2", &proj, "lead").contains("95%"));
}

#[test]
fn stop_stays_quiet_below_the_threshold_and_when_turned_off() {
    let env = Env::new();
    let proj = adopt_ws(&env, "rot3");
    write_reading(&proj, "s1", 64);
    assert!(!stop(&env, "rot3", &proj, "s1").contains("64%"));

    env.cmd().args(["config", "set", "rotate_nudge", "0"]).assert().success();
    write_reading(&proj, "s1", 99);
    assert!(!stop(&env, "rot3", &proj, "s1").contains("99%"));
}
```

Before writing these, check that the notebook reminder cannot fire in a fresh adopted workspace. `notebook_check` returns None when the notebook dir was never written. If `-adopt` writes a notebook, set `.env("XDG_CONFIG_HOME", ...)` and write `notebook_prompt = false` into the config instead. The assertions only look for the `%` text, so a notebook block would not give a false pass. It could still make the "fires" test fail for the wrong reason, which is why this check comes first.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test --test internal stop_`
Expected: `stop_nudges_rotation_once_per_conversation_past_the_threshold` FAILS (no block). The other two pass vacuously for now.

- [ ] **Step 3: Implement.** In `src/internal.rs`, in `stop()` right after the `limit_check` block:

```rust
    // Context before the notebook: the rotation directive asks for the
    // notebook update itself, so both firing on one stop would ask twice.
    if let Some(directive) = rotate_check(&ws, &h) {
        println!("{}", hookio::decision_block(&directive));
        return;
    }
```

Add the function next to `notebook_check`:

```rust
/// Returns Some(directive) once per conversation when its context reading has
/// passed `rotate_nudge`. The stamp holds the session id already nudged: a
/// `/compact` keeps the id and lowers the reading, and must not re-arm it; a
/// `/clear` brings a new id, which gets its own nudge.
fn rotate_check(ws: &Workspace, h: &hookio::HookInput) -> Option<String> {
    let threshold = crate::config::load().rotate_nudge;
    if threshold == 0 {
        return None;
    }
    let pct = crate::rotation::reading_for(ws, &h.session_id, limits::now_epoch())?;
    if pct < threshold {
        return None;
    }
    let stamp = ws.local_dir().join("rotate-nudge.stamp");
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == h.session_id.trim()) {
        return None;
    }
    let _ = std::fs::create_dir_all(ws.local_dir());
    let _ = std::fs::write(&stamp, h.session_id.trim());
    Some(format!(
        "Context check: this conversation is at {pct}% of its context window. Finish \
         only the step you are on and start nothing new. Then rotate: run /ws:rotate \
         (it writes and arms a handoff), and tell the user to type /clear, which \
         continues from the handoff in a fresh conversation. If the work has reached \
         a natural end, say that in one line instead and stop."
    ))
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test --test internal`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings
git add src/internal.rs tests/internal.rs
git commit -m "feat(rotation): Stop hook asks to rotate once per conversation past rotate_nudge"
```

---

### Task 4: Arm the handoff; consume it on the next fresh start

**Files:**
- Modify: `src/handoff.rs` (add `arm`, `take`, `MARKER_MAX_AGE_SECS`)
- Modify: `src/commands.rs:1580-1617` (`rotate` arms after writing)
- Modify: `src/internal.rs` (`session_start`, after `build_context`)
- Test: `src/handoff.rs` (unit), `tests/internal.rs`, `tests/task_rotate.rs`

**Interfaces:**
- Produces: `handoff::arm(ws: &Workspace, handoff: &Path) -> anyhow::Result<()>`, `handoff::take(ws: &Workspace) -> Option<PathBuf>`. The marker is `.ws/local/pending-handoff`, holding the handoff's file name.

- [ ] **Step 1: Write the failing unit tests.** Append to `src/handoff.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(td: &TempDir) -> Workspace {
        Workspace { name: "t".into(), root: td.path().to_path_buf() }
    }

    fn handoff(w: &Workspace, name: &str) -> PathBuf {
        let dir = w.ws_dir().join("handoffs");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "# Handoff").unwrap();
        p
    }

    #[test]
    fn an_armed_handoff_is_taken_once() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "2026-09-30T120000Z-me.md");
        arm(&w, &h).unwrap();
        assert_eq!(take(&w), Some(h));
        assert_eq!(take(&w), None, "consumed");
    }

    #[test]
    fn a_marker_naming_a_path_is_refused_and_removed() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        std::fs::create_dir_all(w.local_dir()).unwrap();
        for bad in ["../../etc/passwd", "a/b.md", "a\\b.md", ".."] {
            std::fs::write(w.local_dir().join("pending-handoff"), bad).unwrap();
            assert_eq!(take(&w), None, "{bad}");
            assert!(!w.local_dir().join("pending-handoff").exists(), "{bad} left armed");
        }
    }

    #[test]
    fn a_marker_whose_handoff_is_gone_takes_nothing() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "x.md");
        arm(&w, &h).unwrap();
        std::fs::remove_file(&h).unwrap();
        assert_eq!(take(&w), None);
    }

    #[test]
    fn a_marker_older_than_the_max_age_is_dropped_not_taken() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "old.md");
        arm(&w, &h).unwrap();
        let old = std::time::SystemTime::now()
            - std::time::Duration::from_secs(MARKER_MAX_AGE_SECS + 60);
        let f = std::fs::File::options()
            .write(true)
            .open(w.local_dir().join("pending-handoff"))
            .unwrap();
        f.set_modified(old).unwrap();
        assert_eq!(take(&w), None);
        assert!(!w.local_dir().join("pending-handoff").exists());
    }
}
```

Add stubs so it compiles:

```rust
/// An armed handoff older than this is out of date: the user rotated, then
/// carried on in the old conversation instead of clearing.
pub const MARKER_MAX_AGE_SECS: u64 = 6 * 3600;

pub fn arm(ws: &Workspace, handoff: &std::path::Path) -> anyhow::Result<()> {
    unimplemented!()
}

pub fn take(ws: &Workspace) -> Option<PathBuf> {
    unimplemented!()
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test --bin ws handoff::`
Expected: FAIL, `not implemented`.

- [ ] **Step 3: Implement**

```rust
fn marker(ws: &Workspace) -> PathBuf {
    ws.local_dir().join("pending-handoff")
}

/// Arm `handoff` for the next fresh conversation. Stores the file name only:
/// the consumer resolves it under `.ws/handoffs/`, so the marker can never
/// point anywhere else.
pub fn arm(ws: &Workspace, handoff: &std::path::Path) -> anyhow::Result<()> {
    let name = handoff
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("handoff has no file name: {}", handoff.display()))?;
    std::fs::create_dir_all(ws.local_dir())?;
    crate::atomic::atomic_write(&marker(ws), name)
}

/// Consume the armed handoff. The marker is removed whatever it held: a marker
/// that is invalid, stale or points at a missing file is dropped, not left to
/// fire later.
pub fn take(ws: &Workspace) -> Option<PathBuf> {
    let m = marker(ws);
    let age = std::fs::metadata(&m).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    let raw = std::fs::read_to_string(&m).unwrap_or_default();
    let _ = std::fs::remove_file(&m);
    if age.as_secs() > MARKER_MAX_AGE_SECS {
        return None;
    }
    let name = raw.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return None;
    }
    let p = ws.ws_dir().join("handoffs").join(name);
    p.is_file().then_some(p)
}
```

- [ ] **Step 4: Run the unit tests and confirm they pass**

Run: `cargo test --bin ws handoff::`
Expected: 4 passed.

- [ ] **Step 5: Write the failing integration tests.** In `tests/task_rotate.rs`:

```rust
#[test]
fn rotate_arms_the_handoff_it_writes() {
    let env = Env::new();
    let p = adopt(&env, "proj");
    env.cmd().env("WS_WORKSPACE", "proj").current_dir(&p).arg("-rotate").assert().success();
    let armed = std::fs::read_to_string(p.join(".ws/local/pending-handoff")).unwrap();
    assert!(p.join(".ws/handoffs").join(armed.trim()).is_file(), "armed: {armed}");
}
```

In `tests/internal.rs`:

```rust
fn arm(proj: &std::path::Path, name: &str) {
    std::fs::create_dir_all(proj.join(".ws/handoffs")).unwrap();
    std::fs::write(proj.join(".ws/handoffs").join(name), "# Handoff").unwrap();
    std::fs::create_dir_all(proj.join(".ws/local")).unwrap();
    std::fs::write(proj.join(".ws/local/pending-handoff"), name).unwrap();
}

fn session_start(env: &Env, name: &str, proj: &std::path::Path, source: &str) -> String {
    let out = env
        .cmd()
        .env("WS_WORKSPACE", name)
        .env("WS_DIR", proj)
        .args(["internal", "session-start"])
        .write_stdin(format!(r#"{{"source":"{source}","session_id":"n1"}}"#))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn clear_continues_from_the_armed_handoff_once() {
    let env = Env::new();
    let proj = adopt_ws(&env, "h1");
    arm(&proj, "2026-09-30T120000Z-me.md");

    let out = session_start(&env, "h1", &proj, "clear");
    assert!(out.contains(".ws/handoffs/2026-09-30T120000Z-me.md"), "{out}");
    assert!(out.contains("Successor report"), "{out}");
    assert!(!proj.join(".ws/local/pending-handoff").exists());

    let tl = std::fs::read_to_string(proj.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("handoff-consumed"), "{tl}");

    let again = session_start(&env, "h1", &proj, "clear");
    assert!(!again.contains("2026-09-30T120000Z-me.md"), "only once: {again}");
}

#[test]
fn compact_and_resume_leave_the_marker_armed() {
    let env = Env::new();
    let proj = adopt_ws(&env, "h2");
    arm(&proj, "a.md");
    for source in ["compact", "resume"] {
        let out = session_start(&env, "h2", &proj, source);
        assert!(!out.contains("a.md"), "{source}: {out}");
        assert!(proj.join(".ws/local/pending-handoff").exists(), "{source} consumed it");
    }
    assert!(session_start(&env, "h2", &proj, "startup").contains(".ws/handoffs/a.md"));
}
```

- [ ] **Step 6: Run them and confirm they fail**

Run: `cargo test --test task_rotate rotate_arms && cargo test --test internal handoff`
Expected: FAIL (no marker written; no pointer in the context).

- [ ] **Step 7: Implement.** In `commands::rotate`, after `crate::atomic::atomic_write(&path, body)?;`:

```rust
    crate::handoff::arm(&ws, &path)?;
```

Replace the two closing `println!`s with:

```rust
    println!("wrote {}", path.display());
    println!("armed: the next fresh conversation (/clear, or a new `ws {n}`) continues from it.");
    println!("`ws {n} --handoff` also points a launch at it.");
```

In `internal::session_start`, after `let mut ctx = build_context(&ws);`:

```rust
    // A rotation armed by `ws -rotate` continues here. Only on a fresh start: a
    // compaction or a resume is the same conversation, and consuming the marker
    // there would spend a rotation the user has not made yet.
    if h.source == "startup" || h.source == "clear" {
        if let Some(path) = crate::handoff::take(&ws) {
            let name = path.file_name().and_then(|f| f.to_str()).unwrap_or_default();
            ctx.push_str(&format!(
                "\n\nThis conversation continues a rotation. Read .ws/handoffs/{name} before \
                 anything else and take its Next step as your first action. When that step \
                 is done, append a `## Successor report` to the handoff: what you had to \
                 look up again, re-derive, or found wrong — or `none`."
            ));
            let _ = timeline::record(
                &ws.timeline(),
                "handoff-consumed",
                &actors::actor_slug(),
                serde_json::json!({ "file": name, "source": h.source }),
            );
        }
    }
```

- [ ] **Step 8: Run all affected tests and confirm they pass**

Run: `cargo test --test task_rotate && cargo test --test internal && cargo test --bin ws handoff::`
Expected: all pass. `rotate_writes_a_handoff_skeleton_that_handoff_then_finds` still passes, because stdout still contains `wrote` and `--handoff`.

- [ ] **Step 9: Commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings
git add src/handoff.rs src/commands.rs src/internal.rs tests/task_rotate.rs tests/internal.rs
git commit -m "feat(rotation): ws -rotate arms its handoff; the next fresh conversation continues from it"
```

---

### Task 5: Handoffs worth continuing from

**Files:**
- Modify: `src/commands.rs:1604-1606` (template body)
- Modify: `src/assets/prompts/rotate.md`
- Test: `tests/task_rotate.rs` (update the heading assertion; add a prompt test)

**Interfaces:**
- Consumes: `ws -rotate` (Task 4) as the one command the prompt runs.

- [ ] **Step 1: Update the tests first.** In `rotate_writes_a_handoff_skeleton_that_handoff_then_finds`, replace the heading list with:

```rust
    for heading in [
        "# Handoff",
        "## Next step",
        "## Conversation-only facts",
        "## Where things stand",
        "## Rejected alternatives",
        "## Watch out for",
    ] {
        assert!(body.contains(heading), "missing {heading:?} in:\n{body}");
    }
    let next = body.find("## Next step").unwrap();
    let rest = body.find("## Where things stand").unwrap();
    assert!(next < rest, "the next step comes first: a successor reads top-down");
    // Today's literal carries 9 spaces into every line, and 4+ leading spaces
    // make markdown render the whole handoff as a code block.
    assert!(
        body.lines().all(|l| !l.starts_with("    ")),
        "no indented lines:\n{body}"
    );
```

Add a prompt test:

```rust
#[test]
fn the_rotate_prompt_arms_via_ws_rotate_and_names_the_clear_step() {
    let env = Env::new();
    let claude = env.fake_claude();
    env.cmd().env("WS_CLAUDE_BIN", &claude).arg("setup").assert().success();
    let body =
        std::fs::read_to_string(env.home.path().join(".claude/commands/ws/rotate.md")).unwrap();
    assert!(body.contains("ws -rotate"), "writes through the command that arms: {body}");
    assert!(body.contains("Conversation-only facts"), "{body}");
    assert!(body.contains("measured") && body.contains("assumed"), "{body}");
    assert!(body.contains("Type /clear to continue"), "tells the agent the user's step: {body}");
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test --test task_rotate rotate`
Expected: FAIL on the missing `## Next step` heading and on the prompt text.

- [ ] **Step 3: Replace the template** in `commands::rotate`. Use `\` line continuations as below. The current literal has no continuations, so every section line in today's handoffs starts with nine spaces of indentation:

```rust
    let body = format!(
        "# Handoff — {n}\n\n\
         - **Written:** {ts}\n\
         - **By:** {actor}\n\
         - **Agent:** {agent}\n\
         - **Session:** {session}\n\
         - **Objective:** {objective}\n\n\
         ## Next step\n\n\
         <!-- the first action, with every fact it needs: command, path, branch -->\n\n\
         ## Conversation-only facts\n\n\
         <!-- exact readings, ids, counts, event order, the user's own words; \
         anything that exists nowhere but in the conversation being replaced -->\n\n\
         ## Where things stand\n\n\
         <!-- done / in progress / blocked; label each claim measured or assumed -->\n\n\
         ## Rejected alternatives\n\n\
         <!-- what was tried or considered and why it lost -->\n\n\
         ## Watch out for\n\n\
         <!-- anything that would mislead someone reading only the code -->\n"
    );
```

- [ ] **Step 4: Rewrite `src/assets/prompts/rotate.md`** (keep the frontmatter):

```markdown
---
model: claude-sonnet-5
---

Rotate this ws conversation: write a handoff a fresh conversation can continue from.

1. Run `ws -rotate`. It writes a skeleton under `.ws/handoffs/` and arms it, so the next fresh conversation in this workspace starts from it.
2. Before filling it in, list every fact this conversation produced that is written nowhere else: ids, exact readings and error text, counts, the order things happened, what the user said, asked for or corrected. Those go in **Conversation-only facts**, word for word. A fact that also lives in a commit, spec or notebook gets a path, not a restatement.
3. Fill every section of the skeleton:
   - **Next step**: the first action, with every fact it needs (command, path, branch), even if it is also written elsewhere. If it needs a clean worktree, make its first step "commit the handoff".
   - **Where things stand**: label each claim `measured` (you ran it and saw the output; include the command) or `assumed`.
   - **Rejected alternatives**: what lost and why, so it is not re-tried.
   - **Watch out for**: traps and decisions already made.
4. Keep the user's words close to verbatim. Condense your own.
5. Never copy a credential or personal data into the handoff. Name the secret, not its value.
6. Append fresh findings to your own notebook (`.ws/notebook/notebook.<actor>.md`; `ws -whoami` names the actor).

End by telling the user the handoff path. Your last line to them is exactly: "Type /clear to continue in a fresh conversation." On Codex, say instead: "Open the workspace again with `ws <name>` to continue."
```

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test --test task_rotate && cargo test --bin ws prompts::`
Expected: all pass. `prompts::tests::install_writes_all_namespaced_prompts` still finds "handoff".

- [ ] **Step 6: Commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings
git add src/commands.rs src/assets/prompts/rotate.md tests/task_rotate.rs
git commit -m "feat(rotation): handoff template and /ws:rotate carry the facts a successor cannot look up"
```

---

### Task 6: Docs, full gates, real-binary check

**Files:**
- Modify: `CHANGELOG.md` (Unreleased section)
- Modify: `README.md` (rotation section; `rotate_nudge` in the config table)
- Modify: `src/assets/context-template.md`, only if it describes rotation as "relaunch with `--handoff`"

- [ ] **Step 1: CHANGELOG entry** under `## Unreleased`:

```markdown
### Added
- Rotation without leaving the agent. `/ws:rotate` writes the handoff through `ws -rotate`, which now arms it; type `/clear` and the fresh conversation starts from it. A marker older than 6 hours is dropped.
- The Stop hook asks once per conversation to rotate when context passes `rotate_nudge` (default 65%; `ws config set rotate_nudge 0` turns it off). Claude only.

### Changed
- The handoff template leads with the next step and adds Conversation-only facts and Rejected alternatives.
```

- [ ] **Step 2: Full gates**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all green. Record the test count in the notebook.

- [ ] **Step 3: Real-binary check under a pty** (memory: unit tests never exercise the real launch path)

```bash
cargo build --release
WS=./target/release/ws
# in a scratch workspace:
$WS -adopt scratch-rot  # from a temp dir
echo '{"session_id":"s1","context_window":{"used_percentage":70}}' | WS_WORKSPACE=scratch-rot WS_DIR=$PWD $WS statusline
echo '{"session_id":"s1"}' | WS_WORKSPACE=scratch-rot WS_DIR=$PWD $WS internal stop   # expect the block
WS_WORKSPACE=scratch-rot $WS -rotate
echo '{"source":"clear","session_id":"s2"}' | WS_WORKSPACE=scratch-rot WS_DIR=$PWD $WS internal session-start  # expect the handoff pointer
```

Then run one real `ws scratch-rot` Claude session: `/ws:rotate`, then `/clear`, and confirm that the first reply reads the handoff.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md README.md src/assets/context-template.md
git commit -m "docs: context-aware rotation"
```

Release (version bump, tag, signing) is a separate step that follows `docs/releasing.md` and the release-flow traps in memory. It is not part of this plan.
