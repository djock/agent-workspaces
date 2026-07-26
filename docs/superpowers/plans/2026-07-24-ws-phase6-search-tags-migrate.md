# ws Phase 6 (Search, Tags, Archive, Status, Migration) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make a growing collection of workspaces navigable — full-text search across every workspace's `.ws/` documents, tags/status/archive metadata to organize them, and a `migrate-cs` importer that brings existing `~/.claude-sessions` sessions over without touching cs.

**Architecture:** One new typed accessor module, `src/meta.rs`, owns `.ws/workspace.toml` — it reads into a `Meta` struct and writes through a `toml::Table` round-trip so unknown keys are preserved and a corrupt file is never clobbered (the rule established in the hardening pass). Every Phase 6 command reads its filters through `meta`. `src/search.rs` walks registered workspaces with ripgrep's own libraries (`ignore::WalkBuilder` + `grep::searcher`), searching only the committed documents under `.ws/` and deliberately never `.ws/local/` (session logs, limits, state) or `*.enc` (secrets). `src/migrate.rs` reads `~/.claude-sessions/<name>` and produces a ws workspace: it copies (never moves), maps cs's file layout onto ws's contract, and — critically — handles the fact that **8 of 20 real cs sessions are symlinks to live project directories**, which must be adopted in place rather than copied.

**Tech Stack:** Rust 2021. New deps (verified to resolve *and* compile on 2026-07-24): `grep = "0.4.1"`, `ignore = "0.4.31"`, `regex = "1.13.1"`. Existing: serde/serde_json, toml, anyhow, dirs. Dev: assert_cmd, predicates, tempfile.

## Global Constraints

- **Rust 2021, single crate, binary `ws`.** cargo is NOT on PATH — prefix every cargo call with `. "$HOME/.cargo/env";` (Rust 1.97.1).
- **Full suite is the source of truth:** `. "$HOME/.cargo/env"; cargo test` must be all-green before every commit. `.cargo/config.toml` pins `RUST_TEST_THREADS=1` (unit tests mutate global env) — do not change that.
- **All shared-file writes are atomic and clobber-safe.** Write to `<path>.tmp` then `std::fs::rename`. If an existing `workspace.toml` / `registry.toml` / `config.toml` fails to parse, **bail with an error — never overwrite it**. This is a standing invariant from the Phase 1–3 hardening pass; Task 1 has a test for it.
- **`grep::regex::escape` does NOT exist** (verified — this is the one API surprise in the ripgrep crates). Use `regex::escape` from the `regex` crate for literal queries. Verified working API surface: `grep::regex::RegexMatcher::new_line_matcher`, `grep::searcher::{SearcherBuilder, BinaryDetection, sinks::UTF8}`, `ignore::WalkBuilder::new(root).hidden(false)`.
- **Search must never read `.ws/local/`** — it holds `session.log` (bash audit), `limits.json`, `state.toml`, and the lock. Nor any `*.enc` (encrypted secrets file). This is a security boundary, not an optimization; Task 3 has an explicit test for it.
- **`migrate-cs` copies, never moves, and never writes into `~/.claude-sessions`.** cs must remain fully usable afterward. It also never copies `.cs/local/`.
- **cs sessions may be symlinks.** Verified on the real machine: 8 of 20 entries in `~/.claude-sessions` are symlinks into live project dirs (e.g. `keystone -> ~/Projects/Native/keystone`). For those, migrate **in place** (create `.ws/` next to the existing `.cs/`, register name → the resolved target path). Only a real directory gets copied into `<sessions_root>/<name>`. Never copy a project tree you did not create.
- **Single-dash pseudo-subcommands** (`-tag`, `-status`, `-archive`, `-unarchive`, `-search`) match the existing CLI surface in `src/cli.rs`; `migrate-cs` is a bare-word subcommand like `config`/`setup` (spec §14 writes it without a dash).
- **Env test seams:** `WS_ROOT` (sessions root) and `XDG_CONFIG_HOME` (registry/config) already exist. Task 4 adds `WS_CS_ROOT` to override `~/.claude-sessions` so migration is testable without touching the user's real sessions.
- **Archived workspaces are hidden by default** everywhere: `-list` excludes them unless `--archived`; `-search` skips them unless `--include-archived`.

---

## File Structure

```
Cargo.toml            # + grep, ignore, regex
src/meta.rs           # NEW — typed, clobber-safe .ws/workspace.toml accessor (Meta, read, update, mutators)
src/search.rs         # NEW — search_workspaces(): ignore::Walk + grep::searcher over .ws/ docs
src/migrate.rs        # NEW — cs_root(), discover(), migrate_one(): cs → ws mapping
src/cli.rs            # + Cmd::Tag/Status/Archive/Search/MigrateCs, List gains filters
src/commands.rs       # + tag/status/archive/search/migrate_cs handlers; list() gains filters;
                      #   workspace_default_agent/set_workspace_default_agent/workspace_color → meta
src/main.rs           # + mod meta/search/migrate; route the new Cmds; help text
tests/meta.rs         # NEW — -tag / -status / -archive / -unarchive / -list filters (integration)
tests/search.rs       # NEW — -search hits, archived exclusion, local/ exclusion
tests/migrate.rs      # NEW — migrate-cs real dir, symlinked dir, --all, collision, dry-run
```

---

### Task 1: `src/meta.rs` — the workspace.toml accessor

Everything else in this phase reads or writes `.ws/workspace.toml`. Today three ad-hoc helpers in `commands.rs` (`workspace_toml_str`, `workspace_default_agent`, `set_workspace_default_agent`, `workspace_color`) do it by hand. This task centralizes that into one typed, tested module and moves the existing callers onto it — so Tasks 2–4 have a single place to add `tags`, `status`, and `archived`.

**Files:**
- Create: `src/meta.rs`
- Modify: `src/main.rs` (add `mod meta;`)
- Modify: `src/commands.rs:365-396` (replace the four private helpers with `meta::` calls)
- Test: unit tests inside `src/meta.rs`

**Interfaces:**
- Consumes: `crate::workspace::Workspace` (has `.workspace_toml() -> PathBuf`, `.root`, `.name`), `crate::registry::all() -> Vec<(String, PathBuf)>`.
- Produces (Tasks 2, 3 and 4 depend on these exact signatures):
  ```rust
  pub struct Meta {
      pub name: String,
      pub created: String,
      pub contract_version: u32,
      pub default_agent: Option<String>,
      pub archived: bool,
      pub tags: Vec<String>,
      pub status: Option<String>,
      pub color: Option<String>,
  }
  pub fn read(ws_toml: &std::path::Path) -> Meta;                       // missing/corrupt → Meta::default()
  pub fn update(ws_toml: &Path, f: impl FnOnce(&mut toml::Table)) -> Result<()>;  // atomic; bails on corrupt
  pub fn add_tags(ws_toml: &Path, tags: &[String]) -> Result<Vec<String>>;   // returns the new full tag list
  pub fn remove_tags(ws_toml: &Path, tags: &[String]) -> Result<Vec<String>>;
  pub fn set_status(ws_toml: &Path, text: Option<&str>) -> Result<()>;  // None clears
  pub fn set_archived(ws_toml: &Path, archived: bool) -> Result<()>;
  pub fn set_default_agent(ws_toml: &Path, agent: &str) -> Result<()>;
  ```

- [ ] **Step 1: Write the failing tests**

