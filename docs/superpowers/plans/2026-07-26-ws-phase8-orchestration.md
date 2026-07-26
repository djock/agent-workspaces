# ws Phase 8 — Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `ws` the orchestration layer from spec §12 — git-worktree workspaces, a durable task queue with an explicitly-started headless drain, inter-workspace mail, tmux spawn, and the actor commands (`-whoami`, `-who`).

**Architecture:** Four new leaf modules (`queue.rs`, `drain.rs`, `mail.rs`, `worktree.rs`) plus a `spawn.rs`, all built on existing primitives: `atomic::atomic_write` for every shared-file write, `timeline::record` for events, `registry` + `lock` (checked variants) for anything irreversible. The queue is an append-only JSONL event log folded into current state — the same shape as `timeline.jsonl`, which merges cleanly under the union-merge gitattributes that worktrees rely on. The `Agent` trait grows one method, `headless()`, so the drain runs whichever agent the workspace already uses.

**Tech Stack:** Rust 2021, single crate, binary `ws`. serde_json, anyhow, uuid, tempfile (dev). External: `git`, `tmux` 3.x, `claude` 2.1.x, `codex` 0.145.x.

## Global Constraints

Every task's requirements implicitly include this section.

- **cargo is NOT on PATH.** Every cargo invocation must be prefixed `. "$HOME/.cargo/env";`.
- **Zero warnings** in both `cargo build` and `cargo test --no-run`, at every commit. A new warning is a review finding, not a nit.
- **All 290 existing tests keep passing.** Tests this plan intentionally changes: exactly two, both named in Tasks 2 and 3. Changing any other existing test is a review finding.
- **Every shared-file write goes through `crate::atomic::atomic_write`** (or `atomic_write_with_mode`). There is exactly ONE `fs::rename` in the crate, in `src/atomic.rs`. Appends to JSONL logs use `OpenOptions::append` exactly as `src/timeline.rs` already does — that is the established exception and it stays confined to append-only logs.
- **Vocabulary: "workspace", never "session"** in user-facing strings. Metadata dir is `.ws/`.
- **Agents are claude and codex only.** No gemini — no flag, no match arm, no mention.
- Tests that mutate process-global env (`HOME`, `XDG_CONFIG_HOME`, `WS_ROOT`, `TMUX`) must serialize on an explicit module-level `static TEST_LOCK: Mutex<()>` whose guard covers the whole body **including `TempDir` teardown**. Copy the pattern from `src/registry.rs`.
- **Never assert the absence of a short substring** the code echoes back. macOS temp paths contain `folders`, which contains `old`. This has bitten four times.
- Every new test must **discriminate**, not merely pass: ask "would this fail if the behavior regressed?" A test that asserts a string is present, which a neighbouring feature also happens to print, is not a test.
- `git add` **explicit paths only**. Never `git add -A`.
- Do **not** bump `Cargo.toml`'s version and do **not** touch `CHANGELOG.md`. Releases are cut separately.

## Safety Model for Unattended Execution

The queue runs an agent with no human watching. This section is normative — it is not advice, and an implementer may not soften it.

1. **ws never escalates permissions on the user's behalf.** The drain invokes the agent with the same permission posture as an interactive launch. `--dangerously-skip-permissions`, `--permission-mode bypassPermissions`, `--dangerously-bypass-approvals-and-sandbox`, and `--dangerously-bypass-hook-trust` must not appear anywhere in this phase's code. If an agent stalls or exits because it needed approval, that is a **failure**, and failing is the correct outcome.
2. **A drain refuses to start if the workspace lock is held by a live process** — checked with `lock::live_pid_checked`, never the display-only `live_pid`. The drain holds the lock for its own duration so a second drain and an interactive launch cannot collide.
3. **A drain only ever starts explicitly**: `ws -queue drain` or `ws -spawn --task`. No hook, no session-start path, and no TUI key may start one.
4. **Circuit breaker: two consecutive failures stop the drain.** Remaining tasks stay pending, a `circuit-open` record is appended, and the next drain refuses to run until `ws -queue drain --reset`.
5. **A crashed task is never silently retried.** Task state is folded from an append-only log. A task left in `running` with no terminal record — a crash, a kill, a power loss — is marked `failed` by the next drain, not re-run. A half-completed task that gets re-run could repeat destructive work; a human deciding to re-queue it is cheap, and silent duplicate work is not.
6. **Failure is defined per agent, and an unreadable result is a failure**, never a success by default:
   - claude: non-zero exit, OR `is_error: true` in the `--output-format json` object, OR stdout that does not parse as that object.
   - codex: non-zero exit, OR the `--output-last-message` file missing or empty.

## File Structure

**New:**

| File | Responsibility |
|---|---|
| `src/queue.rs` | Task record type, append-only store, state fold. No process spawning. |
| `src/drain.rs` | Runs pending tasks through the workspace's agent; circuit breaker; journal. |
| `src/mail.rs` | Message type, send, list, unread-since-marker. |
| `src/spawn.rs` | tmux session/window creation. |
| `src/worktree.rs` | `git worktree add`/`remove`, `.ws/` bootstrap, `--merge`. |
| `tests/orchestration.rs` | End-to-end CLI tests for the new commands. |

**Modified:** `src/cli.rs` (new `Cmd` variants + parsing), `src/main.rs` (dispatch), `src/commands.rs` (`whoami`, `who`, thin command entry points), `src/agents/mod.rs` (`headless` on the trait), `src/agents/{claude,codex}.rs` (implementations), `src/internal.rs` (mail in `build_context`), `src/tui/detail.rs` (real queue/mail counts), `src/tui/render.rs:211` (render `?` for an unreadable count), `src/workspace.rs` (path accessors), `src/lib.rs` or `src/main.rs` module declarations.

---

### Task 1: Actors — `-whoami` and `-who`

`src/actors.rs` already provides `slugify` and `actor_slug`. What is missing is the two commands, and a workspace-scoped variant of the slug: `actor_slug()` shells out to `git config user.email` in the **process cwd**, which is wrong when `ws -whoami` is run from outside the workspace.

**Files:**
- Modify: `src/actors.rs` (add `actor_slug_in`, `who`)
- Modify: `src/cli.rs` (add `Cmd::Whoami`, `Cmd::Who { name: Option<String> }`)
- Modify: `src/main.rs` (dispatch)
- Modify: `src/commands.rs` (`whoami`, `who` entry points)

**Interfaces:**
- Consumes: `workspace::resolve`, `config::load`, `actors::slugify`.
- Produces:
  - `actors::actor_slug_in(dir: &Path) -> String`
  - `actors::who(ws_dir: &Path) -> anyhow::Result<Vec<(String, usize)>>` — (actor slug, commit count), most commits first.
  - `cli::Cmd::Whoami`, `cli::Cmd::Who { name: Option<String> }`

- [ ] **Step 1: Write the failing tests for `actor_slug_in`**

Add to the `tests` module at the bottom of `src/actors.rs`:

```rust
    #[test]
    fn actor_slug_in_reads_the_given_repos_email_not_the_cwds() {
        let td = TempDir::new().unwrap();
        let repo = td.path();
        run_git(repo, &["init", "-q"]);
        run_git(repo, &["config", "user.email", "Someone.Else@Example.COM"]);
        assert_eq!(actor_slug_in(repo), "someone-else-example-com");
    }

    #[test]
    fn actor_slug_in_falls_back_when_the_dir_is_not_a_repo() {
        let td = TempDir::new().unwrap();
        // No git repo here, and no user.email to find. The fallback must still
        // produce a usable slug rather than an empty string.
        let s = actor_slug_in(td.path());
        assert!(!s.is_empty());
        assert_eq!(s, s.to_lowercase());
    }
```

And at the top of that `tests` module:

```rust
    use tempfile::TempDir;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `. "$HOME/.cargo/env"; cargo test --lib actors 2>&1 | tail -20`
Expected: FAIL — `cannot find function actor_slug_in in this scope`.

- [ ] **Step 3: Implement `actor_slug_in` and refactor `actor_slug` onto it**

Replace the body of `actor_slug` in `src/actors.rs` and add the new function:

```rust
/// Actor slug for a specific directory: the git `user.email` configured there,
/// falling back to `$USER`. Taking the directory explicitly matters because
/// `ws -whoami <name>` may run from anywhere.
pub fn actor_slug_in(dir: &std::path::Path) -> String {
    if let Ok(o) = Command::new("git")
        .args(["config", "user.email"])
        .current_dir(dir)
        .output()
    {
        if o.status.success() {
            let email = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !email.is_empty() {
                return slugify(&email);
            }
        }
    }
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() {
            return slugify(&u);
        }
    }
    "unknown".to_string()
}

pub fn actor_slug() -> String {
    match std::env::current_dir() {
        Ok(d) => actor_slug_in(&d),
        Err(_) => "unknown".to_string(),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test --lib actors 2>&1 | tail -20`
Expected: PASS, 4 tests.

- [ ] **Step 5: Write the failing test for `who`**

Add to the same `tests` module:

```rust
    #[test]
    fn who_ranks_actors_by_commit_count_in_the_ws_dir() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        run_git(root, &["init", "-q"]);
        std::fs::create_dir_all(root.join(".ws/notebook")).unwrap();

        // Two commits from alice, one from bob, all touching .ws/.
        for (i, (name, email)) in [
            ("Alice", "alice@example.com"),
            ("Alice", "alice@example.com"),
            ("Bob", "bob@example.com"),
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(root.join(format!(".ws/notebook/n{i}.md")), "x").unwrap();
            run_git(root, &["config", "user.name", name]);
            run_git(root, &["config", "user.email", email]);
            run_git(root, &["add", ".ws"]);
            run_git(root, &["commit", "-q", "-m", "note"]);
        }
        // A commit outside .ws/ must not count.
        std::fs::write(root.join("unrelated.txt"), "x").unwrap();
        run_git(root, &["config", "user.email", "carol@example.com"]);
        run_git(root, &["add", "unrelated.txt"]);
        run_git(root, &["commit", "-q", "-m", "unrelated"]);

        let ranked = who(&root.join(".ws")).unwrap();
        assert_eq!(ranked, vec![("alice-example-com".to_string(), 2), ("bob-example-com".to_string(), 1)]);
    }

    #[test]
    fn who_on_a_non_repo_is_an_error_not_an_empty_list() {
        // An empty list means "nobody has worked here", which is a real answer.
        // "I could not read the history" is a different answer and must not be
        // flattened into the first one.
        let td = TempDir::new().unwrap();
        assert!(who(&td.path().join(".ws")).is_err());
    }
```

- [ ] **Step 6: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib actors 2>&1 | tail -20`
Expected: FAIL — `cannot find function who`.

- [ ] **Step 7: Implement `who`**

Add to `src/actors.rs`:

```rust
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::Path;

/// Actors who have committed to `ws_dir`, ranked by commit count (descending,
/// then by slug for a stable order). Errors when the history cannot be read —
/// "unreadable" must not be reported to the user as "nobody".
pub fn who(ws_dir: &Path) -> Result<Vec<(String, usize)>> {
    let repo = match ws_dir.parent() {
        Some(p) => p,
        None => bail!("{} has no parent directory", ws_dir.display()),
    };
    let out = Command::new("git")
        .args(["log", "--format=%ae", "--"])
        .arg(ws_dir)
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        bail!(
            "cannot read git history for {}: {}",
            ws_dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let email = line.trim();
        if email.is_empty() {
            continue;
        }
        *counts.entry(slugify(email)).or_insert(0) += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(ranked)
}
```

- [ ] **Step 8: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib actors 2>&1 | tail -20`
Expected: PASS, 6 tests.

- [ ] **Step 9: Write the failing CLI parse tests**

Add to the `tests` module in `src/cli.rs` (match the existing test style there):

```rust
    #[test]
    fn parses_whoami_and_who() {
        assert_eq!(p(&["-whoami"]), Cmd::Whoami);
        assert_eq!(p(&["-who"]), Cmd::Who { name: None });
        assert_eq!(p(&["-who", "proj"]), Cmd::Who { name: Some("proj".into()) });
    }

    #[test]
    fn who_rejects_a_second_name() {
        assert!(parse(vec!["-who".into(), "a".into(), "b".into()]).is_err());
    }
```

If a helper named `p` does not already exist in that module, add:

```rust
    fn p(args: &[&str]) -> Cmd {
        parse(args.iter().map(|s| s.to_string()).collect()).unwrap()
    }
```

- [ ] **Step 10: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: FAIL — no variant `Whoami`.

- [ ] **Step 11: Add the variants, parsing, and dispatch**

In `src/cli.rs`, add to `enum Cmd`:

```rust
    Whoami,
    Who { name: Option<String> },
```

In `parse`, add arms next to `"-doctor"`:

```rust
        "-whoami" => {
            if it.next().is_some() {
                bail!("usage: ws -whoami");
            }
            Ok(Cmd::Whoami)
        }
        "-who" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -who [<name>]");
            }
            Ok(Cmd::Who { name })
        }
