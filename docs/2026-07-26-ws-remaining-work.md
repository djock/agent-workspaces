# ws — what remains to be implemented

**Written:** 2026-07-26
**Branch:** `phase8-orchestration` (10 commits off `main` @ `d0060cd`)
**HEAD:** `79296b6`
**Suite:** 362 passing, 0 failing, 0 warnings in both `cargo build` and `cargo test --no-run`

> Cargo is not on PATH. Every cargo command in this document assumes the prefix
> `. "$HOME/.cargo/env";`. The crate is **bin-only** — `cargo test --lib` does
> nothing; use `cargo test --bin ws <filter>`.

---

## 1. Current state

### 1.1 Committed and reviewed — Phase 8, all six tasks

| Task | Feature | Commit |
|---|---|---|
| 1 | Actors — `ws -whoami`, `ws -who` | `1fec77d`, `2713ea9` |
| 2 | Mail — `ws -msg`, `ws -msg log`, session-start surfacing | `415792b` |
| 3 | Queue store — append-only `tasks.jsonl`, folded state | `e878bd5` |
| 4 | Headless drain — circuit breaker, crash reaping, iteration cap | `fe09de2`, `5605370` |
| 5 | Spawn — tmux window/session | `80a3c3c` |
| 6 | Worktrees — `ws base@feature`, `--merge` | `79296b6` |

Each task passed a per-task spec + quality review. The ledger is
`.superpowers/sdd/2026-07-26-ws-phase8-orchestration/progress.md`.

### 1.2 Uncommitted — the final-review fix wave

The final whole-branch review (opus) found **1 Critical + 6 Important**, written
up in `.superpowers/sdd/2026-07-26-ws-phase8-orchestration/final-review.md`. All
of them are **already fixed in the working tree but not committed and not
reviewed**: ~640 insertions across 11 files.

| ID | Finding | Fix present in tree | Covering test |
|---|---|---|---|
| **C1** | A conflicting `--merge` left the user's base repo mid-merge (MERGE_HEAD set, conflict markers in their source files, staged adds) and reported `failed: ` with an empty reason | `combined()` captures stdout+stderr; `merge --abort` on failure; `mid_merge()` resolves via `rev-parse --git-path`, not by joining `.git/` | `a_conflicting_merge_leaves_the_base_repo_clean_and_says_why` |
| **I2** | `ws base@feature --merge` could not complete on a worktree ws itself created — ws's own untracked bookkeeping blocked it | `WS_BOOKKEEPING` + `user_dirt()` exclude `.ws/base` and `.ws/timeline.jsonl` from the dirty check | `only_ws_own_untracked_bookkeeping_is_ignored_by_the_dirty_check`, `create_then_merge_round_trips_with_no_manual_git_steps` |
| **I3** | Lost mail: `session_start` rendered from one `mail::unread` scan and marked seen from a second, independent one | single scan; `build_context(ws, mail)` takes the scan; watermark comes from what was actually rendered | `many_unread_messages_produce_one_watermark_not_one_per_message` and neighbours |
| **I4** | The unattended drain resolved its agent through the lenient `meta::read`, so a corrupt `workspace.toml` silently ran the config default | `meta::read_checked` (`src/drain.rs:158`) | in `src/drain.rs` tests |
| **I5** | The codex drain wrote the *shared* `launched` marker, so the user's next interactive `ws proj` resumed the drain's headless session | `OWNER_DRAIN` / `OWNER_INTERACTIVE` `last_owner` key; `launched` kept in step for older `ws` versions | in `src/agents/codex.rs` tests |
| **I6** | `ws -spawn --task "one thing"` silently ran a full drain — up to 50 unattended agent runs | `pending_summary()` + `drain_scope_notice()` print the real scope before spawning | in `src/spawn.rs` tests |
| **I7** | No test exercised `create → merge`; all fixtures were hand-built in a state `create` never produces | the round-trip test above | `create_then_merge_round_trips_with_no_manual_git_steps` |

Also in the tree, from the review's non-blocking list:

- `queue/*.jsonl merge=union` added to `.ws/.gitattributes` (`src/contract.rs:66`) — without it, a base and its worktree both appending to the queue conflict on merge, feeding straight into C1.
- **M1** documented in place (`src/cli.rs:228-233`): any name containing an inner `@` routes to the worktree arm, so an adopted workspace literally named `client@acme` is unreachable from the CLI. It fails safe — `create` bails "already exists" via `lookup_checked` — but it is a real limitation.