Create `src/meta.rs` with only the test module plus `use` lines (the impl comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn wt(contents: &str) -> (TempDir, std::path::PathBuf) {
        let d = TempDir::new().unwrap();
        let p = d.path().join("workspace.toml");
        std::fs::write(&p, contents).unwrap();
        (d, p)
    }

    #[test]
    fn read_full_and_missing() {
        let (_d, p) = wt(
            "name = \"proj\"\ncreated = \"2026-07-24T10:00:00Z\"\ncontract_version = 1\n\
             default_agent = \"codex\"\narchived = true\ntags = [\"rust\", \"cli\"]\n\
             status = \"waiting on review\"\ncolor = \"blue\"\n",
        );
        let m = read(&p);
        assert_eq!(m.name, "proj");
        assert_eq!(m.contract_version, 1);
        assert_eq!(m.default_agent.as_deref(), Some("codex"));
        assert!(m.archived);
        assert_eq!(m.tags, vec!["rust".to_string(), "cli".to_string()]);
        assert_eq!(m.status.as_deref(), Some("waiting on review"));
        assert_eq!(m.color.as_deref(), Some("blue"));

        // A missing file reads as defaults, not an error — callers list many
        // workspaces and must tolerate a half-built one.
        let missing = read(std::path::Path::new("/nope/workspace.toml"));
        assert_eq!(missing.name, "");
        assert!(!missing.archived);
        assert!(missing.tags.is_empty());
    }

    #[test]
    fn update_preserves_unknown_keys() {
        let (_d, p) = wt("name = \"proj\"\nfuture_key = \"keep me\"\ntags = [\"a\"]\n");
        set_status(&p, Some("shipping")).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("future_key"), "unknown keys must survive a write: {s}");
        assert!(s.contains("shipping"));
        assert_eq!(read(&p).tags, vec!["a".to_string()]);
    }

    #[test]
    fn update_refuses_to_clobber_corrupt_file() {
        let (_d, p) = wt("this is not toml {{{");
        assert!(set_archived(&p, true).is_err());
        // The corrupt file is still there, byte for byte.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "this is not toml {{{");
    }

    #[test]
    fn tags_add_dedupes_and_sorts_remove_is_idempotent() {
        let (_d, p) = wt("name = \"proj\"\n");
        let after = add_tags(&p, &["rust".into(), "cli".into(), "rust".into()]).unwrap();
        assert_eq!(after, vec!["cli".to_string(), "rust".to_string()]);
        // adding an existing tag is a no-op, not a duplicate
        let after = add_tags(&p, &["cli".into()]).unwrap();
        assert_eq!(after, vec!["cli".to_string(), "rust".to_string()]);
        let after = remove_tags(&p, &["cli".into(), "never-there".into()]).unwrap();
        assert_eq!(after, vec!["rust".to_string()]);
    }

    #[test]
    fn status_clear_removes_the_key_entirely() {
        let (_d, p) = wt("name = \"proj\"\nstatus = \"busy\"\n");
        set_status(&p, None).unwrap();
        assert_eq!(read(&p).status, None);
        assert!(!std::fs::read_to_string(&p).unwrap().contains("status"));
    }

    #[test]
    fn archived_and_default_agent_roundtrip() {
        let (_d, p) = wt("name = \"proj\"\narchived = false\n");
        set_archived(&p, true).unwrap();
        assert!(read(&p).archived);
        set_archived(&p, false).unwrap();
        assert!(!read(&p).archived);
        set_default_agent(&p, "codex").unwrap();
        assert_eq!(read(&p).default_agent.as_deref(), Some("codex"));
    }

    #[test]
    fn update_creates_the_file_when_absent() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("workspace.toml");
        set_status(&p, Some("new")).unwrap();
        assert_eq!(read(&p).status.as_deref(), Some("new"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `. "$HOME/.cargo/env"; cargo test meta::`
Expected: FAIL — compile errors, `cannot find function 'read' in this scope` (and the same for the mutators).

- [ ] **Step 3: Implement `src/meta.rs`**

Put this **above** the test module in `src/meta.rs`:

```rust
//! Typed, clobber-safe access to `.ws/workspace.toml`.
//!
//! Reads produce a `Meta`; writes go through a `toml::Table` round-trip so keys
//! this version of `ws` doesn't know about survive, and a file that fails to
//! parse is never overwritten.
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Meta {
    pub name: String,
    pub created: String,
    pub contract_version: u32,
    pub default_agent: Option<String>,
    pub archived: bool,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub color: Option<String>,
}

fn table(ws_toml: &Path) -> Option<toml::Table> {
    toml::from_str(&std::fs::read_to_string(ws_toml).ok()?).ok()
}

/// Read workspace metadata. A missing or unparseable file reads as defaults —
/// listing commands walk many workspaces and must tolerate a half-built one.
pub fn read(ws_toml: &Path) -> Meta {
    let t = match table(ws_toml) {
        Some(t) => t,
        None => return Meta::default(),
    };
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(String::from);
    Meta {
        name: s("name").unwrap_or_default(),
        created: s("created").unwrap_or_default(),
        contract_version: t
            .get("contract_version")
            .and_then(|v| v.as_integer())
            .unwrap_or(0) as u32,
        default_agent: s("default_agent"),
        archived: t.get("archived").and_then(|v| v.as_bool()).unwrap_or(false),
        tags: t
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        status: s("status"),
        color: s("color"),
    }
}

/// Apply `f` to the parsed table and write it back atomically.
/// Bails if the file exists but cannot be parsed — we never clobber a file we
/// don't understand.
pub fn update(ws_toml: &Path, f: impl FnOnce(&mut toml::Table)) -> Result<()> {
    let mut t = match std::fs::read_to_string(ws_toml) {
        Ok(s) => toml::from_str(&s).with_context(|| {
            format!(
                "{} is corrupt (refusing to overwrite)",
                ws_toml.display()
            )
        })?,
        Err(_) => toml::Table::new(),
    };
    f(&mut t);
    if let Some(dir) = ws_toml.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = ws_toml.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(&t)?)?;
    std::fs::rename(&tmp, ws_toml)?;
    Ok(())
}

fn write_tags(t: &mut toml::Table, tags: &[String]) {
    t.insert(
        "tags".into(),
        toml::Value::Array(tags.iter().map(|s| toml::Value::String(s.clone())).collect()),
    );
}

/// Add tags (deduped, sorted). Returns the resulting full tag list.
pub fn add_tags(ws_toml: &Path, tags: &[String]) -> Result<Vec<String>> {
    let mut all = read(ws_toml).tags;
    for tag in tags {
        if !all.iter().any(|t| t == tag) {
            all.push(tag.clone());
        }
    }
    all.sort();
    all.dedup();
    let out = all.clone();
    update(ws_toml, |t| write_tags(t, &all))?;
    Ok(out)
}

/// Remove tags. Removing a tag that isn't there is not an error.
pub fn remove_tags(ws_toml: &Path, tags: &[String]) -> Result<Vec<String>> {
    let mut all = read(ws_toml).tags;
    all.retain(|t| !tags.iter().any(|r| r == t));
    let out = all.clone();
    update(ws_toml, |t| write_tags(t, &all))?;
    Ok(out)
}

/// Set the one-line status text; `None` clears it (removes the key).
pub fn set_status(ws_toml: &Path, text: Option<&str>) -> Result<()> {
    let text = text.map(str::to_string);
    update(ws_toml, |t| match text {
        Some(s) => {
            t.insert("status".into(), toml::Value::String(s));
        }
        None => {
            t.remove("status");
        }
    })
}

pub fn set_archived(ws_toml: &Path, archived: bool) -> Result<()> {
    update(ws_toml, |t| {
        t.insert("archived".into(), toml::Value::Boolean(archived));
    })
}

pub fn set_default_agent(ws_toml: &Path, agent: &str) -> Result<()> {
    let agent = agent.to_string();
    update(ws_toml, |t| {
        t.insert("default_agent".into(), toml::Value::String(agent));
    })
}
```

Add `mod meta;` to `src/main.rs` (keep the module list alphabetical — between `mod lock;` and `mod prompts;`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `. "$HOME/.cargo/env"; cargo test meta::`
Expected: PASS — 7 tests in `meta::tests`.

- [ ] **Step 5: Move `commands.rs` onto `meta`**

Delete `workspace_toml_str`, `workspace_default_agent`, `set_workspace_default_agent` and `workspace_color` from the bottom of `src/commands.rs` (currently lines 365-396) and replace their call sites:

```rust
// in launch(), replacing `let recorded_default = workspace_default_agent(&name, &cfg);`
let recorded_default = crate::meta::read(&workspace::resolve(&name, &cfg).workspace_toml()).default_agent;
```

```rust
// in launch(), inside `if switching { ... }`, replacing set_workspace_default_agent(&ws, agent.id())?
crate::meta::set_default_agent(&ws.workspace_toml(), agent.id())?;
```

```rust
// in launch(), replacing `let color = workspace_color(&ws);`
let color = crate::meta::read(&ws.workspace_toml()).color;
```

- [ ] **Step 6: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS — all 122 existing tests plus the 7 new ones (129), zero failures. The agent-switch tests in `tests/launch.rs` are the ones that prove the refactor kept behavior; they must still pass.

- [ ] **Step 7: Commit**

```bash
git add src/meta.rs src/main.rs src/commands.rs
git commit -m "feat: meta.rs — typed, clobber-safe workspace.toml accessor"
```

---

### Task 2: `-tag`, `-status`, `-archive`/`-unarchive`, and `-list` filters

**Files:**
- Modify: `src/cli.rs` (new `Cmd` variants + parsers)
- Modify: `src/commands.rs` (`tag`, `status`, `archive` handlers; `list` gains filters)
- Modify: `src/main.rs` (routing + help)
- Test: `tests/meta.rs` (new integration test file)

**Interfaces:**
- Consumes: `meta::{read, add_tags, remove_tags, set_status, set_archived}` from Task 1; `registry::{all, lookup}`; `workspace::resolve`.
- Produces:
  ```rust
  // cli.rs
  pub enum Cmd {
      // …existing…
      List { tag: Option<String>, archived: bool },   // NOTE: List gains fields
      Tag(TagCmd),
      Status { name: Option<String>, text: Option<String> },  // text None = --clear
      Archive { names: Vec<String>, archived: bool },          // archived=false for -unarchive
  }
  pub enum TagCmd {
      Add { name: Option<String>, tags: Vec<String> },
      Rm  { name: Option<String>, tags: Vec<String> },
      List { name: Option<String> },
  }
  // commands.rs
  pub fn tag(cmd: TagCmd) -> Result<()>;
  pub fn status(name: Option<String>, text: Option<String>) -> Result<()>;
  pub fn archive(names: Vec<String>, archived: bool) -> Result<()>;
  pub fn list(tag: Option<String>, archived: bool) -> Result<()>;
  // shared resolver used by tag/status (and reused by nothing else):
  fn current_or_named(name: Option<String>) -> Result<(String, std::path::PathBuf)>;
  ```
  `current_or_named` resolves in this order: an explicit `name` via the registry → `$WS_WORKSPACE` → the current directory if it contains `.ws/`. Otherwise it errors with `not in a workspace (name one, or run inside one)`. This mirrors `secrets::workspace_name()`'s contract so the two feel the same.

- [ ] **Step 1: Write the failing CLI-parse tests**

Append to the `mod tests` block at the bottom of `src/cli.rs`:

```rust
    #[test]
    fn list_filters() {
        assert_eq!(p(&["-list"]), Cmd::List { tag: None, archived: false });
        assert_eq!(p(&["-ls", "--archived"]), Cmd::List { tag: None, archived: true });
        assert_eq!(
            p(&["-list", "--tag", "rust"]),
            Cmd::List { tag: Some("rust".into()), archived: false }
        );
        assert_eq!(p(&[]), Cmd::List { tag: None, archived: false });
    }

    #[test]
    fn tag_subcommands() {
        assert_eq!(
            p(&["-tag", "add", "rust", "cli"]),
            Cmd::Tag(TagCmd::Add { name: None, tags: vec!["rust".into(), "cli".into()] })
        );
        assert_eq!(
            p(&["-tag", "rm", "--workspace", "proj", "rust"]),
            Cmd::Tag(TagCmd::Rm { name: Some("proj".into()), tags: vec!["rust".into()] })
        );
        assert_eq!(p(&["-tag", "list"]), Cmd::Tag(TagCmd::List { name: None }));
        assert_eq!(
            p(&["-tag", "list", "--workspace", "proj"]),
            Cmd::Tag(TagCmd::List { name: Some("proj".into()) })
        );
        // add/rm need at least one tag
        assert!(parse(vec!["-tag".into(), "add".into()]).is_err());
        assert!(parse(vec!["-tag".into(), "bogus".into()]).is_err());
    }

    #[test]
    fn status_set_and_clear() {
        assert_eq!(
            p(&["-status", "waiting on review"]),
            Cmd::Status { name: None, text: Some("waiting on review".into()) }
        );
        assert_eq!(p(&["-status", "--clear"]), Cmd::Status { name: None, text: None });
        assert_eq!(
            p(&["-status", "--workspace", "proj", "busy"]),
            Cmd::Status { name: Some("proj".into()), text: Some("busy".into()) }
        );
        // `-status` with no argument is ambiguous — require --clear to clear.
        assert!(parse(vec!["-status".into()]).is_err());
    }

    #[test]
    fn archive_and_unarchive() {
        assert_eq!(
            p(&["-archive", "a", "b"]),
            Cmd::Archive { names: vec!["a".into(), "b".into()], archived: true }
        );
        assert_eq!(
            p(&["-unarchive", "a"]),
            Cmd::Archive { names: vec!["a".into()], archived: false }
        );
        assert!(parse(vec!["-archive".into()]).is_err());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `. "$HOME/.cargo/env"; cargo test cli::`
Expected: FAIL — `variant 'Tag' not found`, `struct variant Cmd::List has no field named 'tag'`.

- [ ] **Step 3: Implement the parsers**

In `src/cli.rs`, change the `List` variant and add the new ones:

```rust
#[derive(Debug, PartialEq)]
pub enum Cmd {
    Launch { name: String, agent: Option<String>, fresh: bool, force: bool, handoff: bool },
    List { tag: Option<String>, archived: bool },
    Adopt { name: Option<String> },
    Rm { names: Vec<String>, force: bool },
    Config(ConfigCmd),
    Version,
    Help,
    Setup,
    Internal(Vec<String>),
    Statusline,
    SubagentStatusline,
    Limits,
    Doctor,
    Secrets(SecretsCmd),
    Tag(TagCmd),
    Status { name: Option<String>, text: Option<String> },
    Archive { names: Vec<String>, archived: bool },
}

#[derive(Debug, PartialEq)]
pub enum TagCmd {
    Add { name: Option<String>, tags: Vec<String> },
    Rm { name: Option<String>, tags: Vec<String> },
    List { name: Option<String> },
}
```

Update the two existing `Ok(Cmd::List)` sites and add the new arms in `parse`:

```rust
        None => return Ok(Cmd::List { tag: None, archived: false }), // no args → friendly list
```
```rust
        "-list" | "-ls" => parse_list(it.collect()),
        "-tag" => parse_tag(it.collect()),
        "-status" => parse_status(it.collect()),
        "-archive" => parse_archive(it.collect(), true),
        "-unarchive" => parse_archive(it.collect(), false),
```

Add the four parser functions next to `parse_secrets`:

```rust
fn parse_list(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let mut tag = None;
    let mut archived = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--archived" => archived = true,
            "--tag" => tag = Some(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -list [--tag <tag>] [--archived]"))?),
            other => bail!("unexpected argument: {other}"),
        }
    }
    Ok(Cmd::List { tag, archived })
}

/// Pull an optional `--workspace <name>` out of `args`, returning it plus the rest.
fn take_workspace(args: Vec<String>) -> Result<(Option<String>, Vec<String>)> {
    let mut it = args.into_iter();
    let mut name = None;
    let mut rest = Vec::new();
    while let Some(a) = it.next() {
        if a == "--workspace" {
            name = Some(it.next().ok_or_else(|| anyhow::anyhow!("--workspace needs a name"))?);
        } else {
            rest.push(a);
        }
    }
    Ok((name, rest))
}

fn parse_tag(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it.next().unwrap_or_default();
    let (name, tags) = take_workspace(it.collect())?;
    let cmd = match sub.as_str() {
        "add" | "rm" => {
            if tags.is_empty() {
                bail!("usage: ws -tag {sub} [--workspace <name>] <tag>...");
            }
            if sub == "add" { TagCmd::Add { name, tags } } else { TagCmd::Rm { name, tags } }
        }
        "list" => {
            if !tags.is_empty() {
                bail!("usage: ws -tag list [--workspace <name>]");
            }
            TagCmd::List { name }
        }
        other => bail!("unknown -tag subcommand: {other} (want add|rm|list)"),
    };
    Ok(Cmd::Tag(cmd))
}

fn parse_status(args: Vec<String>) -> Result<Cmd> {
    let mut clear = false;
    let args: Vec<String> = args
        .into_iter()
        .filter(|a| {
            if a == "--clear" { clear = true; false } else { true }
        })
        .collect();
    let (name, rest) = take_workspace(args)?;
    if clear {
        if !rest.is_empty() {
            bail!("ws -status: --clear takes no text");
        }
        return Ok(Cmd::Status { name, text: None });
    }
    match rest.len() {
        1 => Ok(Cmd::Status { name, text: Some(rest[0].clone()) }),
        _ => bail!("usage: ws -status [--workspace <name>] \"<text>\" | --clear"),
    }
}

fn parse_archive(args: Vec<String>, archived: bool) -> Result<Cmd> {
    if args.is_empty() {
        bail!("usage: ws {} <name>...", if archived { "-archive" } else { "-unarchive" });
    }
    Ok(Cmd::Archive { names: args, archived })
}
```

- [ ] **Step 4: Run the parse tests**

Run: `. "$HOME/.cargo/env"; cargo test cli::`
Expected: PASS. (`cargo test` overall will still fail to compile until Step 5 — `main.rs` doesn't handle the new variants yet. That's expected here.)

- [ ] **Step 5: Write the failing integration test**

Create `tests/meta.rs`:

```rust
mod common;
use common::Env;
use predicates::prelude::*;

/// Create a workspace directory the way `ws -adopt` would, without launching an
/// agent: make the dir, run `-adopt` inside it.
fn make_ws(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    dir
}

#[test]
fn tag_add_list_rm() {
    let env = Env::new();
    let dir = make_ws(&env, "proj");
    env.cmd().current_dir(&dir).args(["-tag", "add", "rust", "cli"]).assert().success();
    env.cmd().current_dir(&dir).args(["-tag", "list"]).assert().success()
        .stdout(predicate::str::contains("cli"))
        .stdout(predicate::str::contains("rust"));
    env.cmd().current_dir(&dir).args(["-tag", "rm", "cli"]).assert().success();
    env.cmd().current_dir(&dir).args(["-tag", "list"]).assert().success()
        .stdout(predicate::str::contains("rust"))
        .stdout(predicate::str::contains("cli").not());
}

#[test]
fn tag_by_name_from_anywhere() {
    let env = Env::new();
    make_ws(&env, "proj");
    // No cwd inside the workspace — address it by name instead.
    env.cmd().current_dir(env.home.path())
        .args(["-tag", "add", "--workspace", "proj", "rust"]).assert().success();
    env.cmd().current_dir(env.home.path())
        .args(["-tag", "list", "--workspace", "proj"]).assert().success()
        .stdout(predicate::str::contains("rust"));
}

#[test]
fn status_set_shows_in_list_then_clears() {
    let env = Env::new();
    let dir = make_ws(&env, "proj");
    env.cmd().current_dir(&dir).args(["-status", "waiting on review"]).assert().success();
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("waiting on review"));
    env.cmd().current_dir(&dir).args(["-status", "--clear"]).assert().success();
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("waiting on review").not());
}