```

In `src/commands.rs`:

```rust
pub fn whoami() -> Result<()> {
    let dir = std::env::current_dir()?;
    println!("{}", crate::actors::actor_slug_in(&dir));
    Ok(())
}

pub fn who(name: Option<String>) -> Result<()> {
    // current_or_named is the established resolution for "this workspace or the
    // named one": it honours $WS_WORKSPACE, which matters because -who is most
    // often run from inside an agent session. Every command in this phase that
    // takes an optional workspace name uses it — do not hand-roll a second path.
    let (_name, root) = current_or_named(name)?;
    let ranked = crate::actors::who(&root.join(".ws"))?;
    if ranked.is_empty() {
        println!("no commits to .ws/ yet");
        return Ok(());
    }
    for (actor, n) in ranked {
        println!("{actor}  {n}");
    }
    Ok(())
}
```

In `src/main.rs`, add to the dispatch match:

```rust
        Cmd::Whoami => commands::whoami()?,
        Cmd::Who { name } => commands::who(name)?,
```

Also add both to `print_help`'s body, in the same style as the neighbouring lines:

```
  ws -whoami              print your actor slug
  ws -who [<name>]        actors who have worked in a workspace
```

- [ ] **Step 12: Run the full suite and check for warnings**

Run: `. "$HOME/.cargo/env"; cargo test 2>&1 | grep -E "^test result|^error|warning" | tail -30`
Expected: all suites pass, no `warning` lines.

- [ ] **Step 13: Commit**

```bash
git add src/actors.rs src/cli.rs src/commands.rs src/main.rs
git commit -m "feat(actors): ws -whoami and ws -who"
```

---

### Task 2: Mail

Messages are individual JSON files in the target's `.ws/mail/` — separate files never conflict when two worktrees merge, which is why this is not a JSONL log. "Unread" is computed against a single marker in `.ws/local/`, so surfacing N messages at session start costs **one** `atomic_write`, not N. This matters: `atomic_write` fsyncs twice (~3.4 ms) and this runs in a hook path.

**Files:**
- Create: `src/mail.rs`
- Modify: `src/workspace.rs` (add `mail_dir`, `mail_seen`)
- Modify: `src/cli.rs`, `src/main.rs`, `src/commands.rs`
- Modify: `src/internal.rs` (`build_context`)
- Modify: `src/tui/detail.rs`, `src/tui/render.rs`

**Interfaces:**
- Consumes: `atomic::atomic_write`, `actors::actor_slug_in` (Task 1), `timeline::record`, `workspace::resolve`.
- Produces:
  - `mail::Message { id: String, from: String, ts: String, body: String }`
  - `mail::send(mail_dir: &Path, from: &str, body: &str) -> anyhow::Result<String>` (returns id)
  - `mail::all(mail_dir: &Path) -> anyhow::Result<Vec<Message>>` (ascending by id)
  - `mail::unread(mail_dir: &Path, seen_marker: &Path) -> anyhow::Result<Vec<Message>>`
  - `mail::mark_seen(seen_marker: &Path, upto_id: &str) -> anyhow::Result<()>`
  - `workspace::Workspace::mail_dir()`, `workspace::Workspace::mail_seen()`

**Cross-task note:** Task 3 also edits `src/tui/detail.rs`. This task owns the **`mail`** field only; Task 3 owns the **`queue`** field only. Neither may restructure the other's line.

- [ ] **Step 1: Write the failing tests for the store**

Create `src/mail.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn send_then_all_round_trips_in_order() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("mail");
        let a = send(&dir, "alice", "first").unwrap();
        let b = send(&dir, "bob", "second").unwrap();
        assert_ne!(a, b, "each message gets its own id");

        let msgs = all(&dir).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body, "first");
        assert_eq!(msgs[0].from, "alice");
        assert_eq!(msgs[1].body, "second");
        assert!(msgs[0].id < msgs[1].id, "ids sort in send order");
    }

    #[test]
    fn all_on_a_missing_dir_is_empty_but_all_on_a_corrupt_file_is_an_error() {
        let td = TempDir::new().unwrap();
        // Never-written mailbox: genuinely empty.
        assert!(all(&td.path().join("nope")).unwrap().is_empty());

        // Corrupt message: must not be silently dropped, because "you have no
        // mail" and "one of your messages is unreadable" are different answers.
        let dir = td.path().join("mail");
        send(&dir, "alice", "first").unwrap();
        std::fs::write(dir.join("99999-garbage.json"), "{not json").unwrap();
        assert!(all(&dir).is_err());
    }

    #[test]
    fn unread_is_everything_after_the_marker() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("mail");
        let marker = td.path().join("mail-seen");

        let a = send(&dir, "alice", "first").unwrap();
        send(&dir, "bob", "second").unwrap();
        assert_eq!(unread(&dir, &marker).unwrap().len(), 2, "no marker: all unread");

        mark_seen(&marker, &a).unwrap();
        let u = unread(&dir, &marker).unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].body, "second", "the marked message is excluded, the later one is not");

        let c = send(&dir, "carol", "third").unwrap();
        assert_eq!(unread(&dir, &marker).unwrap().len(), 2, "new mail after the marker is unread again");
        mark_seen(&marker, &c).unwrap();
        assert!(unread(&dir, &marker).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_marker_means_error_not_all_unread() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("mail");
        send(&dir, "alice", "first").unwrap();
        let marker = td.path().join("marker-dir");
        std::fs::create_dir_all(&marker).unwrap(); // a directory: read fails
        assert!(unread(&dir, &marker).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib mail 2>&1 | tail -20`
Expected: FAIL — the module is not declared, then unresolved names.

- [ ] **Step 3: Implement the store**

Prepend to `src/mail.rs`:

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::atomic::atomic_write;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Sortable id: "<epoch-millis>-<uuid>". Sorting ids sorts by send time,
    /// which is what the unread marker relies on.
    pub id: String,
    pub from: String,
    pub ts: String,
    pub body: String,
}

fn new_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Zero-padded so lexicographic order matches numeric order past the year 5138.
    format!("{millis:015}-{}", uuid::Uuid::new_v4())
}

/// Write one message into `mail_dir`. Returns its id.
pub fn send(mail_dir: &Path, from: &str, body: &str) -> Result<String> {
    std::fs::create_dir_all(mail_dir)
        .with_context(|| format!("cannot create {}", mail_dir.display()))?;
    let msg = Message {
        id: new_id(),
        from: from.to_string(),
        ts: crate::now_iso(),
        body: body.to_string(),
    };
    let path = mail_dir.join(format!("{}.json", msg.id));
    atomic_write(&path, serde_json::to_vec_pretty(&msg)?)?;
    Ok(msg.id)
}

/// All messages, ascending by id. A missing mailbox is empty; a corrupt message
/// is an error — the two must never collapse into each other.
pub fn all(mail_dir: &Path) -> Result<Vec<Message>> {
    let rd = match std::fs::read_dir(mail_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", mail_dir.display())),
    };
    let mut msgs = Vec::new();
    for entry in rd {
        let entry = entry.with_context(|| format!("cannot read {}", mail_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let msg: Message = serde_json::from_str(&raw)
            .with_context(|| format!("corrupt message {}", path.display()))?;
        msgs.push(msg);
    }
    msgs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(msgs)
}

/// Messages sent after the marked one. A missing marker means nothing has been
/// read yet; a marker that exists but cannot be read is an error.
pub fn unread(mail_dir: &Path, seen_marker: &Path) -> Result<Vec<Message>> {
    let seen = match std::fs::read_to_string(seen_marker) {
        Ok(s) => Some(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {}", seen_marker.display()))
        }
    };
    let msgs = all(mail_dir)?;
    Ok(match seen {
        None => msgs,
        Some(upto) => msgs.into_iter().filter(|m| m.id > upto).collect(),
    })
}

pub fn mark_seen(seen_marker: &Path, upto_id: &str) -> Result<()> {
    atomic_write(seen_marker, upto_id.as_bytes())
}
```

Declare the module where the other modules are declared (`src/main.rs` and/or `src/lib.rs` — match how `timeline` is declared):

```rust
mod mail;
```

- [ ] **Step 4: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib mail 2>&1 | tail -20`
Expected: PASS, 4 tests.

- [ ] **Step 5: Add the workspace path accessors**

In `src/workspace.rs`, inside `impl Workspace`, next to `notebook_dir`:

```rust
    pub fn mail_dir(&self) -> PathBuf {
        self.ws_dir().join("mail")
    }
    /// Marker for the newest message already surfaced. Lives under local/ because
    /// "what I have read" is per-checkout, not shared state to merge.
    pub fn mail_seen(&self) -> PathBuf {
        self.local_dir().join("mail-seen")
    }
```

- [ ] **Step 6: Write the failing CLI parse tests**

Add to the `tests` module in `src/cli.rs`:

```rust
    #[test]
    fn parses_msg_send_and_log() {
        assert_eq!(
            p(&["-msg", "proj", "ship it"]),
            Cmd::Msg(MsgCmd::Send { to: "proj".into(), body: "ship it".into() })
        );
        assert_eq!(p(&["-msg", "log"]), Cmd::Msg(MsgCmd::Log { name: None }));
        assert_eq!(p(&["-msg", "log", "proj"]), Cmd::Msg(MsgCmd::Log { name: Some("proj".into()) }));
    }

    #[test]
    fn msg_send_requires_a_body() {
        assert!(parse(vec!["-msg".into(), "proj".into()]).is_err());
    }
```

- [ ] **Step 7: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: FAIL — no `MsgCmd`.

- [ ] **Step 8: Add parsing, command, and dispatch**

In `src/cli.rs`:

```rust
#[derive(Debug, PartialEq)]
pub enum MsgCmd {
    Send { to: String, body: String },
    Log { name: Option<String> },
}
```

Add `Msg(MsgCmd)` to `enum Cmd`, and this arm to `parse`:

```rust
        "-msg" => parse_msg(it.collect()),
```

And the parser, beside `parse_tag`:

```rust
fn parse_msg(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let first = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ws -msg <name> <body> | ws -msg log [<name>]"))?;
    if first == "log" {
        let name = it.next();
        if it.next().is_some() {
            bail!("usage: ws -msg log [<name>]");
        }
        return Ok(Cmd::Msg(MsgCmd::Log { name }));
    }
    let rest: Vec<String> = it.collect();
    if rest.is_empty() {
        bail!("usage: ws -msg <name> <body>");
    }
    Ok(Cmd::Msg(MsgCmd::Send { to: first, body: rest.join(" ") }))
}
```

In `src/commands.rs`:

```rust
pub fn msg(cmd: crate::cli::MsgCmd) -> Result<()> {
    use crate::cli::MsgCmd;
    let cfg = config::load();
    match cmd {
        MsgCmd::Send { to, body } => {
            let target = crate::workspace::resolve(&to, &cfg);
            if !target.exists() {
                anyhow::bail!("no workspace named {to}");
            }
            let from = crate::actors::actor_slug_in(&std::env::current_dir()?);
            let id = crate::mail::send(&target.mail_dir(), &from, &body)?;
            crate::timeline::record(
                &target.timeline(),
                "mail",
                &from,
                serde_json::json!({ "id": id }),
            )?;
            println!("sent to {to}");
            Ok(())
        }
        MsgCmd::Log { name } => {
            // Same resolution as every other optional-name command (Task 1).
            let (_n, root) = current_or_named(name)?;
            let msgs = crate::mail::all(&root.join(".ws/mail"))?;
            if msgs.is_empty() {
                println!("no mail");
                return Ok(());
            }
            for m in msgs {
                println!("{}  {}  {}", m.ts, m.from, m.body);
            }
            Ok(())
        }
    }
}
```

In `src/main.rs`: `Cmd::Msg(c) => commands::msg(c)?,`

Add to `print_help`:

```
  ws -msg <name> <body>   send a message to another workspace
  ws -msg log [<name>]    read the message history
```

- [ ] **Step 9: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 10: Write the failing test for session-start surfacing**

Add to the `tests` module in `src/internal.rs` (create one following the file's existing style if absent):

```rust
    #[test]
    fn build_context_lists_unread_mail_and_says_nothing_when_there_is_none() {
        let td = TempDir::new().unwrap();
        let ws = Workspace { name: "proj".into(), root: td.path().to_path_buf() };
        std::fs::create_dir_all(ws.ws_dir()).unwrap();

        let quiet = build_context(&ws);
        assert!(!quiet.contains("Unread mail"), "no mail: no mail section");

        crate::mail::send(&ws.mail_dir(), "alice", "please review the plan").unwrap();
        let loud = build_context(&ws);
        assert!(loud.contains("Unread mail (1)"), "count is shown: {loud}");
        assert!(loud.contains("please review the plan"), "body is shown: {loud}");
        assert!(loud.contains("alice"), "sender is shown: {loud}");
    }
```

- [ ] **Step 11: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib internal 2>&1 | tail -20`
Expected: FAIL — assertion `loud.contains("Unread mail (1)")`.

- [ ] **Step 12: Surface mail in `build_context`**

In `src/internal.rs`, inside `build_context`, after the notebook-file listing and before the closing `Protocol:` push:

```rust
    // Unread mail. A read error is reported, not swallowed: silently showing an
    // empty mailbox would be indistinguishable from having no mail.
    match crate::mail::unread(&ws.mail_dir(), &ws.mail_seen()) {
        Ok(msgs) if !msgs.is_empty() => {
            s.push_str(&format!("Unread mail ({}):\n", msgs.len()));
            for m in &msgs {
                s.push_str(&format!("- from {}: {}\n", m.from, m.body));
            }
            s.push('\n');
        }
        Ok(_) => {}
        Err(e) => {
            s.push_str(&format!("Mail could not be read ({e}); check .ws/mail/.\n\n"));
        }
    }
```

Then in `session_start`, after `let context = build_context(&ws);` and before the `println!`, mark the surfaced mail as seen — one write, regardless of message count:

```rust
    // One atomic_write per session start, not one per message: this is a hook
    // path and atomic_write fsyncs twice.
    if let Ok(msgs) = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen()) {
        if let Some(newest) = msgs.last() {
            let _ = crate::mail::mark_seen(&ws.mail_seen(), &newest.id);
        }
    }
```

- [ ] **Step 13: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib internal 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 14: Make the TUI mail count real**

This changes the `mail` field's type. In `src/tui/detail.rs`, change the struct field:

```rust
    /// Unread count, or None when the mailbox could not be read. Rendering "?"
    /// for unreadable beats rendering "0", which would be a lie.
    pub mail: Option<usize>,
```

and its construction in the `Detail { .. }` literal:

```rust
        mail: crate::mail::unread(&ws.join("mail"), &ws.join("local/mail-seen"))
            .ok()
            .map(|m| m.len()),
```

In `src/tui/render.rs:211`, replace the format line:

```rust
        format!(
            "queue {}   mail {}",
            det.queue,
            det.mail.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
        ),
```

**This is one of the two existing tests this plan changes.** In `src/tui/detail.rs`, the test `counts_queue_and_mail_when_phase_8_creates_them` writes `mail/msg.json` containing `"x"` and asserts `det.mail == 1`. Replace its mail half so it exercises the real format, and add a discrimination case:

```rust
    #[test]
    fn counts_unread_mail_and_reports_unreadable_as_none() {
        let td = TempDir::new().unwrap();
        let ws = td.path().join(".ws");
        std::fs::create_dir_all(ws.join("local")).unwrap();
        crate::mail::send(&ws.join("mail"), "alice", "hi").unwrap();
        let det = build(&ws_at(td.path().to_path_buf()), 5);
        assert_eq!(det.mail, Some(1));

        std::fs::write(ws.join("mail/bad.json"), "{not json").unwrap();
        let det = build(&ws_at(td.path().to_path_buf()), 5);
        assert_eq!(det.mail, None, "a corrupt message reads as unknown, not as zero");
    }
```

Also update `assert_eq!(det.mail, 0);` in the empty-workspace test to `assert_eq!(det.mail, Some(0));`.

- [ ] **Step 15: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test 2>&1 | grep -E "^test result|^error|warning" | tail -30`
Expected: all pass, no warnings.

- [ ] **Step 16: Commit**

```bash
git add src/mail.rs src/workspace.rs src/cli.rs src/commands.rs src/main.rs src/internal.rs src/tui/detail.rs src/tui/render.rs
git commit -m "feat(mail): inter-workspace messages surfaced at session start"
```

---

### Task 3: Queue store

Append-only JSONL folded into current state, mirroring `timeline.jsonl`. No process is spawned in this task — that is Task 4, and keeping them apart is what lets a reviewer reject the runner without rejecting the store.

**Files:**
- Create: `src/queue.rs`
- Modify: `src/workspace.rs` (add `queue_dir`, `queue_tasks`, `queue_journal`, `circuit_marker`)
- Modify: `src/cli.rs`, `src/main.rs`, `src/commands.rs`
- Modify: `src/tui/detail.rs`, `src/tui/render.rs`

**Interfaces:**
- Consumes: `timeline`-style append, `actors::actor_slug_in`.
- Produces:
  - `queue::TaskState { Pending, Running, Done, Failed }`
  - `queue::Task { id: String, text: String, state: TaskState, added: String, note: Option<String> }`
  - `queue::add(tasks_path: &Path, text: &str, actor: &str) -> anyhow::Result<String>`
  - `queue::set_state(tasks_path: &Path, id: &str, state: TaskState, note: Option<&str>) -> anyhow::Result<()>`
  - `queue::tasks(tasks_path: &Path) -> anyhow::Result<Vec<Task>>` (add order)
  - `queue::pending(tasks_path: &Path) -> anyhow::Result<Vec<Task>>`
  - `queue::reap_orphans(tasks_path: &Path) -> anyhow::Result<usize>` — marks `Running` tasks `Failed`; returns how many.
  - `cli::Cmd::Queue(QueueCmd)` with `Add`, `List`, `Drain` (Drain is parsed here, implemented in Task 4)

**Cross-task note:** Task 2 also edits `src/tui/detail.rs` and `src/tui/render.rs:211`. This task owns the **`queue`** field only.

**Storage decision — resolves a spec/code conflict.** Spec §12 says the queue is `tasks.jsonl`; `src/tui/detail.rs:84` currently *counts files* in `.ws/queue/`. A single JSONL file would make that pane read `queue 1` forever. The JSONL wins (it is what the spec specifies, it appends atomically, and it union-merges across worktrees) and the TUI is corrected here to count **pending tasks**. Path: `.ws/queue/tasks.jsonl`.

- [ ] **Step 1: Write the failing tests for add/fold**

Create `src/queue.rs` with this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn q(td: &TempDir) -> std::path::PathBuf {
        td.path().join("queue/tasks.jsonl")
    }

    #[test]
    fn added_tasks_are_pending_in_add_order() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "first task", "alice").unwrap();
        let b = add(&p, "second task", "alice").unwrap();
        assert_ne!(a, b);

        let ts = tasks(&p).unwrap();
        assert_eq!(ts.len(), 2);
        assert_eq!(ts[0].text, "first task");
        assert_eq!(ts[1].text, "second task");
        assert!(ts.iter().all(|t| t.state == TaskState::Pending));
        assert_eq!(pending(&p).unwrap().len(), 2);
    }

    #[test]
    fn the_last_state_record_wins_and_pending_shrinks() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "first", "alice").unwrap();
        add(&p, "second", "alice").unwrap();

        set_state(&p, &a, TaskState::Running, None).unwrap();
        assert_eq!(tasks(&p).unwrap()[0].state, TaskState::Running);
        assert_eq!(pending(&p).unwrap().len(), 1, "running is not pending");

        set_state(&p, &a, TaskState::Done, Some("finished cleanly")).unwrap();
        let ts = tasks(&p).unwrap();
        assert_eq!(ts[0].state, TaskState::Done);
        assert_eq!(ts[0].note.as_deref(), Some("finished cleanly"));
        assert_eq!(pending(&p).unwrap().len(), 1);
    }

    #[test]
    fn a_state_record_for_an_unknown_id_is_ignored_not_a_phantom_task() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        add(&p, "real", "alice").unwrap();
        set_state(&p, "no-such-id", TaskState::Done, None).unwrap();
        assert_eq!(tasks(&p).unwrap().len(), 1);
    }

    #[test]
    fn a_missing_queue_is_empty_but_a_corrupt_line_is_an_error() {
        let td = TempDir::new().unwrap();
        assert!(tasks(&td.path().join("queue/tasks.jsonl")).unwrap().is_empty());

        let p = q(&td);
        add(&p, "real", "alice").unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{{not json").unwrap();
        // A queue that runs an agent unattended must never guess at its own
        // contents. Half a queue silently presented as the whole queue is how
        // a drain skips work the user asked for.
        assert!(tasks(&p).is_err());
    }

    #[test]
    fn reap_orphans_fails_running_tasks_and_leaves_the_rest_alone() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "crashed", "alice").unwrap();
        let b = add(&p, "untouched", "alice").unwrap();
        set_state(&p, &a, TaskState::Running, None).unwrap();

        assert_eq!(reap_orphans(&p).unwrap(), 1);
        let ts = tasks(&p).unwrap();
        assert_eq!(ts[0].state, TaskState::Failed, "a crashed task is failed, never retried");
        assert_eq!(ts[1].state, TaskState::Pending, "an untouched task is left pending");
        assert_eq!(pending(&p).unwrap()[0].id, b);

        assert_eq!(reap_orphans(&p).unwrap(), 0, "reaping twice is a no-op");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib queue 2>&1 | tail -20`
Expected: FAIL — unresolved names.

- [ ] **Step 3: Implement the store**

Prepend to `src/queue.rs`:

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub text: String,
    pub state: TaskState,
    pub added: String,
    pub note: Option<String>,
}

/// One line of the log. `add` records carry the text; `state` records carry a
/// transition. Current state is the left fold of the whole file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Record {
    Add { ts: String, id: String, text: String, actor: String },
    State { ts: String, id: String, state: TaskState, note: Option<String> },
}