### 1.3 Uncommitted — unrelated to Phase 8

| Path | What it is | Decision needed |
|---|---|---|
| `Cargo.toml`, `Cargo.lock` | adds `fs2 = "0.4.3"` | **`fs2` is not referenced anywhere in `src/`.** It is prep for hardening Changeset 1. Either drop it from the Phase 8 commit or accept shipping an unused dependency in v0.2.0. |
| `.gitignore` | adds `.cs/local/`, `.cs/archives/`, `.cs/.narrative-reminder-cooldown` | session-tooling noise, unrelated to Phase 8 — commit separately |
| `AGENTS.md` (untracked) | from a codex launch | decide whether it belongs in the repo |
| `docs/superpowers/plans/2026-07-26-ws-mandatory-hardening.md` (untracked) | a separate 9-changeset plan (§3) | commit as a plan doc |

---

## 2. To ship Phase 8 as v0.2.0

Ordered. Steps 1–2 are the only ones with real risk.

### Step 1 — Review the uncommitted fix wave ⚠️ **not yet done**

640 lines across 11 files, touching the Critical path that writes into the
user's git repositories, currently **unreviewed**. Everything else on this
branch went through a per-task review plus a whole-branch review; this wave has
had neither.

What the review must confirm, at minimum:

- `merge --abort` genuinely restores the base repo — the test asserts it, but
  verify the assertion discriminates (break the abort, watch it fail).
- `mid_merge()` via `rev-parse --git-path` is correct for a *linked worktree*,
  where `.git` is a file holding a `gitdir:` pointer, not a directory. The code
  comment says conflating these produced a Critical in Phase 6.
- The I5 codex change writes `launched` **and** `last_owner`. Confirm an older
  `ws` binary reading that `state.toml` still behaves, and that a drain can
  never resume an interactive session or vice versa.
- I3's single-scan restructure still performs **at most one `atomic_write` per
  session start** regardless of message count (`atomic_write` fsyncs twice,
  ~3.4 ms, and this is a hook path).
- `user_dirt()` excludes *only* ws's own bookkeeping. A user file named
  `.ws/base` — or any path that merely starts with those strings — must still
  count as dirty.

### Step 2 — Commit the fix wave

Explicit paths only, never `git add -A`. Split the Phase 8 fixes from the
unrelated `.gitignore` / `Cargo.toml` changes.

### Step 3 — Resolve the `fs2` question

Decide before tagging. An unused dependency in a release is avoidable.

### Step 4 — Merge to main

```
git checkout main && git merge --no-ff phase8-orchestration
```

Re-run the full suite on the merged result before proceeding — the merge itself
can break things nothing on either side caught.

### Step 5 — Version bump and CHANGELOG

Bump `Cargo.toml` `0.1.2` → `0.2.0` and add a CHANGELOG entry. **The release
workflow verifies the tag matches the Cargo version**, so these must agree.

CHANGELOG should mention the M1 limitation (workspaces with `@` in the name).

### Step 6 — Tag and push

```
git tag v0.2.0 && git push origin main && git push origin v0.2.0
```

Tag-driven release workflow fires on `v*`.

> **Note:** this supersedes the earlier convention that releases are cut
> separately from phase merges. That was the standing rule until this session.

---

## 3. The separate hardening plan — **not started**

`docs/superpowers/plans/2026-07-26-ws-mandatory-hardening.md`, from a `cs` vs
`ws` audit. Nine changesets. Only artefact so far is the unused `fs2`
dependency. This is **larger than Phase 8 was** and is independent of shipping
v0.2.0.

Its eight safety rules are worth reading before any of it starts; rule 1 in
particular reframes work already shipped:

> *An atomic rename is not a transaction. Every shared read-modify-write must
> hold an interprocess lock from the first read through the durable rename.*