#[test]
fn archive_hides_from_list_until_flagged() {
    let env = Env::new();
    make_ws(&env, "keep");
    make_ws(&env, "old");
    env.cmd().args(["-archive", "old"]).assert().success();

    // default listing hides it
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("keep"))
        .stdout(predicate::str::contains("old").not());
    // --archived shows it, marked
    env.cmd().args(["-list", "--archived"]).assert().success()
        .stdout(predicate::str::contains("old"))
        .stdout(predicate::str::contains("archived"));
    // unarchive brings it back
    env.cmd().args(["-unarchive", "old"]).assert().success();
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("old"));
}

#[test]
fn list_filters_by_tag() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    make_ws(&env, "beta");
    env.cmd().current_dir(&a).args(["-tag", "add", "rust"]).assert().success();
    env.cmd().args(["-list", "--tag", "rust"]).assert().success()
        .stdout(predicate::str::contains("alpha"))
        .stdout(predicate::str::contains("beta").not());
}

#[test]
fn archive_unknown_workspace_errors() {
    let env = Env::new();
    env.cmd().args(["-archive", "ghost"]).assert().failure()
        .stderr(predicate::str::contains("no such workspace"));
}

#[test]
fn tag_outside_workspace_errors() {
    let env = Env::new();
    env.cmd().current_dir(env.home.path())
        .args(["-tag", "add", "rust"]).assert().failure()
        .stderr(predicate::str::contains("not in a workspace"));
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test meta`
Expected: FAIL to compile the binary — `main.rs` has no arm for `Cmd::Tag`.

- [ ] **Step 7: Implement the handlers**

In `src/commands.rs`, replace `pub fn list()` and add the new handlers:

```rust
/// Resolve which workspace a metadata command applies to: an explicit name,
/// else $WS_WORKSPACE, else the current directory if it is a workspace.
fn current_or_named(name: Option<String>) -> Result<(String, std::path::PathBuf)> {
    if let Some(n) = name {
        let path = registry::lookup(&n)
            .ok_or_else(|| anyhow::anyhow!("no such workspace: {n}"))?;
        return Ok((n, path));
    }
    if let Ok(n) = std::env::var("WS_WORKSPACE") {
        if let Some(path) = registry::lookup(&n) {
            return Ok((n, path));
        }
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".ws").is_dir() {
        let n = crate::meta::read(&cwd.join(".ws/workspace.toml")).name;
        let n = if n.is_empty() {
            cwd.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()
        } else {
            n
        };
        return Ok((n, cwd));
    }
    anyhow::bail!("not in a workspace (name one with --workspace, or run inside one)")
}

pub fn tag(cmd: crate::cli::TagCmd) -> Result<()> {
    use crate::cli::TagCmd;
    let (name, tags_arg) = match &cmd {
        TagCmd::Add { name, .. } | TagCmd::Rm { name, .. } | TagCmd::List { name } => {
            (name.clone(), ())
        }
    };
    let _ = tags_arg;
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    match cmd {
        TagCmd::Add { tags, .. } => {
            let all = crate::meta::add_tags(&wt, &tags)?;
            println!("{ws_name}: {}", all.join(" "));
        }
        TagCmd::Rm { tags, .. } => {
            let all = crate::meta::remove_tags(&wt, &tags)?;
            println!("{ws_name}: {}", all.join(" "));
        }
        TagCmd::List { .. } => {
            let all = crate::meta::read(&wt).tags;
            if all.is_empty() {
                println!("{ws_name}: (no tags)");
            } else {
                println!("{ws_name}: {}", all.join(" "));
            }
        }
    }
    Ok(())
}

pub fn status(name: Option<String>, text: Option<String>) -> Result<()> {
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    crate::meta::set_status(&wt, text.as_deref())?;
    match text {
        Some(t) => println!("{ws_name}: {t}"),
        None => println!("{ws_name}: status cleared"),
    }
    Ok(())
}

pub fn archive(names: Vec<String>, archived: bool) -> Result<()> {
    let mut failed = false;
    for name in names {
        let path = match registry::lookup(&name) {
            Some(p) => p,
            None => {
                eprintln!("ws: no such workspace: {name}");
                failed = true;
                continue;
            }
        };
        crate::meta::set_archived(&path.join(".ws/workspace.toml"), archived)?;
        println!("{name}: {}", if archived { "archived" } else { "unarchived" });
    }
    if failed {
        anyhow::bail!("some workspaces could not be found");
    }
    Ok(())
}

pub fn list(tag: Option<String>, archived: bool) -> Result<()> {
    let all = registry::all();
    if all.is_empty() {
        println!("no workspaces yet — create one with: ws <name>");
        return Ok(());
    }
    let mut shown = 0;
    for (name, path) in all {
        let m = crate::meta::read(&path.join(".ws/workspace.toml"));
        if m.archived && !archived {
            continue;
        }
        if let Some(t) = &tag {
            if !m.tags.iter().any(|x| x == t) {
                continue;
            }
        }
        let missing = if path.join(".ws").is_dir() { "" } else { "  (missing)" };
        let flag = if m.archived { "  [archived]" } else { "" };
        let tags = if m.tags.is_empty() { String::new() } else { format!("  [{}]", m.tags.join(" ")) };
        let status = m.status.map(|s| format!("  — {s}")).unwrap_or_default();
        println!("{name}\t{}{missing}{flag}{tags}{status}", path.display());
        shown += 1;
    }
    if shown == 0 {
        match tag {
            Some(t) => println!("no workspaces tagged {t}"),
            None => println!("no active workspaces (try: ws -list --archived)"),
        }
    }
    Ok(())
}
```

In `src/main.rs`, route them and extend the help text:

```rust
        Cmd::List { tag, archived } => commands::list(tag, archived)?,
        Cmd::Tag(c) => commands::tag(c)?,
        Cmd::Status { name, text } => commands::status(name, text)?,
        Cmd::Archive { names, archived } => commands::archive(names, archived)?,
```
```rust
         ws -list | -ls       list workspaces (--tag <t>, --archived)\n\
         ws -tag add|rm|list [--workspace <n>] <tag>...\n\
         ws -status \"<text>\" | --clear\n\
         ws -archive | -unarchive <name>...\n\
```

- [ ] **Step 8: Run the integration tests**

Run: `. "$HOME/.cargo/env"; cargo test --test meta`
Expected: PASS — 7 tests.

- [ ] **Step 9: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS, zero failures. If `tests/smoke.rs` or `tests/workspace.rs` asserted on the old `-list` output shape, update those assertions — the new columns are additive, so a `contains` check should still hold.

- [ ] **Step 10: Commit**

```bash
git add src/cli.rs src/commands.rs src/main.rs tests/meta.rs
git commit -m "feat: -tag, -status, -archive/-unarchive, and -list filters"
```

---

### Task 3: `src/search.rs` and `ws -search`

**Files:**
- Modify: `Cargo.toml` (add `grep`, `ignore`, `regex`)
- Create: `src/search.rs`
- Modify: `src/cli.rs`, `src/commands.rs`, `src/main.rs`
- Test: `tests/search.rs` (new), plus unit tests in `src/search.rs`

**Interfaces:**
- Consumes: `meta::read` (Task 1) for the archived filter; `registry::all()`.
- Produces:
  ```rust
  pub struct Hit { pub workspace: String, pub file: std::path::PathBuf, pub line: u64, pub text: String }
  pub fn search_dir(root: &Path, query: &str) -> Result<Vec<(PathBuf, u64, String)>>;  // one workspace's .ws/
  pub fn search_all(query: &str, include_archived: bool) -> Result<Vec<Hit>>;
  ```

- [ ] **Step 1: Add the dependencies**

```bash
. "$HOME/.cargo/env"; cargo add grep@0.4.1 ignore@0.4.31 regex@1.13.1
```

Expected: `Cargo.toml` `[dependencies]` gains `grep = "0.4.1"`, `ignore = "0.4.31"`, `regex = "1.13.1"`. These versions were verified to resolve and compile against Rust 1.97.1 on 2026-07-24.

- [ ] **Step 2: Write the failing tests**

Create `src/search.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a `.ws/` tree with a doc, a secret-ish local log, and an .enc file.
    fn fixture() -> TempDir {
        let d = TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("notebook")).unwrap();
        std::fs::create_dir_all(ws.join("local/log")).unwrap();
        std::fs::write(ws.join("README.md"), "# proj\n\nObjective: ship the Kraken parser\n").unwrap();
        std::fs::write(
            ws.join("notebook/notebook.me.md"),
            "day 1\nthe kraken retries on 429\nday 2\n",
        )
        .unwrap();
        std::fs::write(ws.join("local/log/session.log"), "curl kraken --key hunter2\n").unwrap();
        std::fs::write(ws.join("secrets.enc"), "kraken-ciphertext").unwrap();
        d
    }

    #[test]
    fn finds_matches_case_insensitively_with_line_numbers() {
        let d = fixture();
        let hits = search_dir(d.path(), "kraken").unwrap();
        let files: Vec<String> = hits
            .iter()
            .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(files.contains(&"README.md".to_string()), "{files:?}");
        assert!(files.contains(&"notebook.me.md".to_string()), "{files:?}");
        let nb = hits.iter().find(|(p, _, _)| p.ends_with("notebook.me.md")).unwrap();
        assert_eq!(nb.1, 2, "line numbers are 1-based");
        assert!(nb.2.contains("429"));
    }

    #[test]
    fn never_searches_local_or_encrypted_files() {
        let d = fixture();
        let hits = search_dir(d.path(), "kraken").unwrap();
        for (p, _, text) in &hits {
            let s = p.to_string_lossy();
            assert!(!s.contains("/local/"), "search must never read .ws/local: {s}");
            assert!(!s.ends_with(".enc"), "search must never read encrypted secrets: {s}");
            assert!(!text.contains("hunter2"), "a secret leaked into search output");
        }
    }

    #[test]
    fn query_is_a_literal_not_a_regex() {
        let d = fixture();
        // `.*` must match nothing here — if it were treated as a regex it would
        // match every line in the fixture.
        assert!(search_dir(d.path(), ".*").unwrap().is_empty());
        assert!(!search_dir(d.path(), "429").unwrap().is_empty());
    }

    #[test]
    fn missing_ws_dir_yields_no_hits_not_an_error() {
        let d = TempDir::new().unwrap();
        assert!(search_dir(d.path(), "anything").unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test search::`
Expected: FAIL — `cannot find function 'search_dir' in this scope`.

- [ ] **Step 4: Implement `src/search.rs`**

Put this above the test module:

```rust
//! Full-text search across workspaces, using ripgrep's own libraries.
//!
//! Scope is deliberately narrow: the committed documents under `.ws/` only.
//! `.ws/local/` (bash audit log, limits, state, lock) and `*.enc` (encrypted
//! secrets) are never opened — that is a security boundary, not an optimization.
use anyhow::Result;
use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Cap on lines reported per workspace, so one noisy notebook can't bury the rest.
pub const MAX_HITS_PER_WORKSPACE: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub workspace: String,
    pub file: PathBuf,
    pub line: u64,
    pub text: String,
}

fn is_searchable(path: &Path) -> bool {
    // Never read the local/ subtree or encrypted secrets.
    let mut in_ws_local = false;
    let comps: Vec<_> = path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    for (i, c) in comps.iter().enumerate() {
        if c == "local" && i > 0 && comps[i - 1] == ".ws" {
            in_ws_local = true;
        }
    }
    if in_ws_local {
        return false;
    }
    !matches!(path.extension().and_then(|e| e.to_str()), Some("enc"))
}

/// Search one workspace root's `.ws/` documents. Returns (file, 1-based line, text).
pub fn search_dir(root: &Path, query: &str) -> Result<Vec<(PathBuf, u64, String)>> {
    let ws_dir = root.join(".ws");
    if !ws_dir.is_dir() {
        return Ok(Vec::new());
    }
    // Literal, case-insensitive: users type words, not regexes.
    // NOTE: grep::regex has no `escape` — that's why the `regex` crate is a dep.
    let matcher = RegexMatcher::new_line_matcher(&format!("(?i){}", regex::escape(query)))?;
    let mut searcher: Searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();

    let mut out = Vec::new();
    for entry in WalkBuilder::new(&ws_dir).hidden(false).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable entry — skip, never abort the search
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !is_searchable(&path) {
            continue;
        }
        let mut file_hits: Vec<(PathBuf, u64, String)> = Vec::new();
        let res = searcher.search_path(
            &matcher,
            &path,
            UTF8(|lnum, line| {
                file_hits.push((path.clone(), lnum, line.trim_end().to_string()));
                Ok(true)
            }),
        );
        if res.is_ok() {
            out.extend(file_hits);
        }
        if out.len() >= MAX_HITS_PER_WORKSPACE {
            out.truncate(MAX_HITS_PER_WORKSPACE);
            break;
        }
    }
    Ok(out)
}

/// Search every registered workspace, skipping archived ones unless asked.
pub fn search_all(query: &str, include_archived: bool) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for (name, path) in crate::registry::all() {
        let m = crate::meta::read(&path.join(".ws/workspace.toml"));
        if m.archived && !include_archived {
            continue;
        }
        for (file, line, text) in search_dir(&path, query)? {
            hits.push(Hit { workspace: name.clone(), file, line, text });
        }
    }
    Ok(hits)
}
```

Add `mod search;` to `src/main.rs`.

- [ ] **Step 5: Run the unit tests**

Run: `. "$HOME/.cargo/env"; cargo test search::`
Expected: PASS — 4 tests.

- [ ] **Step 6: Write the failing CLI + integration tests**

Add to `src/cli.rs`'s test module:

```rust
    #[test]
    fn search_parses_query_and_flag() {
        assert_eq!(
            p(&["-search", "kraken"]),
            Cmd::Search { query: "kraken".into(), include_archived: false }
        );
        assert_eq!(
            p(&["-search", "kraken", "--include-archived"]),
            Cmd::Search { query: "kraken".into(), include_archived: true }
        );
        assert!(parse(vec!["-search".into()]).is_err());
    }
```

Create `tests/search.rs`:

```rust
mod common;
use common::Env;
use predicates::prelude::*;

fn make_ws(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    dir
}

#[test]
fn search_finds_notebook_text_across_workspaces() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    let b = make_ws(&env, "beta");
    std::fs::write(a.join(".ws/notebook/notes.md"), "the kraken retries on 429\n").unwrap();
    std::fs::write(b.join(".ws/README.md"), "# beta\nno sea monsters here\n").unwrap();

    env.cmd().args(["-search", "kraken"]).assert().success()
        .stdout(predicate::str::contains("alpha"))
        .stdout(predicate::str::contains("429"))
        .stdout(predicate::str::contains("beta").not());
}

#[test]
fn search_skips_archived_unless_asked() {
    let env = Env::new();
    let a = make_ws(&env, "old");
    std::fs::write(a.join(".ws/notebook/notes.md"), "kraken lore\n").unwrap();
    env.cmd().args(["-archive", "old"]).assert().success();

    env.cmd().args(["-search", "kraken"]).assert().success()
        .stdout(predicate::str::contains("old").not());
    env.cmd().args(["-search", "kraken", "--include-archived"]).assert().success()
        .stdout(predicate::str::contains("old"));
}

#[test]
fn search_never_returns_local_log_contents() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    std::fs::create_dir_all(a.join(".ws/local/log")).unwrap();
    std::fs::write(a.join(".ws/local/log/session.log"), "export TOKEN=hunter2\n").unwrap();

    env.cmd().args(["-search", "hunter2"]).assert().success()
        .stdout(predicate::str::contains("hunter2").not())
        .stdout(predicate::str::contains("no matches"));
}

#[test]
fn search_with_no_matches_says_so() {
    let env = Env::new();
    make_ws(&env, "alpha");
    env.cmd().args(["-search", "zzzznope"]).assert().success()
        .stdout(predicate::str::contains("no matches"));
}
```

- [ ] **Step 7: Run to verify they fail**

Run: `. "$HOME/.cargo/env"; cargo test --test search`
Expected: FAIL to compile — `Cmd::Search` doesn't exist yet.

- [ ] **Step 8: Wire up the CLI**

In `src/cli.rs`, add the variant and the parse arm:

```rust
    Search { query: String, include_archived: bool },
```
```rust
        "-search" => {
            let mut query = None;
            let mut include_archived = false;
            for a in it {
                match a.as_str() {
                    "--include-archived" => include_archived = true,
                    other if query.is_none() => query = Some(other.to_string()),
                    other => bail!("unexpected argument: {other}"),
                }
            }
            let query = query.ok_or_else(|| anyhow::anyhow!("usage: ws -search <query> [--include-archived]"))?;
            Ok(Cmd::Search { query, include_archived })
        }
```

In `src/commands.rs`:

```rust
pub fn search(query: String, include_archived: bool) -> Result<()> {
    let hits = crate::search::search_all(&query, include_archived)?;
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(());
    }
    let mut current = String::new();
    for h in &hits {
        if h.workspace != current {
            current = h.workspace.clone();
            println!("\n{current}");
        }
        // Show the path relative to the workspace root when we can — the absolute
        // prefix is noise once the workspace name is the heading.
        let shown = h
            .file
            .iter()
            .skip_while(|c| *c != ".ws")
            .collect::<std::path::PathBuf>();
        println!("  {}:{}: {}", shown.display(), h.line, h.text.trim());
    }
    println!("\n{} match(es) in {} workspace(s)", hits.len(), {
        let mut names: Vec<_> = hits.iter().map(|h| h.workspace.as_str()).collect();
        names.sort();
        names.dedup();
        names.len()
    });
    Ok(())
}
```

In `src/main.rs`:

```rust
        Cmd::Search { query, include_archived } => commands::search(query, include_archived)?,
```
and add to the help text:
```rust
         ws -search <query>   search all workspaces (--include-archived)\n\
```

- [ ] **Step 9: Run the search tests**

Run: `. "$HOME/.cargo/env"; cargo test --test search`
Expected: PASS — 4 tests.

- [ ] **Step 10: Run the full suite and commit**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS, zero failures.

```bash
git add Cargo.toml Cargo.lock src/search.rs src/cli.rs src/commands.rs src/main.rs tests/search.rs
git commit -m "feat: ws -search — full-text search across workspaces (grep + ignore)"
```

---

### Task 4: `src/migrate.rs` and `ws migrate-cs`

**Files:**
- Create: `src/migrate.rs`
- Modify: `src/cli.rs`, `src/commands.rs`, `src/main.rs`
- Test: `tests/migrate.rs` (new), plus unit tests in `src/migrate.rs`

**Interfaces:**
- Consumes: `contract::init(name, root, agent, commit) -> Result<()>` (creates the `.ws/` skeleton with `write_if_absent` semantics, git-inits if needed, and registers the workspace); `meta::{update, set_archived, add_tags, set_status}`; `context::regenerate(path, name)`; `config::{load, sessions_root}`; `agents::for_id(id)?.context_file()`.
- Produces:
  ```rust
  pub struct Plan { pub name: String, pub source: PathBuf, pub dest: PathBuf, pub in_place: bool }
  pub fn cs_root() -> PathBuf;                              // $WS_CS_ROOT or ~/.claude-sessions
  pub fn discover(root: &Path) -> Vec<(String, PathBuf)>;   // (name, entry path) for dirs/symlinks with .cs/
  pub fn plan_for(name: &str, entry: &Path, sessions_root: &Path) -> Result<Plan>;
  pub fn migrate(plan: &Plan, default_agent: &str, dry_run: bool) -> Result<Vec<String>>;  // returns log lines
  ```

**Design notes the implementer must not deviate from** (all verified against the real `~/.claude-sessions` on 2026-07-24):

- A cs entry may be a **symlink to a live project directory** (8 of 20 real ones are). For a symlink, `in_place = true`: the destination is the *resolved target*, and `.ws/` is created there beside the existing `.cs/`. **Never copy a project tree.** For a real directory, `in_place = false` and the whole tree is copied to `<sessions_root>/<name>`.
- File mapping from `<source>/.cs/`:
  | cs | ws |
  |---|---|
  | `README.md` | `.ws/README.md` |
  | `memory/narrative.<actor>.md` | `.ws/notebook/notebook.<actor>.md` |
  | `memory/*` (everything else) | `.ws/memory/*` |
  | `handoffs/*` | `.ws/handoffs/*` |
  | `artifacts/*` | `.ws/artifacts/*` |
  | `plans/*` | `.ws/plans/*` |
  | `checkpoints/*` | `.ws/checkpoints/*` |
  | `timeline.jsonl` | `.ws/timeline.jsonl` |
  | `summary.md` | `.ws/summary.md` |
  | `local/**` | **not copied** (session.log, limits, state, lock) |
  | `.narrative-reminder-cooldown`, `session.lock` | not copied |
- cs `README.md` starts with YAML frontmatter (`status:`, `created:`, `tags: []`, `aliases: [...]`). Parse it: `tags` → `workspace.toml` tags; `status: archived` → `archived = true` (any other value → not archived). The frontmatter stays in the copied README (harmless, and it carries history).
- Secrets are **not** moved automatically. After a successful migration, run `cs -secrets list` with cwd = the source dir; if it prints names, print them with the exact re-store command. Never read a value, never fail the migration if `cs` is missing.

- [ ] **Step 1: Write the failing unit tests**

Create `src/migrate.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A self-contained cs session: real dir with .cs/ and a project file.
    fn cs_session(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        let cs = dir.join(".cs");
        std::fs::create_dir_all(cs.join("memory")).unwrap();
        std::fs::create_dir_all(cs.join("local/log")).unwrap();
        std::fs::create_dir_all(cs.join("plans")).unwrap();
        std::fs::write(
            cs.join("README.md"),
            "---\nstatus: active\ncreated: 2026-07-01\ntags: [\"rust\", \"cli\"]\n---\n# Session: x\n\n## Objective\n\nShip it\n",
        )
        .unwrap();
        std::fs::write(cs.join("memory/narrative.me.md"), "day 1\n").unwrap();
        std::fs::write(cs.join("memory/MEMORY.md"), "- index\n").unwrap();
        std::fs::write(cs.join("plans/p.md"), "plan\n").unwrap();
        std::fs::write(cs.join("timeline.jsonl"), "{\"e\":1}\n").unwrap();
        std::fs::write(cs.join("local/log/session.log"), "TOKEN=hunter2\n").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn discover_finds_sessions_and_skips_non_sessions() {
        let d = TempDir::new().unwrap();
        cs_session(d.path(), "alpha");
        std::fs::create_dir_all(d.path().join("not-a-session")).unwrap();
        std::fs::write(d.path().join("index.md"), "# Sessions\n").unwrap();

        let found = discover(d.path());
        let names: Vec<_> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    #[test]
    fn plan_for_symlink_migrates_in_place() {
        let d = TempDir::new().unwrap();
        let project = cs_session(d.path(), "real-project");
        let cs_root = d.path().join("cs");
        std::fs::create_dir_all(&cs_root).unwrap();
        let link = cs_root.join("proj");
        std::os::unix::fs::symlink(&project, &link).unwrap();

        let sessions_root = d.path().join("agent-workspaces");
        let plan = plan_for("proj", &link, &sessions_root).unwrap();
        assert!(plan.in_place, "a symlinked cs session must never be copied");
        assert_eq!(
            plan.dest.canonicalize().unwrap(),
            project.canonicalize().unwrap()
        );
    }

    #[test]
    fn plan_for_real_dir_copies_into_sessions_root() {
        let d = TempDir::new().unwrap();
        let src = cs_session(d.path(), "alpha");
        let sessions_root = d.path().join("agent-workspaces");
        let plan = plan_for("alpha", &src, &sessions_root).unwrap();
        assert!(!plan.in_place);
        assert_eq!(plan.dest, sessions_root.join("alpha"));
    }

    #[test]
    fn frontmatter_parsing() {
        let fm = parse_frontmatter(
            "---\nstatus: archived\ncreated: 2026-07-01\ntags: [\"rust\", \"cli\"]\naliases: [\"a\"]\n---\n# body\n",
        );
        assert!(fm.archived);
        assert_eq!(fm.tags, vec!["rust".to_string(), "cli".to_string()]);

        let none = parse_frontmatter("# no frontmatter\n");
        assert!(!none.archived);
        assert!(none.tags.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test migrate::`
Expected: FAIL — `cannot find function 'discover' in this scope`.

- [ ] **Step 3: Implement `src/migrate.rs`**

```rust
//! Import `cs` sessions (`~/.claude-sessions/<name>`) into ws workspaces.
//!
//! Copies, never moves: cs stays fully usable afterwards and nothing is ever
//! written into `~/.claude-sessions`.
//!
//! A cs entry may be a SYMLINK to a live project directory (adopted sessions —
//! 8 of 20 on the author's machine). Those are migrated *in place*: `.ws/` is
//! created inside the real project next to the existing `.cs/`. Only a genuine
//! session directory is copied.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub name: String,
    /// The cs session directory (symlink resolved).
    pub source: PathBuf,
    /// Where the ws workspace will live.
    pub dest: PathBuf,
    /// True when dest is an existing project we must not copy into place.
    pub in_place: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct Frontmatter {
    pub archived: bool,
    pub tags: Vec<String>,
}

/// `$WS_CS_ROOT` (test seam) or `~/.claude-sessions`.
pub fn cs_root() -> PathBuf {
    if let Some(p) = std::env::var_os("WS_CS_ROOT") {
        return PathBuf::from(p);
    }
    dirs::home_dir().unwrap_or_default().join(".claude-sessions")
}

/// Every entry under `root` that looks like a cs session (has a `.cs/` dir).
pub fn discover(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let path = e.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        // `path.join(".cs")` follows symlinks, which is what we want here.
        if path.join(".cs").is_dir() {
            out.push((name, path));
        }
    }
    out.sort();
    out
}

pub fn plan_for(name: &str, entry: &Path, sessions_root: &Path) -> Result<Plan> {
    let meta = std::fs::symlink_metadata(entry)
        .with_context(|| format!("cannot stat {}", entry.display()))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::canonicalize(entry)
            .with_context(|| format!("dangling symlink: {}", entry.display()))?;
        return Ok(Plan {
            name: name.to_string(),
            source: target.clone(),
            dest: target,
            in_place: true,
        });
    }
    Ok(Plan {
        name: name.to_string(),
        source: entry.to_path_buf(),
        dest: sessions_root.join(name),
        in_place: false,
    })
}

/// Parse the YAML frontmatter cs writes at the top of `.cs/README.md`.
/// Only the two fields ws cares about; anything else is ignored.
pub fn parse_frontmatter(readme: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut lines = readme.lines();
    if lines.next().map(str::trim) != Some("---") {
        return fm;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("status:") {
            fm.archived = v.trim() == "archived";
        }
        if let Some(v) = line.strip_prefix("tags:") {
            fm.tags = v
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    fm
}

/// Recursively copy `src` into `dst`, skipping any path for which `skip` is true.
fn copy_tree(src: &Path, dst: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<()> {
    if skip(src) {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        return Ok(()); // don't chase symlinks inside a session tree
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            copy_tree(&e.path(), &dst.join(e.file_name()), skip)?;
        }
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Copy one file if it exists. Returns whether it was copied.
fn copy_file(src: &Path, dst: &Path) -> Result<bool> {
    if !src.is_file() {
        return Ok(false);
    }
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::copy(src, dst)?;
    Ok(true)
}

/// Execute a plan. Returns the log lines to print.
pub fn migrate(plan: &Plan, default_agent: &str, dry_run: bool) -> Result<Vec<String>> {
    let mut log = Vec::new();
    let cs = plan.source.join(".cs");
    if !cs.is_dir() {
        anyhow::bail!("{} has no .cs directory", plan.source.display());
    }
    if plan.dest.join(".ws").is_dir() {
        anyhow::bail!("{} is already a ws workspace (skipping)", plan.dest.display());
    }
    if dry_run {
        log.push(format!(
            "would migrate {} → {} ({})",
            plan.source.display(),
            plan.dest.display(),
            if plan.in_place { "in place — symlinked project" } else { "copy" }
        ));
        return Ok(log);
    }

    // 1. Materialize the destination.
    if plan.in_place {
        log.push(format!("{}: adopting in place at {}", plan.name, plan.dest.display()));
    } else {
        // Copy the whole session tree except .cs (mapped below) — project files,
        // .git history and all.
        let cs_dir = cs.clone();
        copy_tree(&plan.source, &plan.dest, &|p: &Path| p == cs_dir)?;
        log.push(format!("{}: copied {} → {}", plan.name, plan.source.display(), plan.dest.display()));
    }

    // 2. ws skeleton (write_if_absent — never clobbers what we copy in next),
    //    registry entry, git init when needed.
    crate::contract::init(&plan.name, &plan.dest, default_agent, /* commit */ false)?;
    let ws = plan.dest.join(".ws");

    // 3. Map cs files onto the ws contract. NOTE: .cs/local/ is never copied —
    //    it holds the bash audit log, limits, state and the lock.
    let readme = cs.join("README.md");
    let readme_text = std::fs::read_to_string(&readme).unwrap_or_default();
    if copy_file(&readme, &ws.join("README.md"))? {
        log.push("  README.md".into());
    }
    if copy_file(&cs.join("timeline.jsonl"), &ws.join("timeline.jsonl"))? {
        log.push("  timeline.jsonl".into());
    }
    if copy_file(&cs.join("summary.md"), &ws.join("summary.md"))? {
        log.push("  summary.md".into());
    }
    // memory/: narratives become notebooks, everything else stays memory.
    if let Ok(rd) = std::fs::read_dir(cs.join("memory")) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            let dst = match fname.strip_prefix("narrative.") {
                Some(rest) => ws.join("notebook").join(format!("notebook.{rest}")),
                None => ws.join("memory").join(&fname),
            };
            if copy_file(&e.path(), &dst)? {
                log.push(format!("  memory/{fname} → {}", dst.strip_prefix(&ws).unwrap_or(&dst).display()));
            }
        }
    }
    for sub in ["handoffs", "artifacts", "plans", "checkpoints"] {
        let src = cs.join(sub);
        if src.is_dir() {
            copy_tree(&src, &ws.join(sub), &|_| false)?;
            log.push(format!("  {sub}/"));
        }
    }

    // 4. workspace.toml enrichment from the cs README frontmatter.
    let fm = parse_frontmatter(&readme_text);
    let wt = ws.join("workspace.toml");
    if !fm.tags.is_empty() {
        crate::meta::add_tags(&wt, &fm.tags)?;
        log.push(format!("  tags: {}", fm.tags.join(" ")));
    }
    if fm.archived {
        crate::meta::set_archived(&wt, true)?;
        log.push("  archived (was archived in cs)".into());
    }
    crate::meta::update(&wt, |t| {
        t.insert("migrated_from".into(), toml::Value::String("cs".into()));
    })?;

    // 5. Context file for the default agent.
    let agent = crate::agents::for_id(default_agent)?;
    crate::context::regenerate(&plan.dest.join(agent.context_file()), &plan.name)?;

    let _ = crate::timeline::record(
        &ws.join("timeline.jsonl"),
        "migrated",
        &crate::actors::actor_slug(),
        serde_json::json!({ "from": plan.source.to_string_lossy(), "tool": "cs" }),
    );

    // 6. Secrets: names only, never values. Best-effort.
    if let Some(names) = cs_secret_names(&plan.source) {
        if !names.is_empty() {
            log.push(format!(
                "  secrets NOT migrated (values are never read): {}",
                names.join(" ")
            ));
            log.push(format!(
                "    re-store each with:  cs -secrets get <NAME> | WS_WORKSPACE={} ws -secrets set <NAME>",
                plan.name
            ));
        }
    }
    Ok(log)
}

/// Ask `cs` which secrets a session has. Names only — never values.
/// Returns None when cs isn't installed or the call fails.
fn cs_secret_names(source: &Path) -> Option<Vec<String>> {
    let out = std::process::Command::new("cs")
        .arg("-secrets")
        .arg("list")
        .current_dir(source)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("No secrets stored") {
        return Some(Vec::new());
    }
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.contains(' '))
            .map(String::from)
            .collect(),
    )
}
```

Add `mod migrate;` to `src/main.rs`.

(`crate::context::regenerate(path, name)` exists at `src/context.rs:27` — verified. Do not add a wrapper.)

- [ ] **Step 4: Run the unit tests**

Run: `. "$HOME/.cargo/env"; cargo test migrate::`
Expected: PASS — 4 tests.

- [ ] **Step 5: Write the failing CLI + integration tests**

Add to `src/cli.rs`'s test module:

```rust
    #[test]
    fn migrate_cs_parses() {
        assert_eq!(
            p(&["migrate-cs", "--all"]),
            Cmd::MigrateCs { names: vec![], all: true, dry_run: false }
        );
        assert_eq!(
            p(&["migrate-cs", "alpha", "beta"]),
            Cmd::MigrateCs { names: vec!["alpha".into(), "beta".into()], all: false, dry_run: false }
        );
        assert_eq!(
            p(&["migrate-cs", "--all", "--dry-run"]),
            Cmd::MigrateCs { names: vec![], all: true, dry_run: true }
        );
        // neither names nor --all is a usage error
        assert!(parse(vec!["migrate-cs".into()]).is_err());
    }
```

Create `tests/migrate.rs`:

```rust
mod common;
use common::Env;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

/// Build a fake cs session under `root`.
fn cs_session(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    let cs = dir.join(".cs");
    std::fs::create_dir_all(cs.join("memory")).unwrap();
    std::fs::create_dir_all(cs.join("local/log")).unwrap();
    std::fs::create_dir_all(cs.join("handoffs")).unwrap();
    std::fs::write(
        cs.join("README.md"),
        "---\nstatus: active\ntags: [\"rust\"]\n---\n# Session: x\n\n## Objective\n\nShip the parser\n",
    )
    .unwrap();
    std::fs::write(cs.join("memory/narrative.me.md"), "day 1: found the bug\n").unwrap();
    std::fs::write(cs.join("memory/MEMORY.md"), "- [note](n.md)\n").unwrap();
    std::fs::write(cs.join("handoffs/h.md"), "handoff\n").unwrap();
    std::fs::write(cs.join("timeline.jsonl"), "{\"event\":\"created\"}\n").unwrap();
    std::fs::write(cs.join("local/log/session.log"), "TOKEN=hunter2\n").unwrap();
    std::fs::write(dir.join("app.py"), "print('hi')\n").unwrap();
    dir
}

#[test]
fn migrates_a_real_session_directory() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    let src = cs_session(&cs_root, "alpha");

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"])
        .assert().success()
        .stdout(predicate::str::contains("alpha"));

    let dest = env.root.join("alpha");
    assert!(dest.join(".ws/workspace.toml").is_file());
    assert!(dest.join(".ws/README.md").is_file());
    assert!(dest.join(".ws/notebook/notebook.me.md").is_file(), "narrative → notebook");
    assert!(dest.join(".ws/memory/MEMORY.md").is_file());
    assert!(dest.join(".ws/handoffs/h.md").is_file());
    assert!(dest.join(".ws/timeline.jsonl").is_file());
    assert!(dest.join("app.py").is_file(), "project files come along");
    // .cs/local is NEVER copied — it holds the bash audit log.
    assert!(!dest.join(".ws/local/log/session.log").exists());
    assert!(!dest.join(".cs").exists(), ".cs is mapped, not copied verbatim");
    // the tag from the cs frontmatter landed
    let wt = std::fs::read_to_string(dest.join(".ws/workspace.toml")).unwrap();
    assert!(wt.contains("rust"), "{wt}");
    // cs is untouched
    assert!(src.join(".cs/README.md").is_file());
    assert!(!src.join(".ws").exists());

    // and it's registered
    env.cmd().args(["-list"]).assert().success()
        .stdout(predicate::str::contains("alpha"));
}

#[test]
fn symlinked_session_is_adopted_in_place_not_copied() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    // The real project lives elsewhere; cs only holds a symlink to it.
    let project = cs_session(env.home.path(), "real-project");
    std::os::unix::fs::symlink(&project, cs_root.join("proj")).unwrap();

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "proj"])
        .assert().success();

    // .ws was created inside the real project, beside .cs — nothing was copied
    // into the sessions root.
    assert!(project.join(".ws/workspace.toml").is_file());
    assert!(project.join(".cs/README.md").is_file(), "cs stays usable");
    assert!(!env.root.join("proj").exists(), "a symlinked session must not be copied");
}

#[test]
fn migrate_all_and_dry_run() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "alpha");
    cs_session(&cs_root, "beta");
    std::fs::write(cs_root.join("index.md"), "# Sessions\n").unwrap();

    // dry run changes nothing
    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all", "--dry-run"])
        .assert().success()
        .stdout(predicate::str::contains("would migrate"));
    assert!(!env.root.join("alpha").exists());

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "--all"])
        .assert().success();
    assert!(env.root.join("alpha/.ws").is_dir());
    assert!(env.root.join("beta/.ws").is_dir());
}

#[test]
fn migrating_twice_refuses_the_second_time() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    cs_session(&cs_root, "alpha");

    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"]).assert().success();
    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "alpha"])
        .assert().failure()
        .stderr(predicate::str::contains("already a ws workspace"));
}

#[test]
fn unknown_session_name_errors() {
    let env = Env::new();
    let cs_root = env.home.path().join("claude-sessions");
    std::fs::create_dir_all(&cs_root).unwrap();
    env.cmd().env("WS_CS_ROOT", &cs_root).args(["migrate-cs", "ghost"])
        .assert().failure()
        .stderr(predicate::str::contains("no cs session named ghost"));
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `. "$HOME/.cargo/env"; cargo test --test migrate`
Expected: FAIL to compile — `Cmd::MigrateCs` doesn't exist.

- [ ] **Step 7: Wire up the CLI**

In `src/cli.rs`:

```rust
    MigrateCs { names: Vec<String>, all: bool, dry_run: bool },
```
and, in `parse`, next to the other bare-word subcommands (`"setup"`, `"internal"`):
```rust
        "migrate-cs" => {
            let mut names = Vec::new();
            let mut all = false;
            let mut dry_run = false;
            for a in it {
                match a.as_str() {
                    "--all" => all = true,
                    "--dry-run" => dry_run = true,
                    other if other.starts_with("--") => bail!("unexpected argument: {other}"),
                    other => names.push(other.to_string()),
                }
            }
            if names.is_empty() && !all {
                bail!("usage: ws migrate-cs <name>... | --all [--dry-run]");
            }
            Ok(Cmd::MigrateCs { names, all, dry_run })
        }
```

In `src/commands.rs`:

```rust
pub fn migrate_cs(names: Vec<String>, all: bool, dry_run: bool) -> Result<()> {
    let cfg = config::load();
    let sessions_root = config::sessions_root(&cfg);
    let cs_root = crate::migrate::cs_root();
    let found = crate::migrate::discover(&cs_root);
    if found.is_empty() {
        anyhow::bail!("no cs sessions found under {}", cs_root.display());
    }

    let selected: Vec<(String, std::path::PathBuf)> = if all {
        found
    } else {
        let mut out = Vec::new();
        for n in &names {
            match found.iter().find(|(name, _)| name == n) {
                Some(hit) => out.push(hit.clone()),
                None => anyhow::bail!("no cs session named {n} under {}", cs_root.display()),
            }
        }
        out
    };

    let mut failed = false;
    for (name, entry) in selected {
        let plan = match crate::migrate::plan_for(&name, &entry, &sessions_root) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ws: {name}: {e:#}");
                failed = true;
                continue;
            }
        };
        match crate::migrate::migrate(&plan, &cfg.default_agent, dry_run) {
            Ok(log) => {
                for line in log {
                    println!("{line}");
                }
            }
            Err(e) => {
                eprintln!("ws: {name}: {e:#}");
                failed = true;
            }
        }
    }
    if failed {
        anyhow::bail!("some sessions could not be migrated");
    }
    Ok(())
}
```

In `src/main.rs`:

```rust
        Cmd::MigrateCs { names, all, dry_run } => commands::migrate_cs(names, all, dry_run)?,