fn append(tasks_path: &Path, rec: &Record) -> Result<()> {
    if let Some(dir) = tasks_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut line = serde_json::to_string(rec)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tasks_path)
        .with_context(|| format!("cannot open {}", tasks_path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("cannot append to {}", tasks_path.display()))?;
    Ok(())
}

pub fn add(tasks_path: &Path, text: &str, actor: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    append(
        tasks_path,
        &Record::Add {
            ts: crate::now_iso(),
            id: id.clone(),
            text: text.to_string(),
            actor: actor.to_string(),
        },
    )?;
    Ok(id)
}

pub fn set_state(tasks_path: &Path, id: &str, state: TaskState, note: Option<&str>) -> Result<()> {
    append(
        tasks_path,
        &Record::State {
            ts: crate::now_iso(),
            id: id.to_string(),
            state,
            note: note.map(str::to_string),
        },
    )
}

/// Fold the log into current task state, in add order. A missing file is an
/// empty queue; an unparseable line is an error.
pub fn tasks(tasks_path: &Path) -> Result<Vec<Task>> {
    let raw = match std::fs::read_to_string(tasks_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {}", tasks_path.display()))
        }
    };
    let mut out: Vec<Task> = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = serde_json::from_str(line).with_context(|| {
            format!("corrupt queue record at {}:{}", tasks_path.display(), i + 1)
        })?;
        match rec {
            Record::Add { ts, id, text, .. } => out.push(Task {
                id,
                text,
                state: TaskState::Pending,
                added: ts,
                note: None,
            }),
            Record::State { id, state, note, .. } => {
                // An id we have never seen is ignored rather than invented: a
                // state record cannot conjure a task with no text.
                if let Some(t) = out.iter_mut().find(|t| t.id == id) {
                    t.state = state;
                    if note.is_some() {
                        t.note = note;
                    }
                }
            }
        }
    }
    Ok(out)
}

