# `cs` vs `ws` — Phase 8 checkpoint

**Checked:** 2026-07-27

**`ws`:** `phase8-orchestration` at `46de766`, package `0.1.2`

**`cs`:** installed `hex/claude-sessions` `v2026.7.24`

## Scope

This is an interim correction to the 2026-07-26 audit after Phase 8. It is not
the final hardening Changeset 9 comparison. That final comparison must be run
again after Changesets 1–8, using the same source counts, test baseline, warm
benchmarks, and scoring method.

The exact `cs` baseline is unchanged: the installed command still reports
`2026.7.24`. The `ws` side has advanced from 333 to 366 passing tests and now
includes actors, mail, a persistent queue and unattended drain, tmux spawn, and
git worktree create/merge.

## Corrected verdict

Phase 8 closes several visible feature gaps, but not the trust gap.

- `cs` remains the stronger Claude-only product: it has deeper queue, recovery,
  checkpoint, conversation, live-monitoring, usage, secret, doctor, completion,
  and release workflows.
- `ws` remains the stronger cross-agent foundation: its typed Rust core is
  faster, less globally invasive, and shares durable workspace state between
  Claude Code and Codex.
- `ws` is not yet a trustworthy full replacement for `cs`. Interprocess
  transactions, fail-closed secrets, exact verified sessions, schema-validated
  queue results, identity/mail semantics, quality gates, and authenticated
  releases are still mandatory work.

## Feature comparison

| Area | `ws` after Phase 8 | `cs` v2026.7.24 | Current edge |
|---|---|---|---|
| Named persistent workspaces | Create, resume, adopt, remove, archive, tags, status | Mature equivalent session lifecycle | `cs`, maturity |
| Claude + Codex continuity | Shared `.ws/`, handoffs, notebooks, agent switching | Claude-only | `ws` |
| Worktrees and merge-back | `base@feature`, `--no-ff` merge, conflict rollback, live-worktree refusal | Worktree creation and merge skill/workflow | Near parity at command surface; `cs` remains more mature |
| Queue | Append-only queue, folded state, explicit unattended drain, breaker and iteration cap | Add/list/remove/clear/log, supervised walk-away workflow | `cs` |
| Queue result integrity | Exit status plus agent-specific heuristics; Codex requires a nonempty output file | Richer supervised workflow | Neither is the target `ws` contract; `ws` Changeset 4 remains mandatory |
| Mail | Send, log, session-start display | Typed message kinds, explicit unread read, history, status-line/TUI badge | `cs` |
| tmux spawn | New session/window and optional full-queue drain | Mature spawn and task seeding | `cs` |
| Actors | `-whoami`, `-who` from git identity/history | Equivalent commands | Parity |
| Search, tags, archive, status | Implemented | Implemented | Parity at the command surface |
| Checkpoints | Not implemented | Labelled checkpoint save/list/show | `cs` |
| Conversation lineage | Latest-handoff continuation only | Explicit conversation chain and rotation lineage | `cs` |
| Live monitoring | Lock/live markers in list and TUI | `-live`, PID plus heartbeat awareness | `cs` |
| Usage attribution | Captured global/workspace limit snapshots | Per-session token usage for 5-hour, weekly, and lifetime windows | `cs` |
| Secrets | Keyring or encrypted file, set/get/list/remove/purge/export | Broader backends, encrypted sync/import/export and migration | `cs`; `ws` still has mandatory failure-safety gaps |
| Doctor | Agent, hook, and shim presence | Keychain, hooks, memory, audit, token, spawn, and platform checks | `cs` |
| TUI | Workspace list/detail, filter, tags/status/archive/remove, live and limit indicators | Richer session monitoring and operational views | `cs` |
| Shell completions | Not implemented | Bash and Zsh completions | `cs` |
| Release/platform support | Draft release, Apple Silicon prebuilt, unauthenticated updater bootstrap | Signed/checksummed multi-platform distribution | `cs` |
| CLI speed | Small native Rust binary startup and central-registry listing | Shell/TUI suite with richer filesystem scanning | `ws` |
| Implementation architecture | One typed Rust binary, narrow agent adapters | Shell-heavy multi-executable suite | `ws` |