| # | Changeset | Notes |
|---|---|---|
| 1 | Interprocess transactions | `fs2` advisory locks, `transaction(path, f)` helper; race-free launch acquisition with `create_new` for the PID record; locks around registry, config, meta, state, queue/timeline appends, mail. **Closes the long-deferred `lock::acquire` TOCTOU** (`exists()`-then-write, not `O_EXCL`). Touches 15 files. |
| 2 | Transactional secrets, fail-closed redaction | Rule 3: no credential delete, purge, store, redaction, rollback or manifest error may be discarded. Relevant to **M7** below. |
| 3 | Exact verified agent sessions | Rule 4: a session id becomes durable only after the agent proves the session started. Phase 8's drain writes the id *before* the run. |
| 4 | Schema-validated queue results | Rule 6: a task is complete only when the CLI exits successfully **and** the agent returns a schema-validated `completed` disposition. Strictly stronger than what Phase 8 shipped (exit code + `is_error` for claude, output-file non-emptiness for codex). |
| 5 | Identity, mail, doctor | Overlaps Phase 8's actors and mail. |
| 6 | Concurrency and quality gates | Repository-wide `rustfmt` currently **fails**; Clippy is clean. |
| 7 | Authenticated multi-platform releases | Prebuilt release is Apple-Silicon-only; rule 8 wants assets and the updater bootstrap authenticated, not merely TLS. |
| 8 | Truthful product contract | Rule 7: README and the design doc must describe shipped behaviour, not planned behaviour. |
| 9 | Repeat the comparison | Re-run the audit with the same measurements. |

Its stated baseline says "333 passing tests" and package `0.1.2` — written
before Tasks 5–6 and the fix wave landed, so its numbers are stale.

---

## 4. Deferred, with rulings

From the final review's triage. None block v0.2.0.

| ID | Item | Ruling |
|---|---|---|
| **M2** | `spawn` builds an unquoted shell command string. An install path with a space breaks it; a workspace name containing `;` executes. Self-inflicted (user chose the name) | fix cheaply with `shell-escape` or by passing argv to tmux |
| **M3** | `worktree::create` leaves an orphan on partial failure — branch and worktree survive unregistered, and every retry bails "already exists". Manual `git worktree remove` required | worth a cleanup path |
| **M4** | `ws -queue add` does not check the agent is installed; it prints "run `ws -queue drain`" which then bails. The *drain* side is correct — the install check precedes `lock::acquire`, and a binary that vanishes mid-run is journalled as a failure | only the optimistic `add` message is off |
| **M5** | `mail::new_id`'s `unwrap_or(0)` — a pre-epoch clock yields an id that sorts below everything and is instantly "already read" | vanishing likelihood; listed as a member of the read-error-to-default family |
| **M6** | `merge` never verifies the base is the worktree's parent repo, nor that it still has the forked-from branch checked out. Switch `api` to another branch after `ws api@retry`, and `--merge` silently commits into that other branch | recording the base's HEAD in `.ws/base` (which stores only the name and is read by nothing) would close it |
| **M7** | **Pre-existing, out of Phase 8 scope.** `src/internal.rs` `note_manifest` maps a JSON *parse* error to `json!({})` and `atomic_write`s it back, discarding every recorded `redacted_secrets` entry — exactly what the comment three lines above says it refuses to do | live instance of the read-error-clobber family; belongs to hardening Changeset 2 |
| **M8** | mail ids are zero-padded epoch-ms + random UUID, so same-millisecond sends order by UUID only | I3's fix makes this unreachable on the display path; leave the id scheme |
| — | `send_then_all_round_trips_in_order` is theoretically flaky by the same mechanism | test-only |
| — | A mid-drain spawn failure burns two never-attempted tasks as `Failed` | needs an exec-closure contract change; fails safe |
| — | `tmux attach` never verified live (no TTY in the sandbox); `new-session`/`new-window` were | disclosed, low risk |
| — | `.ws/base` landing in the *base* workspace on merge, declaring it a worktree of itself | inert today — nothing reads `.ws/base` — but a landmine for whoever wires it up |

---

## 5. Open decisions

1. **Does the fix wave get an independent review before merge?** It is the only
   part of this branch that has had none, and it contains the Critical fix.
2. **`fs2`** — drop from the Phase 8 commit, or ship an unused dependency.
3. **`AGENTS.md`** — in the repo or ignored.
4. **M1 in the CHANGELOG** — `@` in a workspace name now means "worktree spec".
   Anyone with such a workspace loses CLI access to it after upgrading.

---

## 6. Verification commands

```bash
. "$HOME/.cargo/env"

cargo test                      # expect 362 passing, 0 failing
touch src/main.rs && cargo build         # expect zero warnings
touch src/main.rs && cargo test --no-run # expect zero warnings

grep -rn "fs::rename" src/      # must be exactly one hit, in src/atomic.rs
grep -rn "dangerously\|bypassPermissions\|permission-mode" src/
                                # must hit only #[cfg(test)] forbidden-flag arrays
```

The last two are standing invariants: every shared-file write goes through
`atomic::atomic_write` (append-only JSONL logs via `OpenOptions::append` are the
sanctioned exception), and ws never passes a permission-escalation flag to an
unattended agent.