pub fn pending(tasks_path: &Path) -> Result<Vec<Task>> {
    Ok(tasks(tasks_path)?
        .into_iter()
        .filter(|t| t.state == TaskState::Pending)
        .collect())
}

/// Mark every `Running` task `Failed`. Called at the start of a drain: a task
/// still marked running when no drain holds the lock is one whose process died.
/// It is failed, never re-run — re-running a half-finished task could repeat
/// destructive work, and re-queueing by hand is cheap.
pub fn reap_orphans(tasks_path: &Path) -> Result<usize> {
    let orphans: Vec<String> = tasks(tasks_path)?
        .into_iter()
        .filter(|t| t.state == TaskState::Running)
        .map(|t| t.id)
        .collect();
    for id in &orphans {
        set_state(
            tasks_path,
            id,
            TaskState::Failed,
            Some("interrupted: no drain was holding the lock"),
        )?;
    }
    Ok(orphans.len())
}
```

Declare `mod queue;` beside `mod mail;`.

- [ ] **Step 4: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib queue 2>&1 | tail -20`
Expected: PASS, 5 tests.

- [ ] **Step 5: Add the workspace path accessors**

In `src/workspace.rs`, inside `impl Workspace`:

```rust
    pub fn queue_dir(&self) -> PathBuf {
        self.ws_dir().join("queue")
    }
    pub fn queue_tasks(&self) -> PathBuf {
        self.queue_dir().join("tasks.jsonl")
    }
    /// Drain journal: per-checkout run output, not shared state.
    pub fn queue_journal(&self) -> PathBuf {
        self.local_dir().join("queue-journal.log")
    }
    /// Present when the circuit breaker has tripped. Cleared by `--reset`.
    pub fn circuit_marker(&self) -> PathBuf {
        self.local_dir().join("queue-circuit-open")
    }
```

- [ ] **Step 6: Write the failing CLI parse tests**

Add to `src/cli.rs` tests:

```rust
    #[test]
    fn parses_queue_subcommands() {
        assert_eq!(
            p(&["-queue", "add", "proj", "write the docs"]),
            Cmd::Queue(QueueCmd::Add { name: "proj".into(), text: "write the docs".into() })
        );
        assert_eq!(p(&["-queue", "list", "proj"]), Cmd::Queue(QueueCmd::List { name: Some("proj".into()) }));
        assert_eq!(
            p(&["-queue", "drain", "proj"]),
            Cmd::Queue(QueueCmd::Drain { name: Some("proj".into()), reset: false })
        );
        assert_eq!(
            p(&["-queue", "drain", "proj", "--reset"]),
            Cmd::Queue(QueueCmd::Drain { name: Some("proj".into()), reset: true })
        );
    }

    #[test]
    fn queue_add_requires_a_target_and_text() {
        assert!(parse(vec!["-queue".into(), "add".into()]).is_err());
        assert!(parse(vec!["-queue".into(), "add".into(), "proj".into()]).is_err());
    }

    #[test]
    fn an_unknown_queue_subcommand_is_rejected() {
        assert!(parse(vec!["-queue".into(), "flush".into()]).is_err());
    }
```

- [ ] **Step 7: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: FAIL — no `QueueCmd`.

- [ ] **Step 8: Add parsing and the add/list commands**

In `src/cli.rs`:

```rust
#[derive(Debug, PartialEq)]
pub enum QueueCmd {
    Add { name: String, text: String },
    List { name: Option<String> },
    Drain { name: Option<String>, reset: bool },
}
```

Add `Queue(QueueCmd)` to `enum Cmd`, the arm `"-queue" => parse_queue(it.collect()),` to `parse`, and:

```rust
fn parse_queue(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ws -queue add|list|drain ..."))?;
    match sub.as_str() {
        "add" => {
            let name = it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -queue add <name> <text>"))?;
            let rest: Vec<String> = it.collect();
            if rest.is_empty() {
                bail!("usage: ws -queue add <name> <text>");
            }
            Ok(Cmd::Queue(QueueCmd::Add { name, text: rest.join(" ") }))
        }
        "list" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -queue list [<name>]");
            }
            Ok(Cmd::Queue(QueueCmd::List { name }))
        }
        "drain" => {
            let mut name = None;
            let mut reset = false;
            for a in it {
                match a.as_str() {
                    "--reset" => reset = true,
                    other if other.starts_with("--") => bail!("unexpected argument: {other}"),
                    other if name.is_none() => name = Some(other.to_string()),
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Queue(QueueCmd::Drain { name, reset }))
        }
        other => bail!("unknown queue subcommand: {other} (try add, list, drain)"),
    }
}
```

In `src/commands.rs` — note `Drain` is left explicitly unimplemented here and filled in by Task 4:

```rust
pub fn queue(cmd: crate::cli::QueueCmd) -> Result<()> {
    use crate::cli::QueueCmd;
    let cfg = config::load();
    match cmd {
        QueueCmd::Add { name, text } => {
            let ws = crate::workspace::resolve(&name, &cfg);
            if !ws.exists() {
                anyhow::bail!("no workspace named {name}");
            }
            let actor = crate::actors::actor_slug_in(&ws.root);
            crate::queue::add(&ws.queue_tasks(), &text, &actor)?;
            let n = crate::queue::pending(&ws.queue_tasks())?.len();
            println!("queued for {name} ({n} pending) — run `ws -queue drain {name}` to start");
            Ok(())
        }
        QueueCmd::List { name } => {
            // current_or_named: honours $WS_WORKSPACE, same as Task 1's -who.
            let (_n, root) = current_or_named(name)?;
            let tasks = crate::queue::tasks(&root.join(".ws/queue/tasks.jsonl"))?;
            if tasks.is_empty() {
                println!("queue is empty");
                return Ok(());
            }
            for t in tasks {
                let note = t.note.map(|n| format!("  ({n})")).unwrap_or_default();
                println!("{:<8} {}{}", t.state.as_str(), t.text, note);
            }
            Ok(())
        }
        QueueCmd::Drain { name, reset } => crate::drain::run(name, reset),
    }
}
```

Because `crate::drain::run` does not exist until Task 4, add a placeholder `src/drain.rs` **in this task** so the crate compiles, and make it explicit that it is a stub:

```rust
use anyhow::Result;

/// Implemented in Task 4. Kept as a distinct module so the queue store and the
/// unattended runner can be reviewed separately.
pub fn run(_name: Option<String>, _reset: bool) -> Result<()> {
    anyhow::bail!("ws -queue drain is not implemented yet")
}
```

Declare `mod drain;`. In `src/main.rs`: `Cmd::Queue(c) => commands::queue(c)?,`

Add to `print_help`:

```
  ws -queue add <name> <text>   add a task to a workspace's queue
  ws -queue list [<name>]       show the queue
  ws -queue drain [<name>]      run pending tasks through the agent
```

- [ ] **Step 9: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 10: Make the TUI queue count real**

**This is the second of the two existing tests this plan changes.** In `src/tui/detail.rs`:

```rust
    /// Pending task count, or None when the queue could not be read.
    pub queue: Option<usize>,
```

```rust
        queue: crate::queue::pending(&ws.join("queue/tasks.jsonl")).ok().map(|t| t.len()),
```

In `src/tui/render.rs:211`, the format line now reads (Task 2 already changed the mail half; keep it):

```rust
        format!(
            "queue {}   mail {}",
            det.queue.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            det.mail.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
        ),
```

Delete the now-obsolete file-counting `count_files` helper **only if nothing else uses it** — check with `grep -n count_files src/tui/detail.rs` and remove it if the only references were the two lines this plan replaced. Leaving an unused function is a warning, and warnings are a review finding.

Replace the queue half of `counts_queue_and_mail_when_phase_8_creates_them` (Task 2 already replaced the mail half; if that test no longer exists, this is a fresh test):

```rust
    #[test]
    fn counts_pending_queue_tasks_and_reports_a_corrupt_queue_as_none() {
        let td = TempDir::new().unwrap();
        let ws = td.path().join(".ws");
        std::fs::create_dir_all(ws.join("local")).unwrap();
        let tasks = ws.join("queue/tasks.jsonl");
        let a = crate::queue::add(&tasks, "one", "alice").unwrap();
        crate::queue::add(&tasks, "two", "alice").unwrap();
        assert_eq!(build(&ws_at(td.path().to_path_buf()), 5).queue, Some(2));

        crate::queue::set_state(&tasks, &a, crate::queue::TaskState::Done, None).unwrap();
        assert_eq!(
            build(&ws_at(td.path().to_path_buf()), 5).queue,
            Some(1),
            "a finished task is not pending"
        );

        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&tasks).unwrap();
        writeln!(f, "{{not json").unwrap();
        assert_eq!(build(&ws_at(td.path().to_path_buf()), 5).queue, None);
    }
```

Update `assert_eq!(det.queue, 0);` in the empty-workspace test to `assert_eq!(det.queue, Some(0));`, and `src/tui/render.rs:461`'s `assert!(text.contains("queue 0"))` still holds — verify it does rather than assuming.

