# Notebook rollover + cs removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** At session start, ws moves old notebook entries into an archive on its own. Separately, every reference to cs comes out of the repo, so ws stands as an independent tool.

**Architecture:** A new `src/rollover.rs` holds pure functions (split a notebook into entries, pick what to keep, add a pointer) and one fs entry point, `roll_over`, which runs under `txn::transaction`. `internal::session_start` calls it for the current actor on `startup` and `resume`. The cs removal is a set of edits to comments, tests, the quiet-notes mod, git config files and docs, with no behaviour change to ws itself.

**Tech Stack:** Rust (existing crate: anyhow, tempfile, assert_cmd), TypeScript for the Claude Code mod.

**Spec:** `docs/superpowers/specs/2026-11-24-startup-context-trim-design.md` (rollover). The cs removal comes from the user's request "remove all of the cs stuff" (2026-11-24) and has no spec. Its scope is in Tasks 3–4.

## Global Constraints

- Roll-over trigger: notebook file over **32 KB** (`32 * 1024` bytes); keep the newest whole entries that fit in **16 KB** (`16 * 1024`); the newest entry always stays.
- Archive path: `.ws/notebook-archive/notebook.<actor>.md`, append-only, oldest first.
- Pointer line, exact copy: `Older entries live in .ws/notebook-archive/notebook.<actor>.md. Read them only when you need that history.`
- Only the current actor's notebook; only on hook source `startup` or `resume`.
- Archive append happens before the notebook rewrite; the rewrite uses `atomic::atomic_write`; failures are logged to the session log and never fail the hook.
- ws contains nothing cs-specific afterwards (see memory `ws-is-independent-of-cs`).
- Before any cargo command: `export PATH="$HOME/.cargo/bin:$PATH"`. CI runs `cargo fmt --check` and clippy.
- Tests that drive `ws internal …` go through `tests/common::Env`, which already clears leaked `WS_*`.

## Review Focus

1. **A `## ` line inside a fenced code block** (an entry quoting markdown) must not start a new entry. Splitting there would separate a block from its entry. Test: `fenced_heading_is_not_an_entry` (Task 1).
2. **A notebook with no `## ` headings at all** (one long freeform file) must be left untouched. There's nothing whole to move. Test: `no_entries_means_no_roll` (Task 1).
3. **A second session start right after a roll** must not roll again or add a second pointer. Test: `second_roll_is_a_noop` (Task 1).
4. **An existing archive file** must be appended to, never overwritten, with the older content first. Test: `archive_is_appended_in_order` (Task 1).
5. **CRLF line endings** (a notebook edited on Windows or by Codex) must still split on headings and round-trip byte-for-byte for kept entries. Test: `crlf_notebook_round_trips` (Task 1).

---

### Task 1: `src/rollover.rs`

**Files:**
- Create: `src/rollover.rs`
- Modify: `src/main.rs` (add `mod rollover;` in the alphabetical `mod` list)
- Modify: `src/workspace.rs` (add `notebook_archive_dir`, next to `notebook_dir` at line 39)

**Interfaces:**
- Produces:
  - `Workspace::notebook_archive_dir(&self) -> PathBuf` = `self.ws_dir().join("notebook-archive")`
  - `pub const ROLL_OVER_BYTES: usize = 32 * 1024;`
  - `pub const KEEP_BYTES: usize = 16 * 1024;`
  - `fn split_entries(text: &str) -> (&str, Vec<&str>)`: preamble (everything before the first entry) and entries. An entry starts at a line beginning `## ` outside a ``` fence and runs up to the next one. Slices concatenate back to `text` exactly.
  - `fn first_kept(entries: &[&str], budget: usize) -> usize`: index of the oldest entry kept, walking newest to oldest while the running byte total stays ≤ `budget`; never more than `entries.len() - 1`.
  - `fn with_pointer(preamble: &str, actor: &str) -> String`: the preamble with the pointer line inserted after the first line (the `# ` title) followed by a blank line; unchanged if the exact pointer line is already present.
  - `pub fn roll_over(ws: &Workspace, actor: &str) -> anyhow::Result<usize>`: number of entries moved (0 when under the trigger, no entries, or nothing to move).