## What changed since the original audit

The original statement that `cs` wins because `ws` lacks worktrees, mail,
queueing, and tmux spawn is no longer accurate. `ws` now implements each:

- Actors: `ws -whoami`, `ws -who`.
- Mail: `ws -msg`, `ws -msg log`, and session-start surfacing.
- Queue: append-only task records, folded state, drain journal, crash reaping,
  circuit breaker, and a 50-task cap.
- Spawn: tmux session/window creation and task-triggered draining.
- Worktrees: create, merge-back, conflict rollback, and cleanup.

The Phase 8 review also closed several branch-specific failures:

- A conflicting merge is aborted and the base repository is restored.
- A pre-existing user merge is detected before `ws` can abort it.
- Dirty base repositories are refused before merge.
- `MERGE_HEAD` is resolved correctly in linked worktrees.
- Mail is rendered and acknowledged from one scan, eliminating the
  scan-between-display-and-watermark loss window.
- Unattended drains use checked workspace metadata.
- Codex interactive/drain ownership is separated as far as `resume --last`
  permits, and ambiguous legacy markers start fresh.
- `-spawn --task` discloses that it drains the whole pending queue.

## What did not change

The original audit's trust findings still stand unless noted above:

1. Shared read-modify-write state is not protected by interprocess
   transactions; PID lock acquisition still has a TOCTOU race.
2. Secret deletion, purge, storage, rollback, manifest, and redaction paths are
   not yet uniformly fail-closed.
3. Agent session identity is not exact and verified. Codex still depends on
   `resume --last`, and ownership can be recorded before launch success.
4. Queue completion is not backed by a required, schema-validated agent
   disposition.
5. Workspace-name validation, secret namespace identity, mail acknowledgement,
   and doctor coverage remain incomplete.
6. Repository-wide rustfmt is not a passing gate; CI/platform coverage remains
   narrower than `cs`.
7. The updater bootstrap and release assets do not yet meet the authenticated
   release rule.
8. README and the historical design document still need the truthful product
   contract pass.

## Size, tests, and performance

Current `ws` physical-line estimate using the original split method:

- Production Rust under `src/` before each `#[cfg(test)]` module: 7,246 lines.
- Unit-test sections plus `tests/*.rs`: 7,609 lines.
- Test cases: 366, all passing at `46de766`.
- Release binary: 5,279,024 bytes on this Mac.

The unchanged `cs` v2026.7.24 baseline from the original audit:

- Authored production code: approximately 13,610 lines.
- Test code: approximately 25,428 lines and 1,356 tests.
- Generated executables: another 9,679 tracked lines, excluded from authored
  code comparisons.
- Installed executable suite: approximately 1.27 MiB.

The previous warm benchmark results are retained as historical evidence, not
claimed as a fresh run:

| Command | `ws` | `cs` |
|---|---:|---:|
| Version | 2.16 ms | 11.80 ms |
| Help | 2.06 ms | 16.72 ms |
| Empty list | 2.11 ms | 11.79 ms |
| Synthetic 100-workspace list | 5.22 ms | 291.60 ms |
| Representative status line | 15.69 ms | 18.21 ms |

The 100-workspace comparison favors `ws`'s central registry over `cs`'s richer
filesystem scan, but the startup/listing speed advantage is real. Changeset 9
must rerun these measurements rather than copying them into a final verdict.

## Release decision

Phase 8 can be prepared as `v0.2.0` after its branch is merged and the release
steps are completed. That does not satisfy the hardening exit criterion.

Do not describe `ws` as a trustworthy cross-agent beta until all P0 hardening
findings are closed, the full quality gate is green, and release/update
authentication is implemented. Do not describe it as a full `cs` replacement
unless the final Changeset 9 feature comparison supports that claim.