- [ ] **Step 11: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test 2>&1 | grep -E "^test result|^error|warning" | tail -30`
Expected: all pass, no warnings.

- [ ] **Step 12: Commit**

```bash
git add src/queue.rs src/drain.rs src/workspace.rs src/cli.rs src/commands.rs src/main.rs src/tui/detail.rs src/tui/render.rs
git commit -m "feat(queue): append-only task store with folded state"
```

---

### Task 4: Headless drain

Replaces the Task 3 stub. **Re-read the Safety Model section before writing any code in this task** — every numbered rule in it is exercised by a test here.

**Files:**
- Modify: `src/drain.rs` (replace the stub)
- Modify: `src/agents/mod.rs` (add `headless` to the trait)
- Modify: `src/agents/claude.rs`, `src/agents/codex.rs`

**Interfaces:**
- Consumes: `queue::{tasks, pending, set_state, reap_orphans, TaskState}`, `lock::live_pid_checked`, `lock::acquire`/`release`, `timeline::record`, `contract::{read_session_id, write_session_id}`, `workspace::resolve`.
- Produces:
  - `agents::Agent::headless(&self, ws: &Workspace, prompt: &str, ctx: &LaunchCtx) -> anyhow::Result<Command>`
  - `agents::Agent::headless_succeeded(&self, out: &std::process::Output) -> bool`
  - `drain::run(name: Option<String>, reset: bool) -> anyhow::Result<()>`
  - `drain::Outcome { ran: usize, failed: usize, tripped: bool }`

**Verified against the live CLIs (2026-07-26) — do not re-derive these from memory:**
- `claude -p <prompt> --output-format json` prints one JSON object with `is_error` (bool), `subtype` (`"success"`), `session_id`, `result`. Chain follow-up tasks with `--resume <session_id>`.
- `codex exec <prompt> -C <dir> --json -o <file>` writes the final message to `<file>`; `codex exec resume <id> <prompt>` and `codex exec resume --last <prompt>` chain.

- [ ] **Step 1: Write the failing tests for the trait method**

Add to `src/agents/claude.rs`'s test module:

```rust
    #[test]
    fn headless_asks_for_json_and_never_escalates_permissions() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: td.path().to_path_buf() };
        let cmd = ClaudeAgent.headless(&ws, "do the thing", &ctx).unwrap();
        let a = args_of(&cmd);

        assert!(a.contains(&"-p".to_string()), "headless mode: {a:?}");
        assert!(a.contains(&"do the thing".to_string()), "prompt is passed: {a:?}");
        assert!(a.windows(2).any(|w| w[0] == "--output-format" && w[1] == "json"),
                "JSON result so failure is detectable: {a:?}");

        // The drain runs with nobody watching. Escalation flags must never
        // appear, and this assertion is the thing standing between an
        // unattended agent and the user's filesystem.
        for forbidden in ["--dangerously-skip-permissions", "--permission-mode",
                          "--allow-dangerously-skip-permissions"] {
            assert!(!a.iter().any(|x| x == forbidden), "{forbidden} must never be passed: {a:?}");
        }
        assert_eq!(env_of(&cmd, "WS_WORKSPACE").as_deref(), Some("proj"));
    }

    #[test]
    fn headless_resumes_the_recorded_session_on_the_second_task() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let ctx = LaunchCtx { fresh: false, sessions_root: td.path().to_path_buf() };
        crate::contract::write_session_id(&ws.state_toml(), "claude", "abc-123").unwrap();

        let a = args_of(&ClaudeAgent.headless(&ws, "next", &ctx).unwrap());
        assert!(a.windows(2).any(|w| w[0] == "--resume" && w[1] == "abc-123"),
                "chains onto the prior session: {a:?}");
    }

    #[test]
    fn success_requires_both_a_clean_exit_and_is_error_false() {
        use std::os::unix::process::ExitStatusExt;
        let ok = |code: i32, stdout: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        assert!(ClaudeAgent.headless_succeeded(&ok(0, r#"{"is_error":false,"subtype":"success"}"#)));
        assert!(!ClaudeAgent.headless_succeeded(&ok(1, r#"{"is_error":false,"subtype":"success"}"#)),
                "non-zero exit is a failure whatever the body says");
        assert!(!ClaudeAgent.headless_succeeded(&ok(0, r#"{"is_error":true,"subtype":"error"}"#)),
                "is_error true is a failure");
        assert!(!ClaudeAgent.headless_succeeded(&ok(0, "not json at all")),
                "output we cannot read is a failure, never an assumed success");
        assert!(!ClaudeAgent.headless_succeeded(&ok(0, "")), "empty output is a failure");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib agents 2>&1 | tail -20`
Expected: FAIL — no method `headless`.

- [ ] **Step 3: Add the trait methods and both implementations**

In `src/agents/mod.rs`, add to `trait Agent`:

```rust
    /// Build a non-interactive Command that runs `prompt` to completion.
    /// Implementations MUST NOT pass any permission-escalation flag: the drain
    /// runs unattended, and an agent that needed approval should fail, not proceed.
    fn headless(&self, ws: &Workspace, prompt: &str, ctx: &LaunchCtx) -> anyhow::Result<Command>;

    /// Whether a finished headless run counts as success. Unreadable output is
    /// always a failure.
    fn headless_succeeded(&self, out: &std::process::Output) -> bool;
```

In `src/agents/claude.rs`, inside `impl Agent for ClaudeAgent`:

```rust
    fn headless(&self, ws: &Workspace, prompt: &str, ctx: &LaunchCtx) -> anyhow::Result<Command> {
        let mut cmd = Command::new(self.binary());
        cmd.arg("-p").arg(prompt).arg("--output-format").arg("json");
        // Chain onto the prior session so a multi-task drain keeps its context.
        if !ctx.fresh {
            if let Some(id) = contract::read_session_id(&ws.state_toml(), self.id()) {
                cmd.arg("--resume").arg(id);
            }
        }
        cmd.current_dir(&ws.root)
            .env("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", ws.memory_dir())
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
        Ok(cmd)
    }

    fn headless_succeeded(&self, out: &std::process::Output) -> bool {
        if !out.status.success() {
            return false;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        match serde_json::from_str::<serde_json::Value>(text.trim()) {
            Ok(v) => v["is_error"].as_bool() == Some(false),
            Err(_) => false,
        }
    }
```

In `src/agents/codex.rs`:

```rust
    fn headless(&self, ws: &Workspace, prompt: &str, ctx: &LaunchCtx) -> anyhow::Result<Command> {
        let mut cmd = Command::new(self.binary());
        cmd.arg("exec");
        if !ctx.fresh && marker_present(ws) {
            cmd.arg("resume").arg("--last");
        }
        cmd.arg(prompt)
            .arg("-C")
            .arg(&ws.root)
            .arg("--color")
            .arg("never");
        cmd.current_dir(&ws.root)
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
        Ok(cmd)
    }

    fn headless_succeeded(&self, out: &std::process::Output) -> bool {
        // codex exec has no machine-readable success field on stdout; a clean
        // exit with some final output is the signal. Empty output means the run
        // produced nothing, which is a failure, not a quiet success.
        out.status.success() && !out.stdout.is_empty()
    }
```

Add the matching tests to `src/agents/codex.rs`'s test module:

```rust
    #[test]
    fn headless_uses_exec_and_never_bypasses_the_sandbox() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: td.path().to_path_buf() };
        let a = args(&CodexAgent.headless(&ws, "do the thing", &ctx).unwrap());
        assert_eq!(a.first().map(String::as_str), Some("exec"));
        assert!(a.contains(&"do the thing".to_string()), "{a:?}");
        for forbidden in ["--dangerously-bypass-approvals-and-sandbox",
                          "--dangerously-bypass-hook-trust", "-s", "--sandbox"] {
            assert!(!a.iter().any(|x| x == forbidden), "{forbidden} must never be passed: {a:?}");
        }
    }

    #[test]
    fn headless_resumes_when_a_marker_exists_and_not_when_fresh() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        record_marker(&ws).unwrap();

        let resumed = args(&CodexAgent
            .headless(&ws, "next", &LaunchCtx { fresh: false, sessions_root: td.path().into() })
            .unwrap());
        assert_eq!(resumed.get(1).map(String::as_str), Some("resume"), "{resumed:?}");

        let first = args(&CodexAgent
            .headless(&ws, "next", &LaunchCtx { fresh: true, sessions_root: td.path().into() })
            .unwrap());
        assert_ne!(first.get(1).map(String::as_str), Some("resume"), "{first:?}");
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib agents 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Write the failing tests for the drain core**

The drain must be testable without spawning a real agent, so the runnable core takes a closure. Replace `src/drain.rs`'s stub tests with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{self, TaskState};
    use tempfile::TempDir;

    fn setup() -> (TempDir, std::path::PathBuf) {
        let td = TempDir::new().unwrap();
        let tasks = td.path().join(".ws/queue/tasks.jsonl");
        std::fs::create_dir_all(td.path().join(".ws/local")).unwrap();
        (td, tasks)
    }

    #[test]
    fn drains_every_pending_task_in_order() {
        let (td, tasks) = setup();
        queue::add(&tasks, "one", "alice").unwrap();
        queue::add(&tasks, "two", "alice").unwrap();
        let seen = std::cell::RefCell::new(Vec::new());

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            seen.borrow_mut().push(t.text.clone());
            Ok(true)
        })
        .unwrap();

        assert_eq!(out.ran, 2);
        assert_eq!(out.failed, 0);
        assert!(!out.tripped);
        assert_eq!(*seen.borrow(), vec!["one".to_string(), "two".to_string()]);
        assert!(queue::tasks(&tasks).unwrap().iter().all(|t| t.state == TaskState::Done));
    }

    #[test]
    fn two_consecutive_failures_trip_the_breaker_and_leave_the_rest_pending() {
        let (td, tasks) = setup();
        for t in ["one", "two", "three", "four"] {
            queue::add(&tasks, t, "alice").unwrap();
        }
        let attempts = std::cell::Cell::new(0);

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |_| {
            attempts.set(attempts.get() + 1);
            Ok(false)
        })
        .unwrap();

        assert!(out.tripped, "breaker trips");
        assert_eq!(attempts.get(), 2, "stops after the second failure, does not attempt the rest");
        let ts = queue::tasks(&tasks).unwrap();
        assert_eq!(ts[0].state, TaskState::Failed);
        assert_eq!(ts[1].state, TaskState::Failed);
        assert_eq!(ts[2].state, TaskState::Pending, "untried tasks stay pending");
        assert_eq!(ts[3].state, TaskState::Pending);
    }

    #[test]
    fn a_success_between_two_failures_resets_the_breaker() {
        let (td, tasks) = setup();
        for t in ["fail", "ok", "fail2"] {
            queue::add(&tasks, t, "alice").unwrap();
        }
        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            Ok(t.text == "ok")
        })
        .unwrap();

        // Consecutive, not cumulative: 3 tasks, 2 failures, never 2 in a row.
        assert!(!out.tripped, "non-consecutive failures must not trip the breaker");
        assert_eq!(out.ran, 3);
        assert_eq!(out.failed, 2);
    }

    #[test]
    fn a_task_is_marked_running_before_it_executes() {
        let (td, tasks) = setup();
        queue::add(&tasks, "one", "alice").unwrap();
        let tasks2 = tasks.clone();

        drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            // Mid-flight, the log must already say running — that is what lets
            // reap_orphans recognise a crash instead of re-running the task.
            let live = queue::tasks(&tasks2).unwrap();
            assert_eq!(live.iter().find(|x| x.id == t.id).unwrap().state, TaskState::Running);
            Ok(true)
        })
        .unwrap();
    }

    #[test]
    fn the_journal_records_every_attempt() {
        let (td, tasks) = setup();
        queue::add(&tasks, "one", "alice").unwrap();
        queue::add(&tasks, "two", "alice").unwrap();
        let journal = td.path().join(".ws/local/journal.log");

        drive(&tasks, &journal, |t| Ok(t.text == "one")).unwrap();

        let log = std::fs::read_to_string(&journal).unwrap();
        assert!(log.contains("one"), "journal names the task: {log}");
        assert!(log.contains("two"), "journal names the task: {log}");
        assert!(log.contains("ok"), "journal records the outcome: {log}");
        assert!(log.contains("failed"), "journal records the outcome: {log}");
    }

    #[test]
    fn an_orphaned_running_task_is_failed_not_rerun() {
        let (td, tasks) = setup();
        let a = queue::add(&tasks, "crashed", "alice").unwrap();
        queue::add(&tasks, "fresh", "alice").unwrap();
        queue::set_state(&tasks, &a, TaskState::Running, None).unwrap();
        let seen = std::cell::RefCell::new(Vec::new());

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            seen.borrow_mut().push(t.text.clone());
            Ok(true)
        })
        .unwrap();

        assert_eq!(*seen.borrow(), vec!["fresh".to_string()], "the crashed task is NOT re-run");
        assert_eq!(out.ran, 1);
        assert_eq!(queue::tasks(&tasks).unwrap()[0].state, TaskState::Failed);
    }

    #[test]
    fn an_empty_queue_drains_to_nothing_without_error() {
        let (td, tasks) = setup();
        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |_| Ok(true)).unwrap();
        assert_eq!(out.ran, 0);
        assert!(!out.tripped);
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib drain 2>&1 | tail -20`
Expected: FAIL — no `drive`, no `Outcome`.

- [ ] **Step 7: Implement the drain core**

Replace the body of `src/drain.rs` above the tests:

```rust
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