- [ ] **Step 1: Write the failing unit tests** in `#[cfg(test)] mod tests` of `src/rollover.rs`. Use `tempfile::TempDir` and build a `Workspace` pointing at it the way the existing `workspace.rs` tests do.
  - `splits_preamble_and_entries`: `"# T\n\nintro\n## a\nx\n## b\ny\n"` → preamble `"# T\n\nintro\n"`, entries `["## a\nx\n", "## b\ny\n"]`.
  - `fenced_heading_is_not_an_entry`: `"# T\n## a\n```\n## not\n```\n## b\n"` → 2 entries, the first containing `"## not"`.
  - `first_kept_respects_budget`: entries of 10, 10, 10 bytes, budget 25 → `1`; budget 5 → `2` (newest always kept).
  - `under_trigger_is_untouched`: 1 KB notebook → `roll_over` returns 0, file bytes unchanged, no archive dir.
  - `no_entries_means_no_roll`: 40 KB with no `## ` line → 0, unchanged.
  - `rolls_oldest_into_archive`: 40 entries of ~1 KB each (`"## e{i}\n" + "x".repeat(1000)`) → returns n > 0. The notebook is ≤ 16 KB plus preamble and pointer, starts with the title, contains the pointer exactly once and ends with entry 39. The archive starts with `# Notebook archive (<actor>)` and holds `## e0` .. `## e{n-1}` in order.
  - `second_roll_is_a_noop`: after `rolls_oldest_into_archive`'s roll, calling again returns 0 and leaves both files' bytes unchanged.
  - `archive_is_appended_in_order`: an existing archive with `"# Notebook archive (me)\n\n## old\n"`; after a roll the archive begins with that exact text and `## e0` follows it.
  - `crlf_notebook_round_trips`: the same as `rolls_oldest_into_archive` with `\r\n` endings. The kept entries are byte-identical to their input slices.

- [ ] **Step 2: Run the tests to verify they fail**
  Run: `cargo test rollover::`. Expected: compile error / FAIL (module items missing).

- [ ] **Step 3: Implement** the Interfaces items. In `roll_over`: build the notebook path as `ws.notebook_dir().join(format!("notebook.{actor}.md"))`. A missing file returns 0. Do the read, decide and write inside `crate::txn::transaction(&notebook_path, …)`. Append the moved slices to the archive with `OpenOptions::new().create(true).append(true)`, write `# Notebook archive ({actor})\n\n` first only when the file is new or empty, and `sync_all` it. Then `atomic_write` the notebook as `with_pointer(preamble) + kept entries`.

- [ ] **Step 4: Run the tests to verify they pass**
  Run: `cargo test rollover::`. Expected: all 9 pass. Then run `cargo fmt --check && cargo clippy --all-targets -- -D warnings`, which should be clean.

- [ ] **Step 5: Commit**
  `git add src/rollover.rs src/main.rs src/workspace.rs && git commit -m "feat(notebook): roll old entries into .ws/notebook-archive"`

### Task 2: Run the rollover at session start

**Files:**
- Modify: `src/internal.rs` (`session_start`, after `record_session_identity(&ws, &h);` at about line 87)
- Test: `tests/internal.rs`
- Modify: `README.md` (the section that describes `.ws/notebook/`), `CHANGELOG.md` (`## [Unreleased]`)

**Interfaces:**
- Consumes: `rollover::roll_over(ws: &Workspace, actor: &str) -> anyhow::Result<usize>`

- [ ] **Step 1: Write the failing integration test** `session_start_rolls_an_oversized_notebook`. Call `adopt_ws(&env, "proj")` and get the actor from `env.cmd().current_dir(&proj).arg("-whoami")` stdout, trimmed. Write `.ws/notebook/notebook.<actor>.md` with 40 one-KB entries and run `session_start(&env, "proj", &proj, "startup")`. Assert that `.ws/notebook-archive/notebook.<actor>.md` exists and contains `## e0`, that the notebook is under 20 KB, and that `.ws/local/log/session.log` contains `rolled`. Then `session_start_does_not_roll_on_clear`: the same setup with source `"clear"` leaves no archive.

- [ ] **Step 2: Run them to verify they fail**
  Run: `cargo test --test internal rolls`. Expected: FAIL (no archive).

- [ ] **Step 3: Implement.** When `h.source` is `startup` or `resume`, call `rollover::roll_over(&ws, &actors::actor_slug_in(&ws.root))`. Use `actor_slug_in(&ws.root)`, not `actor_slug()`, because the hook's cwd is not guaranteed. On `Ok(n)` with n > 0, log `notebook rolled: {n} entries moved to .ws/notebook-archive/`. On `Err(e)`, log `notebook roll failed: {e:#}`. Neither changes the hook output.

- [ ] **Step 4: Run the tests to verify they pass.**
  Run: `cargo test --test internal`. Expected: all pass. Then run the full `cargo test`.

- [ ] **Step 5: Docs.** In the README, add one paragraph where the notebook is described: past 32 KB, older entries move to `.ws/notebook-archive/` at session start. In the CHANGELOG under `[Unreleased]`, add `### Added`: `- Notebooks roll over: past 32 KB, the oldest entries move to .ws/notebook-archive/ at session start, so agents stop rereading months of history.`

- [ ] **Step 6: Migrate this workspace's hand-made archive.** Run `mv .ws/notebook-archive/notebook.im-ionutmocanu-gmail-com.2026-08-to-10.md .ws/notebook-archive/notebook.im-ionutmocanu-gmail-com.md`, change its first line to `# Notebook archive (im-ionutmocanu-gmail-com)`, and replace the hand-written pointer line in the live notebook with the exact pointer copy. `.ws/` is untracked, so this isn't committed.

- [ ] **Step 7: Commit**
  `git add src/internal.rs tests/internal.rs README.md CHANGELOG.md && git commit -m "feat(notebook): roll over at session start"`

