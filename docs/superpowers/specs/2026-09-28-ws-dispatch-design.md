# `ws -dispatch` — design

Date: 2026-09-28. Status: draft for review. Path: architectural.

## Problem

The user collects tasks for several projects in one piece of text. Today a ws
session only knows its own workspace, so working that list means opening each
workspace in turn and pasting the relevant slice into each one by hand.

## Goal

One command takes the whole list, checks that every project it names is a real
workspace, and starts a single one-time agent session that works the projects in
the order written, leaving each one's changes uncommitted and noted.

## Non-goals

- Running a separate agent per project (headless `claude -p` / `codex exec`).
  The refocus removed unattended agent runs; the dispatcher does the work itself.
- Parallel work across projects.
- Pasting the list into a running agent. Directories cannot be granted to an
  agent after launch, and names must be checked before anything starts, so the
  text is read by ws first.
- Committing, pushing, or switching branches in any target workspace.
- Matching projects from plain prose ("fix the api retry"). Only `@name` counts.

## What already exists

- `registry::lookup` / `registry::all` — name → path for every workspace.
- `lock::live_pid_checked` / `lock::acquire` — the "another session holds this
  workspace" check and the lock `ws <name>` takes.
- `Agent::mode_args` — `-loco` / `-sane` translated per agent.
- `timeline::record` — per-workspace event log read by `ws -who` and the picker.
- `WS_CLAUDE_BIN` / `WS_CODEX_BIN` — binary overrides, used for stub agents in tests.
- Both agents take `--add-dir` (checked: Claude Code, Codex CLI 0.156.1).

Not existing yet: a "did you mean" suggestion for workspace names (see Checks).

## Design

### Command

```
ws -dispatch [<tasks.md>] [-claude|-codex] [-loco|-sane] [--force]
```

- No file: ws opens `$VISUAL`, else `$EDITOR`, else `vi`, on a template in the
  scratch directory. Saving it empty, or unchanged from the template, cancels
  with exit 0 and launches nothing.
- With a file: ws reads it and opens no editor.
- Agent: `-claude` / `-codex`, else the configured `default_agent`.
- Posture: `-loco` / `-sane` for this run only. Not remembered — there is no
  workspace to remember it in. Absent, the agent's own default applies.
- `--force`: take over workspaces another live session holds, as `ws <name>
  --force` does.

### Input format

```
Anything here is outside every section.

@api
- retry 429s with backoff
- drop the v1 client

@web@redesign
- fix the header on mobile
```

- A line whose first non-space character is `@` starts a section. The name is
  the text after `@` up to the first whitespace; the rest of that line, if any,
  is the section's first task line. `base@feature` worktree names work
  (`@web@redesign`).
- `@` anywhere else on a line (an email address, a mention mid-sentence) is
  ordinary text.
- Sections are worked in the order written.
- Every non-blank line in a section is one task, with a leading `- `, `* `,
  `- [ ] ` or `N. ` stripped. Blank lines are dropped.
- A section runs until the next `@name` line or the end, so the only text
  outside every section is what comes before the first one. That text is
  **unassigned**: carried into the plan verbatim and never acted on during the
  run (see Protocol, rule 6).

### Checks (before anything launches)

All of these refuse the whole run. Nothing launches and no lock is taken.

| Condition | Message |
|---|---|
| No sections at all | `no @name sections found` |
| Unknown name | `no workspace 'apii' — did you mean 'api'?` (suggestion only when one name is within edit distance 2; otherwise no suggestion) |
| Name appears twice | `'api' appears twice (lines 4 and 19); merge the sections` |
| Section with no tasks | `'@web' has no tasks` |
| Invalid name | `workspace::validate_name`'s own message |
| Archived workspace | `'api' is archived; ws -unarchive api first` |
| Held by a live session, no `--force` | `'api' is open in another session (pid 4312); close it or pass --force` |

Every error for every section is reported in one go, not just the first. When
the text came from the editor, the message ends with the path of the saved text
so the user can fix it and rerun `ws -dispatch <that file>`.

### Launch

1. Scratch directory: `<cache>/ws/dispatch/<YYYY-MM-DDTHH-MM-SS>/`, where
   `<cache>` is the same cache root the update check uses. Not registered, so it
   never appears in `ws -list`, the picker, or `ws -search`.
2. ws writes the plan to `tasks.md` there (format below).
3. ws takes each target workspace's lock (`lock::acquire`, honouring `--force`)
   and holds all of them until the agent exits, so opening one of those
   workspaces elsewhere during the run reports it as busy.
4. Each target's timeline gets a `dispatched` row, payload
   `{"dispatch": "<scratch path>", "tasks": N}`.