use crate::agents;
use crate::config;
use crate::queue::{self, Task, TaskState};

/// Consecutive failures that stop a drain. See the plan's Safety Model.
const BREAKER_LIMIT: usize = 2;

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub ran: usize,
    pub failed: usize,
    pub tripped: bool,
}

fn journal(path: &Path, line: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot open journal {}", path.display()))?;
    writeln!(f, "[{}] {}", crate::now_iso(), line)?;
    Ok(())
}

/// Run pending tasks one at a time through `exec`, which returns Ok(true) for a
/// successful task. Marks each task running *before* executing it, so a crash is
/// recognisable afterwards. Stops after BREAKER_LIMIT consecutive failures,
/// leaving untried tasks pending.
pub fn drive<F>(tasks_path: &Path, journal_path: &Path, mut exec: F) -> Result<Outcome>
where
    F: FnMut(&Task) -> Result<bool>,
{
    let reaped = queue::reap_orphans(tasks_path)?;
    if reaped > 0 {
        journal(journal_path, &format!("reaped {reaped} interrupted task(s) as failed"))?;
    }

    let mut out = Outcome { ran: 0, failed: 0, tripped: false };
    let mut consecutive = 0usize;

    // Re-read pending each iteration: a task's own run may have appended more.
    loop {
        let next = match queue::pending(tasks_path)?.into_iter().next() {
            Some(t) => t,
            None => break,
        };
        queue::set_state(tasks_path, &next.id, TaskState::Running, None)?;
        journal(journal_path, &format!("start: {}", next.text))?;

        let ok = match exec(&next) {
            Ok(ok) => ok,
            Err(e) => {
                journal(journal_path, &format!("error: {} — {e}", next.text))?;
                false
            }
        };
        out.ran += 1;

        if ok {
            consecutive = 0;
            queue::set_state(tasks_path, &next.id, TaskState::Done, None)?;
            journal(journal_path, &format!("ok: {}", next.text))?;
        } else {
            consecutive += 1;
            out.failed += 1;
            queue::set_state(tasks_path, &next.id, TaskState::Failed, Some("agent run failed"))?;
            journal(journal_path, &format!("failed: {}", next.text))?;
            if consecutive >= BREAKER_LIMIT {
                out.tripped = true;
                journal(
                    journal_path,
                    &format!("circuit breaker open after {consecutive} consecutive failures"),
                )?;
                break;
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 8: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib drain 2>&1 | tail -20`
Expected: PASS, 7 tests.

- [ ] **Step 9: Wire `drain::run` to the real agent**

Append to `src/drain.rs` (above the tests):

```rust
/// Entry point for `ws -queue drain`. Holds the workspace lock for the whole
/// drain, refuses to start when another live process holds it, and refuses
/// while the circuit breaker is open.
pub fn run(name: Option<String>, reset: bool) -> Result<()> {
    let cfg = config::load();
    // Same resolution as -queue list and -who: honours $WS_WORKSPACE. Expose
    // commands::current_or_named to the crate (make it `pub(crate)`) rather than
    // hand-rolling a second lookup here.
    let (ws_name, root) = crate::commands::current_or_named(name)?;
    let ws = crate::workspace::Workspace { name: ws_name, root };

    let marker = ws.circuit_marker();
    if reset {
        if marker.exists() {
            std::fs::remove_file(&marker)
                .with_context(|| format!("cannot clear {}", marker.display()))?;
            println!("circuit breaker reset");
        }
    } else if marker.exists() {
        anyhow::bail!(
            "the circuit breaker is open for {} — inspect {} then rerun with --reset",
            ws.name,
            ws.queue_journal().display()
        );
    }

    // live_pid_checked, never live_pid: starting an unattended agent alongside a
    // live one is not a display decision. Note it takes the LOCK FILE, not the root.
    if let Some(pid) = crate::lock::live_pid_checked(&ws.lock_file())? {
        anyhow::bail!("{} is in use by pid {pid} — not starting a drain", ws.name);
    }

    // Same resolution order as commands::launch, minus the --agent override:
    // workspace default, then config default.
    let agent_id = crate::meta::read(&ws.workspace_toml())
        .default_agent
        .unwrap_or_else(|| cfg.default_agent.clone());
    let agent = agents::for_id(&agent_id)?;
    if !agent.is_installed() {
        anyhow::bail!("{agent_id} is not installed — cannot drain {}", ws.name);
    }

    // force=false: never steal a lock for an unattended run.
    let _guard = crate::lock::acquire(&ws.lock_file(), false)?;
    let actor = crate::actors::actor_slug_in(&ws.root);
    crate::timeline::record(&ws.timeline(), "drain-start", &actor, serde_json::json!({}))?;

    let mut first = true;
    let outcome = drive(&ws.queue_tasks(), &ws.queue_journal(), |task| {
        let ctx = agents::LaunchCtx {
            // The first task of a drain starts fresh; later ones chain onto it.
            fresh: first,
            sessions_root: config::sessions_root(&cfg),
        };
        first = false;
        let mut cmd = agent.headless(&ws, &task.text, &ctx)?;
        let out = cmd.output().with_context(|| format!("cannot run {agent_id}"))?;
        Ok(agent.headless_succeeded(&out))
    })?;

    crate::timeline::record(
        &ws.timeline(),
        "drain-end",
        &actor,
        serde_json::json!({ "ran": outcome.ran, "failed": outcome.failed, "tripped": outcome.tripped }),
    )?;

    if outcome.tripped {
        crate::atomic::atomic_write(&marker, crate::now_iso().as_bytes())?;
        println!(
            "drained {} task(s), {} failed — circuit breaker open; see {}",
            outcome.ran,
            outcome.failed,
            ws.queue_journal().display()
        );
        std::process::exit(1);
    }
    println!("drained {} task(s), {} failed", outcome.ran, outcome.failed);
    Ok(())
}
```

**Signatures verified live on 2026-07-26 — use them as written:** `lock::acquire(lock_file: &Path, force: bool) -> Result<LockGuard>` (RAII; the guard has a `keep()` that this code must NOT call), `lock::live_pid_checked(lock_file: &Path)` — both take the **lock file**, not the workspace root. `meta::read(&ws.workspace_toml()).default_agent` is `Option<String>`; there is no `contract::agent_of`. Confirm `atomic_write`'s import path against `src/atomic.rs` before compiling.

- [ ] **Step 10: Write an integration test for the refusals**

Add to `tests/orchestration.rs` (create it; model the harness on `tests/workspace.rs`):

```rust
#[test]
fn drain_refuses_while_the_circuit_breaker_is_open() {
    // ... create a workspace via the ws binary, write .ws/local/queue-circuit-open,
    // run `ws -queue drain <name>`, assert non-zero exit and that stderr mentions
    // "--reset". Assert the queue still has its pending task afterwards: refusing
    // must not consume work.
}

#[test]
fn drain_refuses_when_a_live_process_holds_the_lock() {
    // ... write a lock file naming this test process's own pid (std::process::id()),
    // then assert `ws -queue drain <name>` exits non-zero and names the pid.
}
```

Fill both bodies out using the existing helpers in `tests/workspace.rs` — read that file and reuse its binary-invocation helper rather than writing a new one.

- [ ] **Step 11: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test 2>&1 | grep -E "^test result|^error|warning" | tail -30`
Expected: all pass, no warnings.

- [ ] **Step 12: Grep for escalation flags across the whole crate**

This is an action, not a note. Run:

```bash
grep -rn "dangerously\|bypassPermissions\|permission-mode\|--sandbox" src/ | grep -v "^src/.*test"
```

Expected: only the *assertions* in the agent test modules that forbid them. Any occurrence in non-test code is a defect — fix it before committing.

- [ ] **Step 13: Commit**

```bash
git add src/drain.rs src/agents/mod.rs src/agents/claude.rs src/agents/codex.rs tests/orchestration.rs
git commit -m "feat(queue): headless drain with circuit breaker and crash reaping"
```

---

### Task 5: Spawn

**Files:**
- Create: `src/spawn.rs`
- Modify: `src/cli.rs`, `src/main.rs`, `src/commands.rs`

**Interfaces:**
- Consumes: `queue::add`, `workspace::resolve`, `actors::actor_slug_in`.
- Produces:
  - `spawn::TmuxPlan { session: String, window: String, dir: PathBuf, command: String, attach: Attach }`
  - `spawn::Attach { AlreadyInside, FromOutside }`
  - `spawn::plan(ws_name: &str, dir: &Path, inside_tmux: bool, drain: bool, ws_bin: &str) -> TmuxPlan`
  - `spawn::commands_for(plan: &TmuxPlan, session_exists: bool) -> Vec<Vec<String>>` — argv lists, in order.
  - `cli::Cmd::Spawn { name: String, task: Option<String> }`

**Verified against tmux 3.7b (2026-07-26):** `tmux new-session -d -s ws -n <win> -c <dir> <cmd>`, `tmux has-session -t ws` (exit 0 when present), `tmux new-window -t ws: -n <win> -c <dir> <cmd>`, `tmux attach -t ws`, `tmux select-window -t ws:<win>`.

Building the argv lists as data, separately from running them, is what makes this testable without a tmux server in the test suite.

- [ ] **Step 1: Write the failing tests**

Create `src/spawn.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        PathBuf::from("/tmp/proj")
    }

    #[test]
    fn a_fresh_session_is_created_and_attached_from_outside_tmux() {
        let plan = plan("proj", &dir(), false, false, "ws");
        let cmds = commands_for(&plan, false);
        assert_eq!(
            cmds[0],
            vec!["new-session", "-d", "-s", "ws", "-n", "proj", "-c", "/tmp/proj", "ws proj"]
        );
        assert_eq!(cmds[1], vec!["attach", "-t", "ws"], "outside tmux we attach");
    }

    #[test]
    fn an_existing_session_gets_a_new_window_instead_of_a_second_session() {
        let plan = plan("proj", &dir(), false, false, "ws");
        let cmds = commands_for(&plan, true);
        assert_eq!(
            cmds[0],
            vec!["new-window", "-t", "ws:", "-n", "proj", "-c", "/tmp/proj", "ws proj"]
        );
    }

    #[test]
    fn inside_tmux_we_select_the_window_and_never_attach() {
        let plan = plan("proj", &dir(), true, false, "ws");
        let cmds = commands_for(&plan, true);
        // Attaching from inside tmux is the classic nested-session error.
        assert!(!cmds.iter().any(|c| c[0] == "attach"), "must not attach from inside: {cmds:?}");
        assert_eq!(cmds.last().unwrap(), &vec!["select-window", "-t", "ws:proj"]);
    }

    #[test]
    fn the_task_variant_runs_a_drain_rather_than_an_interactive_launch() {
        let interactive = plan("proj", &dir(), false, false, "ws");
        assert_eq!(interactive.command, "ws proj");

        let drained = plan("proj", &dir(), false, true, "ws");
        assert_eq!(drained.command, "ws -queue drain proj");
        assert_ne!(drained.command, interactive.command);
    }

    #[test]
    fn the_ws_binary_path_is_used_verbatim_so_a_non_path_install_still_works() {
        let plan = plan("proj", &dir(), false, false, "/opt/bin/ws");
        assert_eq!(plan.command, "/opt/bin/ws proj");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib spawn 2>&1 | tail -20`
Expected: FAIL — unresolved names.

- [ ] **Step 3: Implement plan/commands_for**

Prepend to `src/spawn.rs`:

```rust
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SESSION: &str = "ws";

#[derive(Debug, PartialEq)]
pub enum Attach {
    /// Already inside tmux: switch to the window, never attach (that nests).
    AlreadyInside,
    FromOutside,
}

#[derive(Debug, PartialEq)]
pub struct TmuxPlan {
    pub session: String,
    pub window: String,
    pub dir: PathBuf,
    pub command: String,
    pub attach: Attach,
}

pub fn plan(ws_name: &str, dir: &Path, inside_tmux: bool, drain: bool, ws_bin: &str) -> TmuxPlan {
    let command = if drain {
        format!("{ws_bin} -queue drain {ws_name}")
    } else {
        format!("{ws_bin} {ws_name}")
    };
    TmuxPlan {
        session: SESSION.to_string(),
        window: ws_name.to_string(),
        dir: dir.to_path_buf(),
        command,
        attach: if inside_tmux { Attach::AlreadyInside } else { Attach::FromOutside },
    }
}

/// The tmux argv lists to run, in order.
pub fn commands_for(plan: &TmuxPlan, session_exists: bool) -> Vec<Vec<String>> {
    let dir = plan.dir.to_string_lossy().to_string();
    let mut out: Vec<Vec<String>> = Vec::new();
    if session_exists {
        out.push(vec![
            "new-window".into(), "-t".into(), format!("{}:", plan.session),
            "-n".into(), plan.window.clone(), "-c".into(), dir, plan.command.clone(),
        ]);
    } else {
        out.push(vec![
            "new-session".into(), "-d".into(), "-s".into(), plan.session.clone(),
            "-n".into(), plan.window.clone(), "-c".into(), dir, plan.command.clone(),
        ]);
    }
    match plan.attach {
        Attach::AlreadyInside => out.push(vec![
            "select-window".into(),
            "-t".into(),
            format!("{}:{}", plan.session, plan.window),
        ]),
        Attach::FromOutside => {
            out.push(vec!["attach".into(), "-t".into(), plan.session.clone()])
        }
    }
    out
}

pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn run(name: String, task: Option<String>) -> Result<()> {
    let cfg = crate::config::load();
    let ws = crate::workspace::resolve(&name, &cfg);
    if !ws.exists() {
        anyhow::bail!("no workspace named {name}");
    }
    if !tmux_available() {
        anyhow::bail!("ws -spawn needs tmux, which is not installed (brew install tmux)");
    }

    let drain = task.is_some();
    if let Some(text) = task {
        let actor = crate::actors::actor_slug_in(&ws.root);
        crate::queue::add(&ws.queue_tasks(), &text, &actor)?;
    }

    let ws_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "ws".to_string());
    let inside = std::env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false);
    let p = plan(&name, &ws.root, inside, drain, &ws_bin);

    for argv in commands_for(&p, session_exists(&p.session)) {
        let status = Command::new("tmux")
            .args(&argv)
            .status()
            .with_context(|| format!("cannot run tmux {}", argv.join(" ")))?;
        if !status.success() {
            anyhow::bail!("tmux {} failed", argv.join(" "));
        }
    }
    Ok(())
}
```

Declare `mod spawn;`.

- [ ] **Step 4: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib spawn 2>&1 | tail -20`
Expected: PASS, 5 tests.

- [ ] **Step 5: Write the failing CLI parse tests**

```rust
    #[test]
    fn parses_spawn_with_and_without_a_task() {
        assert_eq!(p(&["-spawn", "proj"]), Cmd::Spawn { name: "proj".into(), task: None });
        assert_eq!(
            p(&["-spawn", "proj", "--task", "write the docs"]),
            Cmd::Spawn { name: "proj".into(), task: Some("write the docs".into()) }
        );
    }

    #[test]
    fn spawn_requires_a_name_and_a_task_body() {
        assert!(parse(vec!["-spawn".into()]).is_err());
        assert!(parse(vec!["-spawn".into(), "proj".into(), "--task".into()]).is_err());
    }
```

- [ ] **Step 6: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 7: Add parsing and dispatch**

Add `Spawn { name: String, task: Option<String> }` to `enum Cmd` and this arm to `parse`:

```rust
        "-spawn" => {
            let mut it = it;
            let name = it
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: ws -spawn <name> [--task <text>]"))?;
            let mut task = None;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--task" => {
                        let rest: Vec<String> = it.by_ref().collect();
                        if rest.is_empty() {
                            bail!("usage: ws -spawn <name> --task <text>");
                        }
                        task = Some(rest.join(" "));
                    }
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Spawn { name, task })
        }
```

In `src/main.rs`: `Cmd::Spawn { name, task } => spawn::run(name, task)?,`

Add to `print_help`:

```
  ws -spawn <name> [--task <text>]   open a workspace in a tmux window
```

- [ ] **Step 8: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test 2>&1 | grep -E "^test result|^error|warning" | tail -30`
Expected: all pass, no warnings.

- [ ] **Step 9: Verify by hand against a real tmux server**

Run, and paste the output into the task report:

```bash
. "$HOME/.cargo/env"; cargo build 2>&1 | tail -3
tmux kill-session -t ws 2>/dev/null; true
./target/debug/ws -spawn <some-existing-workspace> &
sleep 2
tmux list-windows -t ws -F '#{window_name} #{pane_current_path}'
tmux kill-session -t ws
```

Expected: one window named after the workspace, with the workspace root as its path.

- [ ] **Step 10: Commit**

```bash
git add src/spawn.rs src/cli.rs src/main.rs
git commit -m "feat(spawn): open workspaces in tmux windows"
```

---

### Task 6: Worktrees

`ws <base>@<feature>` creates a git worktree of the base workspace's repo with its own minimal `.ws/`. `--merge` merges the branch back with `--no-ff` and removes the worktree. The union-merge attributes in `.ws/.gitattributes` (already committed since Phase 2) are what keep notebooks and the timeline from conflicting — no record fusion.

**Files:**
- Create: `src/worktree.rs`
- Modify: `src/cli.rs` (parse `base@feature`, add `--merge`), `src/main.rs`, `src/commands.rs`
- Modify: `tests/orchestration.rs`

**Interfaces:**
- Consumes: `contract::init`, `registry::{register, unregister, lookup_checked}`, `lock::live_pid_checked`, `config::sessions_root`, `atomic::atomic_write`.
- Produces:
  - `worktree::Spec { base: String, feature: String }`
  - `worktree::parse_name(s: &str) -> Option<Spec>`
  - `worktree::create(spec: &Spec) -> anyhow::Result<PathBuf>`
  - `worktree::merge(spec: &Spec) -> anyhow::Result<()>`
  - `cli::Cmd::Worktree { spec: String, merge: bool }`

- [ ] **Step 1: Write the failing tests for name parsing**

Create `src/worktree.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_at_feature() {
        assert_eq!(
            parse_name("api@retry-logic"),
            Some(Spec { base: "api".into(), feature: "retry-logic".into() })
        );
    }

    #[test]
    fn a_plain_name_is_not_a_worktree_spec() {
        assert_eq!(parse_name("api"), None);
    }

    #[test]
    fn empty_halves_are_rejected() {
        assert_eq!(parse_name("@feature"), None);
        assert_eq!(parse_name("api@"), None);
        assert_eq!(parse_name("@"), None);
    }

    #[test]
    fn only_the_first_at_splits_so_branch_names_may_contain_one() {
        assert_eq!(
            parse_name("api@fix@2"),
            Some(Spec { base: "api".into(), feature: "fix@2".into() })
        );
    }

    #[test]
    fn workspace_name_round_trips() {
        let s = parse_name("api@retry").unwrap();
        assert_eq!(s.workspace_name(), "api@retry");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib worktree 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 3: Implement parsing**

Prepend to `src/worktree.rs`:

```rust
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub base: String,
    pub feature: String,
}

impl Spec {
    pub fn workspace_name(&self) -> String {
        format!("{}@{}", self.base, self.feature)
    }
}

/// `base@feature`. Splits on the FIRST `@` so a branch name may contain one.
/// Both halves must be non-empty; anything else is not a worktree spec and the
/// caller should treat the argument as an ordinary workspace name.
pub fn parse_name(s: &str) -> Option<Spec> {
    let (base, feature) = s.split_once('@')?;
    if base.is_empty() || feature.is_empty() {
        return None;
    }
    Some(Spec { base: base.to_string(), feature: feature.to_string() })
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib worktree 2>&1 | tail -20`
Expected: PASS, 5 tests.

- [ ] **Step 5: Write the failing test for create/merge against a real repo**

Add to the same test module:

```rust
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn base_repo(td: &TempDir) -> PathBuf {
        let root = td.path().join("api");
        std::fs::create_dir_all(root.join(".ws/notebook")).unwrap();
        git(td.path(), &["init", "-q", "api"]);
        git(&root, &["config", "user.email", "dev@example.com"]);
        git(&root, &["config", "user.name", "Dev"]);
        std::fs::write(root.join(".ws/README.md"), "# api\n\nObjective: ship it\n").unwrap();
        std::fs::write(root.join(".ws/.gitattributes"),
                       "notebook/*.md merge=union\ntimeline.jsonl merge=union\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);
        root
    }

    #[test]
    fn add_worktree_creates_a_branch_a_checkout_and_a_ws_dir_naming_its_base() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");

        add_worktree(&base, &wt, "retry").unwrap();

        assert!(wt.join(".git").exists(), "worktree checkout exists");
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "retry");
        let branches = git(&base, &["branch", "--list", "retry"]);
        assert!(branches.contains("retry"), "branch created: {branches}");
    }

    #[test]
    fn merging_brings_the_branch_back_with_a_merge_commit_and_removes_the_worktree() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();

        std::fs::create_dir_all(wt.join(".ws/notebook")).unwrap();
        std::fs::write(wt.join(".ws/notebook/notebook.dev.md"), "found a thing\n").unwrap();
        git(&wt, &["config", "user.email", "dev@example.com"]);
        git(&wt, &["config", "user.name", "Dev"]);
        git(&wt, &["add", ".ws"]);
        git(&wt, &["commit", "-q", "-m", "note"]);

        merge_worktree(&base, &wt, "retry").unwrap();

        assert!(base.join(".ws/notebook/notebook.dev.md").is_file(), "work landed in base");
        let log = git(&base, &["log", "--oneline", "--merges"]);
        assert!(!log.trim().is_empty(), "--no-ff produced a merge commit: {log}");
        assert!(!wt.exists(), "worktree directory removed");
    }

    #[test]
    fn merging_refuses_while_the_worktree_has_uncommitted_changes() {
        let td = TempDir::new().unwrap();
        let base = base_repo(&td);
        let wt = td.path().join("api@retry");
        add_worktree(&base, &wt, "retry").unwrap();
        std::fs::write(wt.join("scratch.txt"), "unsaved work\n").unwrap();
        git(&wt, &["add", "scratch.txt"]);

        let err = merge_worktree(&base, &wt, "retry").unwrap_err().to_string();
        assert!(err.contains("uncommitted"), "explains why: {err}");
        // Refusing must not destroy the thing it refused to merge.
        assert!(wt.join("scratch.txt").is_file(), "the worktree survives a refusal");
    }