### Task 3: Remove cs from code, tests, the mod and git config

**Files and the exact change for each:**
- Comments rewritten to state the lesson without naming cs:
  - `src/agentstate.rs:232`, `src/autosave.rs:15` and `:554`, `src/cli.rs:304`, `src/mail.rs:10` and `:421`, `src/rewrite.rs:156`, `src/update.rs:82`, `:165` and `:434`
  - `src/prompts.rs:65` becomes "so /ws:summary never clobbers another plugin's /summary"
  - `src/hooksetup.rs:364` becomes "so a prior statusline is recoverable"
  - `src/contract.rs:41`, `src/workspace.rs:126` and `:254`: drop `migrate-cs`, leaving `-adopt` and worktree creation
  - `tests/launch.rs:628`, `tests/redact_population.rs:5`
- `src/main.rs:375`: drop `"migrate-cs"` from the token list.
- `src/cli.rs:1125-1142`: keep only the `subagent-statusline` half. The doc comment becomes singular. The flag assert becomes `parse(vec!["subagent-statusline".into(), "--all".into()]).is_err()`.
- `src/hooksetup.rs:963-994`, `:1220`: change the foreign hook to `~/.claude/hooks/other-tool/session-start.sh`, the filters to `"/other-tool/"`, and the comment to "a pre-existing foreign hook must survive".
- `tests/setup.rs:122-199`: change the foreign statusline to `/opt/other/other-statusline` and the assertions to `"other-statusline"`.
- `plugins/ws-quiet-notes/hooks/register.tsx:9`: `QUIET = ['/.ws/']`. In `:86`, the description becomes `(.ws/)`. Delete the `.cs/` line in `register.test.tsx:26`.
- `plugins/ws-quiet-notes/.claude-plugin/plugin.json`: drop `, .cs/` from the description and bump the version to `0.1.1`. Make the same description change in `.claude-plugin/marketplace.json`.
- `.gitignore` lines 11, 38–40 and `.gitattributes` lines 1–3: copy them to `.git/info/exclude` and `.git/info/attributes` **first**, so this checkout's `.cs/` stays ignored, then delete them from the tracked files.

- [ ] **Step 1: Make the edits above.**
- [ ] **Step 2: Verify that nothing is left.** Run `git grep -niw cs -- src tests plugins .claude-plugin .gitignore .gitattributes`, which should print nothing. Then `git status --short`, which should not list `.cs/`.
- [ ] **Step 3: Run the checks.** Run `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`, which should all pass. For the mod, run its tests and `tsc` as in the 2026-10-07 setup (the plugin-authoring skill's validate/test). Expected: tests pass, tsc clean.
- [ ] **Step 4: Commit**
  `git add -A src tests plugins .claude-plugin .gitignore .gitattributes && git commit -m "chore: drop every cs reference from code, tests and the quiet-notes mod"`

### Task 4: Remove cs from docs

**Files:**
- Delete `docs/2026-07-27-cs-vs-ws-comparison.md`, `docs/2026-07-27-cs-vs-ws-independent-audit.md`, `docs/2026-07-27-cs-vs-ws-rerun.md` and `docs/superpowers/specs/2026-09-30-cs-parity-roadmap.md`.
- `README.md`:
  - Remove the acknowledgment line 3.
  - Remove the paragraph at 118–124 about the old gap list, keeping its last sentence about mail without the cs comparison.
  - Change line 528 to "a file under `.ws/`".
- `SECURITY.md:81`: remove the pointer to the deleted audit.
- `docs/going-public-checklist.md`, `docs/releasing.md`, `docs/2026-07-26-ws-remaining-work.md`: reword each mention so it stands alone, and drop links to the deleted files.
- Dated historical plans and specs in `docs/superpowers/plans/2026-07-*`, `docs/specs/2026-07-24-*`, `docs/superpowers/plans/2026-09-30-context-rotation.md`, and the CHANGELOG's released entries: **left as written** (see "Call for review" below).

- [ ] **Step 1: Make the edits and deletions.**
- [ ] **Step 2: Verify.** Run `git grep -niw cs -- README.md SECURITY.md docs/releasing.md docs/going-public-checklist.md docs/2026-07-26-ws-remaining-work.md`, which should print nothing. Then `git grep -n "cs-vs-ws\|cs-parity" -- . ':!docs/superpowers/plans/2026-07-*' ':!CHANGELOG.md'`, which should print nothing.
- [ ] **Step 3: Commit**
  `git add -A docs README.md SECURITY.md && git commit -m "docs: drop the cs comparisons and references"`

## Call for review

1. **The README acknowledgment** credits cs for the original idea. Task 4 removes it because you asked for all of it. ws doesn't copy cs code, so no license notice depends on it.
2. **History is left alone:** released CHANGELOG entries and the dated July plans (`migrate-cs` alone accounts for 96 mentions in the phase-6 plan). They record what shipped, and the published GitHub release notes keep the old wording anyway. Say so if you want those rewritten or deleted too.