5. The agent starts with its working directory set to the scratch directory,
   each target path granted with `--add-dir`, `mode_args` for the posture, and
   the first prompt `Work through tasks.md in this directory.` The user types
   nothing.
6. `WS_WORKSPACE` is not set, so ws's own session hooks treat the run as a
   non-workspace launch and add nothing.

This needs a new `Agent` method, since `Agent::launch` is built around one
`Workspace` and its resume state:

```rust
fn dispatch(&self, scratch: &Path, dirs: &[PathBuf], prompt: &str,
            mode: Option<LaunchMode>) -> anyhow::Result<Command>;
```

It is always a fresh session and records no session id.

### `tasks.md` (generated)

```
# Dispatch 2026-09-28 14:02

## Protocol

(rules below)

## 1. api — /Users/ionut/Projects/api
- [ ] retry 429s with backoff
- [ ] drop the v1 client

## 2. web@redesign — /Users/ionut/.agent-workspaces/web@redesign
- [ ] fix the header on mobile

## Unassigned (do not act on; ask the user at the end)

Anything here is outside every section.
```

The Unassigned section is omitted when there is no unassigned text.

### Protocol (the rules written into `tasks.md`)

1. Work the projects in the order listed. Before starting one, read its
   `.ws/README.md`, `.ws/conventions.md` if present, its `.ws/notebook/` files,
   and its root `CLAUDE.md` / `AGENTS.md` if present, and follow them. They do
   not load on their own because you were started elsewhere.
2. Change files only inside the current project's directory. Do not commit, push,
   or switch branches.
3. Tick `[x]` in this file as each task is finished.
4. If a task is blocked or needs the user's decision, write the question under
   that task in this file, leave it unticked, and continue with the next one.
   Do not stop to ask.
5. Before moving on, append an entry to your notebook in that workspace
   (`.ws/notebook/notebook.<actor>.md`, `ws -whoami` for the actor): what was
   done, what was not, and why.
6. When every project is done, print the report, then the collected questions,
   then show the Unassigned text and ask the user what to do with it.

Report format:

```
api  (2/2 done)
  ✓ retry 429s with backoff — src/client.rs
  ✓ drop the v1 client
  changed: 3 files (uncommitted)
web@redesign  (0/1)
  ? fix the header on mobile — which breakpoint, 640 or 768?
```

### Rerunning a dispatch

`ws -dispatch <scratch>/tasks.md` recognises a generated plan by its first line
(`# Dispatch `) and its numbered `## N. name — path` headings. It re-runs the
checks on those names and relaunches with only the projects that still have
unticked tasks, reusing the same scratch directory. A plan with nothing
unticked prints `nothing left to do` and exits 0.

### Help and docs

- `ws --help` gains the `-dispatch` line (the parser-vs-help test enforces this).
- README: a short "Dispatching tasks to several workspaces" section.

## Error handling

- Editor exits non-zero: cancel, keep the saved text, print its path.
- `--add-dir` rejected by the installed agent (older version): the agent's own
  error reaches the terminal; ws does not retry without it, since the run would
  then be unable to edit anything.
- Lock acquisition fails partway: release the locks already taken, launch
  nothing.
- Agent exits (normally or not): locks are released by the guard's drop; the
  scratch directory stays so a rerun is possible. Scratch directories older than
  14 days are swept on the next `-dispatch`, matching the snapshot sweep.

## Testing

- Parser unit tests: section splitting, `@` mid-line ignored, worktree names,
  task-prefix stripping, unassigned text before the first section, empty
  input, template-only input.
- Check tests: each row of the Checks table, and that all errors are reported
  together.
- Plan rendering: golden test of `tasks.md` with and without Unassigned.
- Rerun: generated plan detected; ticked projects dropped; all-ticked exits 0.
- `Agent::dispatch` for both agents: args contain every `--add-dir`, the mode
  args, the prompt, and no resume/session-id flags; `WS_WORKSPACE` absent.
- Locks: targets are locked during the run and released after; partial failure
  releases what it took. Uses real lock files in a temp dir, two processes, as
  the vault tests do.
- Manual, before tagging: run the real binary under a pty with a stub editor and
  a stub agent binary (`WS_CLAUDE_BIN`), check the launch argv and that the
  editor-cancel path launches nothing. Then one real run against two throwaway
  workspaces.

## Open questions

- Codex with no posture flag: its default sandbox decides whether `--add-dir`
  directories are writable. Verify with a real run during implementation; if
  they are read-only by default, `-dispatch -codex` without `-loco`/`-sane`
  applies `-sane` and says so, rather than launching an agent that cannot edit.