```

- [ ] **Step 6: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib worktree 2>&1 | tail -20`
Expected: FAIL — no `add_worktree`.

- [ ] **Step 7: Implement the git operations**

Add to `src/worktree.rs`:

```rust
fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `git worktree add -b <branch> <path>` from `base`.
pub fn add_worktree(base: &Path, path: &Path, branch: &str) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let path_s = path.to_string_lossy().to_string();
    git_ok(base, &["worktree", "add", "-b", branch, &path_s])?;
    Ok(())
}

/// Merge `branch` into whatever `base` has checked out, `--no-ff`, then remove
/// the worktree. Refuses if the worktree has uncommitted work: merging would
/// leave the change stranded in a directory this function then deletes.
pub fn merge_worktree(base: &Path, path: &Path, branch: &str) -> Result<()> {
    let dirty = git_ok(path, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        bail!(
            "{} has uncommitted changes — commit or discard them first:\n{}",
            path.display(),
            dirty.trim()
        );
    }
    git_ok(base, &["merge", "--no-ff", "-m", &format!("merge {branch}"), branch])?;
    let path_s = path.to_string_lossy().to_string();
    git_ok(base, &["worktree", "remove", &path_s])?;
    Ok(())
}
```

- [ ] **Step 8: Run to verify it passes**

