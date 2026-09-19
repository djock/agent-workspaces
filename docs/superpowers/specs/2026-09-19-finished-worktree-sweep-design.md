# Finished-worktree sweep — design

Date: 2026-09-19. Status: draft for review. Path: architectural.

## Problem

The user runs several feature worktrees (`base@feature`) beside the main
project. When they finish a task on main they type `/clear` — the agent process
stays alive. Nothing then tells them that sibling worktrees have finished too, so
finished work sits unmerged until they remember it.

## Goal

When work on main is finished, ws finds the sibling worktrees that have marked
themselves done, and offers each for review and merge — without interrupting
work in progress and without spending model tokens on anything ws can compute.

## Non-goals

In-session agent review of each diff; a combined review branch; adopting plain
`git worktree` checkouts; any prompt delivered through the Stop hook's
`{"decision":"block"}` channel (it forces a model turn, and is the mechanism
behind the earlier long-run interruption problem).

## What already exists

- `worktree::readiness` — one function behind `-features` and `--merge`; returns
  `ahead`, `already_merged`, and `blockers` (`Live`, `FeatureDirty`, `BaseDirty`,
  `BaseMidMerge`, `Unreadable`).
- `worktree::features(base)` — every `base@*` workspace with its readiness.
- `worktree::merge` — `--no-ff`, removes the worktree, leaves the base clean on
  conflict.
- `internal::session_start` — a `SessionStart` hook that already fires on
  `/clear` and injects context via `build_context`.
- The statusline.

The only missing pieces are a *done signal* and a *trigger*.

## Design

### 1. The marker

`<worktree>/.ws/done`, written atomically, holding `head` (commit sha), `at`
(timestamp) and `by` (`prompt` | `manual`).

A marker is **fresh** only when `head` equals the worktree's current `HEAD` and
the tree has no user dirt (the existing `user_dirt` rule). A later commit or edit
makes it stale by itself; nothing needs to delete it. A separate per-sha
"declined" record (`.ws/done-declined`, same shape) stops the question being
asked twice for one `HEAD`.

Both files are ws bookkeeping and must be ignored by `user_dirt`, like the
existing `.ws/` untracked files, or writing a marker would dirty the tree it
certifies.

### 2. Setting the marker (worktree side)

- On `SessionStart` with `source == "clear"` inside a `base@feature` workspace:
  if `HEAD` has commits ahead of the base, there is no fresh marker, and no
  declined record for this sha, `build_context` appends one line asking the agent
  to put "Mark this feature done?" to the user. Yes runs `ws -done`; No runs
  `ws -done --declined`.
- Manual: `ws <base>@<feature> -done`, and `-done --undo`.
- Otherwise the hook adds nothing, and costs nothing.

### 3. Finishing main

- On `SessionStart` with `source == "clear"` in the base workspace: if any
  `base@*` marker is fresh, `build_context` appends "N worktrees are marked done:
  … Review them now?". Yes runs `ws <base> -done`. None fresh: no addition.
- Manual: `ws <base> -done` forces the sweep at any time.
- No tty and no agent (script, CI): print one line and exit 0. Never wait.

### 4. The sweep

`done::classify(base)` reads `worktree::features(base)` plus each marker and
returns one of:

| Class | Meaning | Shown as |
|---|---|---|
| Ready | fresh marker, `readiness().ready()` | offered |
| Done, blocked | fresh marker, a blocker exists | listed with the first blocker |
| Not done | no fresh marker | one line, not offered |

For each Ready worktree the sweep shows commits and diffstat, then asks
`merge` / `skip` / `open`. `merge` calls the existing `worktree::merge`
unchanged. One worktree at a time, so a conflict in one never stops the rest.

### 5. Live-session blockers (found while reading `readiness()`)

`readiness()` reports `Live` for a held session lock on either side. Two
consequences:

- **The base is always live during the sweep** — the sweep runs from inside the
  base's own session. The base lock must not block a sweep started by its own
  holder. The sweep therefore treats a base lock whose pid is an ancestor of the
  current process as the caller. *Open point:* confirm during implementation what
  pid `lock.rs` records (the `ws` launcher or the agent) and that ancestry can be
  established portably on macOS and Linux; if it cannot, fall back to an explicit
  `--from-session` flag set by the hook-issued command.
- **A `/clear`ed worktree stays live** until its agent is closed. That blocker
  is correct — removing a directory under a running agent is the harm it exists
  to prevent — so it stays. Such a worktree is classed Done-blocked and shown as
  "done, but still open in pid N — close it, then merge". It becomes Ready
  without re-marking once closed, since the marker is keyed on `HEAD`.

### 6. Statusline chip

The statusline shows `N ready` when the workspace has fresh, merge-ready
markers. Read-only, no tokens, visible at the moment the user would `/clear`.

### 7. Structure

- New `src/done.rs`: marker read/write, freshness, declined record, `classify`.
  No git code of its own — it calls `worktree`.
- `cli.rs`: the `-done` verb (`--undo`, `--declined`, `--porcelain`).
- `internal.rs`: the two `build_context` additions.
- `statusline.rs`: the chip.
- `worktree.rs`: expose `user_dirt` and the base-lock handling; no behaviour
  change to `merge` or `-features`.
- `-features` is unchanged; `-done` is the marker-aware view of the same data.

## Cost

Reading markers, readiness and diffstats are plain ws code: no tokens. The two
hook additions add one short line only when there is something to act on; the
user's answer costs one ordinary agent turn.

A spike to run during implementation: whether `SessionEnd` with `reason:
"clear"` can show a user-only message. If it can, it replaces the injected line
with a zero-token prompt.

## Error handling

- Marker file missing, unparseable or from a different `head`: treated as no
  marker. Never an error, never a crash of the hook.
- Hook failures must not fail the session: `session_start` already returns
  quietly on error; the additions keep that.
- A worktree whose directory is gone stays `Blocker::Unreadable`, as today.
- Non-interactive: never block on input.

## Testing

- Unit: freshness (head moved, dirty tree, unparseable marker); marker files not
  counted as user dirt; declined-once-per-sha; classification of all three
  classes; the base-lock ancestry rule.
- Integration: two real worktrees, one marked and one not — the sweep offers
  only the marked one and the merge lands on main; a marked worktree whose
  session lock is live is listed blocked, not offered.
- A real vault-style check per project memory: the marker must survive a second
  process (write in one, read in another) — the suite has gone green before over
  state that persisted nothing.
- Pty run of the real binary before tagging (project memory: picker unit tests
  never touch the key mapping).
- Release notes follow `docs/releasing.md`; a new verb needs the README and
  help-text token test to stay green.
