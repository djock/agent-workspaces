# Startup context trim: automatic notebook rollover

## Why

Every conversation in a workspace pays for what the protocol tells the agent to
read at start: the README and every notebook in `.ws/notebook/`. Nothing caps a
notebook, so the cost only grows. In the `ws` workspace it reached 96 KB, about
24k tokens per conversation, before it was trimmed by hand on 2026-11-24. ws
should keep notebooks small by itself.

## Goals

- A notebook never grows past a size limit. Old entries move to an archive that
  the start protocol doesn't read.

## Non-goals and anti-goals

- ws stays independent: nothing in this change knows about any other session
  tool or edits text that ws didn't write.
- No change to the protocol wording or to `.ws/README.md` / `summary.md`.
- Never lose a notebook entry. The archive is append-only, and an entry moves as
  a whole or not at all.

## Approaches considered

- **Roll over at session start (picked).** Automatic and invisible, and it runs
  before the agent writes anything.
- An explicit `ws -trim` command. Rejected: the user wanted this automatic.
- Change the protocol to read only the notebook's tail. Rejected: it depends on
  every agent following a subtler instruction, while a smaller file works for
  any agent.

## Design

- **When:** in the SessionStart hook, on `startup` and `resume` only. It rolls
  only the **current actor's** notebook, because that agent hasn't started
  writing yet; a teammate's file could be mid-append.
- **Trigger:** the file is over 32 KB. It then keeps the newest whole `## `
  entries that fit in 16 KB, so the roll doesn't fire again on the next launch.
  The newest entry always stays, even when it alone is over 16 KB.
- **Archive:** older entries are appended, oldest first, to
  `.ws/notebook-archive/notebook.<actor>.md`. That's one file per actor, kept in
  chronological order. The folder is outside `.ws/notebook/`, so
  `build_context` never lists it.
- **Pointer:** a single line under the title, added once:
  `Older entries live in .ws/notebook-archive/notebook.<actor>.md. Read them only when you need that history.`
- **Safety:** runs inside `txn::transaction` on the notebook path. The archive
  append happens before the notebook rewrite (crash ⇒ duplicate, never loss).
  The rewrite uses `atomic_write`. Text before the first `## ` heading (the
  title and the pointer) always stays.
- Logged to the session log. Failures are logged and ignored: the hook must
  never block a launch.

## Left to the builder

- The module (likely a new `src/rollover.rs`).
- Tests. Unit tests cover entry splitting, the keep-newest-in-budget rule, the
  oversized single entry, pointer idempotence and the archive append order.
  One integration test drives the real `ws internal session-start` with `WS_*`
  cleared.
- CHANGELOG and README text.