Run: `. "$HOME/.cargo/env"; cargo test --lib worktree 2>&1 | tail -20`
Expected: PASS, 8 tests.

- [ ] **Step 9: Implement `create` and `merge` at the workspace level**

Add to `src/worktree.rs`:

```rust
/// Create `<base>@<feature>`: a git worktree of the base workspace's repo, with
/// its own `.ws/` naming the base, registered under the combined name.
pub fn create(spec: &Spec) -> Result<PathBuf> {
    let cfg = crate::config::load();
    let base_path = crate::registry::lookup_checked(&spec.base)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {}", spec.base))?;
    if !base_path.join(".git").exists() {
        bail!("{} is not a git repository — worktrees need one", base_path.display());
    }
    let name = spec.workspace_name();
    if crate::registry::lookup_checked(&name)?.is_some() {
        bail!("{name} already exists");
    }

    let path = crate::config::sessions_root(&cfg).join(&name);
    add_worktree(&base_path, &path, &spec.feature)?;

    // Minimal .ws/ bootstrap. commit=false: the contract files land in the
    // worktree's working copy and the user commits them with their own work.
    let agent = cfg.default_agent.clone();
    crate::contract::init(&name, &path, &agent, false)?;
    crate::atomic::atomic_write(
        &path.join(".ws/base"),
        format!("{}\n", spec.base).as_bytes(),
    )?;
    crate::registry::register(&name, &path)?;
    let actor = crate::actors::actor_slug_in(&path);
    crate::timeline::record(
        &path.join(".ws/timeline.jsonl"),
        "worktree-created",
        &actor,
        serde_json::json!({ "base": spec.base, "branch": spec.feature }),
    )?;
    Ok(path)
}

/// Merge the worktree back into its base and remove it.
pub fn merge(spec: &Spec) -> Result<()> {
    let name = spec.workspace_name();
    let path = crate::registry::lookup_checked(&name)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {name}"))?;
    let base_path = crate::registry::lookup_checked(&spec.base)?
        .ok_or_else(|| anyhow::anyhow!("no workspace named {}", spec.base))?;

    // live_pid_checked: this deletes a directory. An unreadable lock must stop
    // us, not read as "nobody home". Takes the lock FILE, not the root.
    if let Some(pid) = crate::lock::live_pid_checked(&path.join(".ws/local/lock"))? {
        bail!("{name} is in use by pid {pid} — close it before merging");
    }

    merge_worktree(&base_path, &path, &spec.feature)?;
    crate::registry::unregister(&name)?;
    println!("merged {} into {} and removed the worktree", spec.feature, spec.base);
    Ok(())
}
```

**Signatures verified live on 2026-07-26:** `contract::init(name: &str, root: &Path, agent: &str, commit: bool) -> Result<()>`, `config::Config::default_agent` is a plain `String` (not an Option — the Option lives on `meta::read(..).default_agent`), `registry::register(name: &str, path: &Path) -> Result<()>`. Confirm `registry::lookup_checked`'s return type against `src/registry.rs` before compiling.

- [ ] **Step 10: Write the failing CLI parse tests**

```rust
    #[test]
    fn a_name_with_an_at_parses_as_a_worktree_not_a_launch() {
        assert_eq!(p(&["api@retry"]), Cmd::Worktree { spec: "api@retry".into(), merge: false });
        assert_eq!(p(&["api@retry", "--merge"]), Cmd::Worktree { spec: "api@retry".into(), merge: true });
    }

    #[test]
    fn a_plain_name_still_launches() {
        assert_eq!(
            p(&["api"]),
            Cmd::Launch { name: "api".into(), agent: None, fresh: false, force: false, handoff: false }
        );
    }

    #[test]
    fn a_malformed_worktree_spec_is_treated_as_an_ordinary_name() {
        // "api@" is not a worktree spec; it must not silently become one.
        match p(&["api@"]) {
            Cmd::Launch { name, .. } => assert_eq!(name, "api@"),
            other => panic!("expected a launch, got {other:?}"),
        }
    }
```

- [ ] **Step 11: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --lib cli 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 12: Add parsing and dispatch**

Add `Worktree { spec: String, merge: bool }` to `enum Cmd`. In `parse`'s final `name => { ... }` arm, before the existing launch-flag loop:

```rust
        name if crate::worktree::parse_name(name).is_some() => {
            let mut merge = false;
            for a in it {
                match a.as_str() {
                    "--merge" => merge = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Worktree { spec: name.to_string(), merge })
        }
```

In `src/main.rs`:

```rust
        Cmd::Worktree { spec, merge } => {
            let s = worktree::parse_name(&spec)
                .ok_or_else(|| anyhow::anyhow!("not a worktree spec: {spec}"))?;
            if merge {
                worktree::merge(&s)?
            } else {
                let path = worktree::create(&s)?;
                println!("created {} at {}", s.workspace_name(), path.display());
            }
        }
```

Declare `mod worktree;`. Add to `print_help`:

```
  ws <base>@<feature>     create a git worktree workspace off <base>
  ws <base>@<feature> --merge   merge it back (--no-ff) and remove it
```

- [ ] **Step 13: Run the full suite and check warnings**

Run: `. "$HOME/.cargo/env"; cargo test 2>&1 | grep -E "^test result|^error|warning" | tail -30`
Expected: all pass, no warnings.

- [ ] **Step 14: Verify the union merge actually works end to end**

Union merge is the whole reason this design skips record fusion, and no unit test above proves it. Run this by hand and paste the output into the task report:

```bash
cd "$(mktemp -d)" && git init -q base && cd base
git config user.email d@e.com; git config user.name D
mkdir -p .ws/notebook
printf 'notebook/*.md merge=union\n' > .ws/.gitattributes
printf 'shared line\n' > .ws/notebook/notebook.dev.md
git add .ws && git commit -q -m init
git worktree add -q -b feat ../feat
printf 'shared line\nfrom the feature branch\n' > ../feat/.ws/notebook/notebook.dev.md
git -C ../feat add .ws && git -C ../feat commit -q -m feat
printf 'shared line\nfrom the base branch\n' > .ws/notebook/notebook.dev.md
git add .ws && git commit -q -m base
git merge --no-ff -m merge feat && cat .ws/notebook/notebook.dev.md
```

Expected: the merge succeeds with no conflict and the file contains **both** added lines. If it conflicts, the `.gitattributes` path or the `merge=union` driver is not in effect and that must be fixed before this task is done.

- [ ] **Step 15: Commit**

```bash
git add src/worktree.rs src/cli.rs src/commands.rs src/main.rs tests/orchestration.rs
git commit -m "feat(worktree): ws base@feature with --no-ff merge back"
```

---

## Self-Review

**1. Spec coverage (§12).**

| Spec item | Task |
|---|---|
| 33 Worktrees: `git worktree add` + minimal `.ws/`, `--merge` with union merge | Task 6 |
| 34 Queue: `-queue add` → tasks.jsonl, headless drain, breaker at 2, journal, explicit start only | Tasks 3 + 4 |
| 35 Mail: JSON in `.ws/mail/`, surfaced at session start, `-msg log` | Task 2 |
| 36 Spawn: tmux window, `--task` seeds and drains, plain error without tmux | Task 5 |
| 37 Actors: slug from git email, per-actor notebooks, `-whoami`, `-who` | Task 1 (per-actor notebooks already exist — `src/contract.rs:42`) |

**2. Cross-task contradiction check** (the last plan shipped two tasks that undid each other):

- `src/tui/detail.rs` and `render.rs:211` are touched by Task 2 (mail) and Task 3 (queue). Each task's step names the other and confines itself to one field. Task 3 executes after Task 2, so its render snippet includes Task 2's mail half already applied — deliberate, not a duplicate edit.
- `src/drain.rs` is created as a stub in Task 3 and replaced in Task 4. Task 3's stub is explicitly labelled; Task 4's first step replaces it.
- Task 4's drain acquires the workspace lock; Task 5's `--task` spawn runs `ws -queue drain` as a subprocess and therefore does **not** take the lock itself. No double-acquire.
- Task 6's `contract::init` writes a per-actor notebook using the slug from Task 1. Task 1 runs first.
- `-queue drain` parsing lands in Task 3 but is implemented in Task 4 — the only forward reference, and it is stubbed so every task compiles on its own.

**3. Type consistency.** `TaskState` is spelled the same in `queue.rs`, `drain.rs`, and the detail test. `Detail.queue` and `Detail.mail` are both `Option<usize>` after Tasks 2–3, and `render.rs:211` unwraps both. `LaunchCtx { fresh, sessions_root }` matches the existing struct. `Spec.workspace_name()` is used in Task 6's `create`, `merge`, and `main.rs`.

**4. Known gap, deliberately out of scope.** `lock::acquire` still uses `exists()`-then-`write` rather than `O_EXCL` (a deferred item from Phase 7.5). Task 4 narrows the window by checking `live_pid_checked` immediately before acquiring, but two drains started in the same millisecond could still both proceed. Fixing the lock primitive properly is its own change with its own blast radius; it is called out here so the final reviewer sees it was a decision rather than an oversight.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-26-ws-phase8-orchestration.md`.

Execute subagent-driven on a `phase8-orchestration` branch: fresh implementer per task, reviewer per task, **opus** whole-branch review at the end, merge `--no-ff`, no version bump.