```
and the help line:
```rust
         ws migrate-cs <name>...|--all   import cs sessions (--dry-run)\n\
```

- [ ] **Step 8: Run the migration tests**

Run: `. "$HOME/.cargo/env"; cargo test --test migrate`
Expected: PASS — 5 tests.

- [ ] **Step 9: Run the full suite**

Run: `. "$HOME/.cargo/env"; cargo test`
Expected: PASS, zero failures (~150 tests).

- [ ] **Step 10: Live dry-run against the real cs sessions (read-only)**

Run: `. "$HOME/.cargo/env"; cargo run --quiet -- migrate-cs --all --dry-run`
Expected: 20 `would migrate …` lines. The 8 symlinked sessions (coach, finance, hunger, invest, keystone, lilo, milo, ws) must say **"in place — symlinked project"**; the rest say "copy". Confirm nothing changed: `git -C ~/.claude-sessions status` is not applicable (it's not a repo), so instead verify no `.ws` appeared: `find ~/.claude-sessions -maxdepth 2 -name .ws` prints nothing.

- [ ] **Step 11: Commit**

```bash
git add src/migrate.rs src/cli.rs src/commands.rs src/main.rs tests/migrate.rs
git commit -m "feat: ws migrate-cs — import cs sessions (copy-only, symlink-aware)"
```

---

## Self-Review

**Spec coverage (§17.6):** `-search <query> [--include-archived]` → Task 3. `-tag add|rm|list` → Task 2. `-archive`/`-unarchive` + `-list` exclusion → Task 2. `-status "<text>"|--clear` → Task 2. `migrate-cs [<name>…|--all]` with the §15 mapping (README, narratives→notebook, memory, artifacts, handoffs, timeline, generated workspace.toml + context files, cs untouched, secrets documented) → Task 4. `-list [--tag t]` from the §14 CLI table → Task 2.

**Deliberate additions beyond the spec, flagged for the reviewer:** `--dry-run` on `migrate-cs` (a 20-session import deserves a preview), `--workspace <name>` on `-tag`/`-status` (otherwise they only work from inside the workspace), and `migrated_from = "cs"` in workspace.toml (provenance). The symlink-aware in-place migration is not in the spec text but is forced by the real data.

**Type consistency:** `meta::read` returns `Meta` everywhere; `search_dir` returns `Vec<(PathBuf, u64, String)>` and `search_all` wraps those into `Hit`; `Plan`/`Frontmatter` are used only inside `migrate.rs` and its tests; `TagCmd` variants carry `name: Option<String>` in all three arms. `Cmd::List` gains fields in Task 2, which is why Task 2 owns updating both existing `Ok(Cmd::List)` sites and `main.rs`.
