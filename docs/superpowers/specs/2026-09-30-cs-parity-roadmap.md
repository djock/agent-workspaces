# cs parity roadmap

**Status:** draft for user review
**Date:** 2026-09-30
**Source:** cs CHANGELOG v2026.7.25 → v2026.9.23 (44 releases since our
2026-07-27 baseline of v2026.7.24), checked against `ws` at `84c18ed`.

## Already in ws (dropped from scope)

Checked in the code, not assumed:

| cs feature | ws equivalent |
|---|---|
| busy / waiting / idle per session | `src/agentstate.rs`, shown in the picker (`rows.rs:35`) |
| mail threads, `--reply` | `ws -msg --reply <thread>` (`cli.rs:794`) |
| queue a task mid-turn (`/queue`) | `/ws:task` → `ws -task add`, Stop hook asks once per change |
| unread mail surfaced to the agent | `mail::digest` on every prompt (`internal.rs:160`) |
| launch update notice + headlines | `update::notify` (v0.5.0), restyled 2026-09-30 |
| picker archive `a` | v0.6.0 |
| handoff skeleton | `ws -rotate` (`commands.rs:1580`), `ws <name> --handoff` |

## Gaps, in build order

Each phase ships on its own, with its own plan and release.

### Phase 1: context-aware rotation (plan: `plans/2026-09-30-context-rotation.md`)

1. The status line records the conversation's context % to `.ws/local/context.json`, keyed by session id.
2. The Stop hook nudges once per conversation when context passes `rotate_nudge` (default 65, 0 = off). The nudge says: finish the step, write the handoff, tell the user to `/clear`.
3. `ws -rotate` **arms** the handoff it writes (`.ws/local/pending-handoff`). The next SessionStart with source `startup` or `clear` consumes it and points the fresh conversation at the handoff. This is how rotation stays inside the agent: `/ws:rotate`, then `/clear`, with no relaunch.
4. The handoff template and `/ws:rotate` prompt take cs's quality rules: next step first and self-contained, conversation-only facts, a measured-or-assumed label on each claim, rejected alternatives, and a successor report.

Codex gets items 3 and 4. Item 2 is Claude-only, because Codex has no status line and the context size in its rollout is not verified yet. That is a follow-up, not a blocker.

### Phase 2: notebook rotation

- `ws -notebook rotate [<name>]`: when `notebook.<actor>.md` is over `notebook_max_kib` (default 512), move the oldest `## ` sections verbatim into `.ws/notebook-archive/<actor>/<sha8>.md` and keep the newest 256 KiB tail byte-identical. Byte-identical keeps it merge-safe with `merge=union`.
- The Stop hook's notebook reminder says when the notebook is over budget. `ws doctor` warns about it too.
- `ws -search` covers the archive.
- The notebook protocol text becomes "append-only; corrections are new dated notes".

### Phase 3: idle mail wake (Claude)

- SessionStart returns `watchPaths: [<ws>/.ws/mail/new]`, and a `FileChanged` hook turns a new message into a wake: a system reminder naming the sender. The hook ignores `task`-kind mail, and a cap (`mail_wake_max`, default 5 between user prompts) stops two sessions from bouncing messages forever.
- A `CwdChanged` hook re-returns the watch path. Claude Code **replaces** the watch list on a cwd change; cs lost its wake to this (cs 8.15).
- Codex: nothing to watch. It keeps the per-prompt digest.
- Spike first: confirm that the current Claude Code still honours `watchPaths` from SessionStart and CwdChanged.

### Phase 4: `pre-open` hook

- An executable `.ws/local/pre-open` runs from the workspace root before `ws <name>` opens it (before locks and the agent exec). If it exits non-zero, the open is aborted with its output shown. Use case: mounting an encrypted volume.
- If `.ws/memory` or `.ws/notebook` is a dangling symlink, the error names the link and its target, instead of failing with a bare `mkdir` error.

### Phase 5: release notes inside the session

- When `update::cached()` says a newer ws exists, the SessionStart context gains one line: `ws <latest> is available (you have <current>); /ws:whatsnew lists what changed.`
- `/ws:whatsnew` (both agents) runs `ws -update --notes`: every headline newer than the installed version, with no install. It reuses `update::summarize` with no cap.
- This finishes the workspace objective: the notice is visible inside the conversation, not only at launch.

### Phase 6 (optional): hooks resolve from the directory

- If `WS_WORKSPACE` is unset, hooks walk up from the payload `cwd` to the nearest `.ws/workspace.toml`, stopping at `$HOME`. They skip the walk for a plain terminal `claude` (entrypoint `cli`), because a session is entered by running `ws`. They keep it for desktop and IDE front ends.
- `.ws/local/disabled` opts a directory out.
- Only a `ws` launch owns the lock. A walked-in SessionEnd must not remove it.
- Lowest priority: it only helps if you use Claude Code desktop or an IDE on workspace directories.

## Not planned (and why)

- **Forced rotation with countdown and a Handoff pane.** cs builds this from Claude Code function-hook "mods", which are Claude-only TypeScript inside Claude Code. The Phase 1 nudge covers the need for both agents.
- **Capsule status line and Fable usage chip.** The ws status line is already its own design, and the Fable chip calls a private OAuth endpoint.
- **ctrl+g multi-provider rewriter.** ws has `rewrite` already. Adding providers is separate work.
- **Windows removal.** ws never supported it.
