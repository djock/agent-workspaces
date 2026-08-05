# Picker actions: open, info, delete, archive

**Date:** 2026-08-05 · **Status:** approved, ready to implement

## Problem

`ws` with no arguments opens the console picker (`src/picker.rs`). It can move,
filter, open, and toggle two things. Everything else — deleting a workspace,
archiving one, seeing what a workspace actually is — means quitting the picker
and running a command with a name you have to remember.

Three specific complaints:

1. **No delete.** `ws -rm <name>` exists, but the picker is where you are when
   you notice a workspace you no longer want.
2. **The detail view (`d`) does not look good.** It dumps four indented lines
   under the selected row: `objective:`, `open tasks:`, a notebook tail, and
   timeline entries. No alignment, no framing, no room for anything else.
3. **`a` "does not work".** It does fire — it toggles `show_archived` and
   reloads — but with no archived workspaces the list is byte-identical either
   way and nothing says so. A key whose only observable effect is "nothing
   changed" is indistinguishable from a dead key.

## Keys

| key | action | previously |
|---|---|---|
| `enter` | open the selection | open |
| `i` | info page for the selection | — |
| `d` | delete the selection, with confirmation | detail toggle |
| `a` | archive / unarchive the selection | show-archived filter |
| `A` | show / hide archived workspaces | — |
| `/` `j` `k` `↑` `↓` `q` `esc` | unchanged | unchanged |

`d`, `i` and `a` all act on the selection, like `enter`. The filter that used to
own `a` moves to `A`, which reads as the shifted, less-frequent form of the same
idea.

While filtering, printable keys extend the query rather than acting as commands.
That is existing behaviour and none of these keys change it — you must leave the
filter (enter or esc) before `d` deletes anything.

## Modes

`State` gains a mode: `List`, `Info`, or `ConfirmDelete`. `on_key` dispatches on
it. `State` still touches no terminal and performs no I/O, so every transition
below is unit-testable without a tty — the property the module was built around.

```
List --i--> Info --i/esc/q--> List
List --d--> ConfirmDelete --y--> (delete) --> List
                          --any--> List
```

From `Info`, `enter` opens the workspace and `q` quits, so the page is never a
trap. From `ConfirmDelete`, **only** `y` proceeds; every other key cancels,
including `enter` — a confirmation whose default answer is destructive is not a
confirmation.

## Effects

Deleting and archiving are I/O. `State` returns them as new `Step` variants that
`run()` performs, keeping the decision logic pure:

- `Step::Delete { name, path }` → `commands::remove_one(&name, &path, false)`
- `Step::SetArchived { name, archived }` → `meta::set_archived(...)`

Both are followed by a reload, so the list reflects the result. This mirrors the
existing `Step::Reload`, which already exists because the visible set can change
underneath the picker.

### Delete semantics

`remove_one` is called with `force: false`, the same as `ws -rm` without
`--force`. Consequences, all deliberate:

- A workspace whose lock a live process holds is **refused**, naming the pid.
  The picker offers no force; deleting a workspace out from under a running
  agent loses whatever that session has not yet written.
- What gets deleted differs by workspace, and the confirmation must say which:
  a managed workspace under the workspaces root is deleted whole, while an
  adopted project loses only its `.ws/` directory and the source tree stays.
  `commands::deletes_whole_directory(path)` already distinguishes them.

The confirmation replaces the hint line:

```
  Delete milo at ~/Projects/Native/milo? [y/N]
  Remove ws metadata from agent-workspaces (the project itself is kept)? [y/N]
```

A failure prints one line above the hint and leaves the row in place. A refusal
is not an error state to recover from — the next keypress returns to the list.

### Archive semantics

`a` toggles `archived` on the selected workspace via `meta::set_archived`, the
same call `ws -archive` makes. With the default filter (`show_archived = false`)
archiving makes the row disappear from the list, which is the feedback; `A`
brings it back so it can be unarchived.

## Info page

`i` draws a full page **for one workspace in place of the list**, using the
picker's existing erase-and-redraw. Not an alternate screen: the module's stated
premise is that it never takes over the terminal, and a page that is just a
taller frame keeps that true — it leaves nothing behind when you leave it.

```
  milo                                    codex
  ~/Projects/Native/milo
  ───────────────────────────────────────────
  status     running · pid 48211
  activity   5m ago
  usage      5h 41%  ·  week 12%
  tags       #native #sync

  OBJECTIVE
  Port the sync layer to gRPC without
  breaking the 0.4 clients.

  NOTEBOOK (last 3)
  · retry budget lives in sync/mod.rs
  · the 0.4 proto is not wire-compatible
  · codex hooks need /hooks trust first

  RECENT
  14:02  opened    ionut
  13:40  rotated   ionut

  esc back · enter open · q quit
```

Rules:

- Every field is optional. An absent one is **omitted**, never rendered blank or
  as a placeholder — the existing list already treats an empty column as a
  rendering bug and a dash as an answer.
- A section whose content is entirely absent (no objective, no notebook, no
  timeline) drops its heading too.
- The notebook tail is capped at 5 lines and RECENT at 4, so the page fits a
  short terminal. Long lines are truncated to the terminal width; nothing wraps,
  because a wrapped line breaks the erase-by-line-count arithmetic.
- Data comes from `WorkspaceRow` (name, path, agent, state, live_pid, tags,
  status, last_activity, limits) and `detail::gather` (objective, notebook,
  chain, queue). Nothing new needs to be read from disk.

Rendering is a pure `render_info(row: &WorkspaceRow, d: &Detail, now: i64,
width: u16) -> Vec<String>`, next to `render_row` and tested the same way: `now`
is passed in so the relative-activity line is assertable.

## Testing

Unit tests in `picker.rs`, driving `State` directly:

- `i` enters and leaves the info page; `esc` and `q` from it do the right thing.
- `d` opens the confirmation; `y` yields `Step::Delete`; `n`, `esc`, `enter` and
  an arbitrary letter all cancel without one.
- `d` on an empty list (everything filtered out) does nothing.
- Keys are inert in the wrong mode — arrows must not move the selection behind
  the confirmation.
- `a` yields `Step::SetArchived` with the flipped value; `A` toggles the filter
  and reloads.
- While filtering, `d` types a `d` rather than deleting.
- `render_info` omits absent fields and drops empty sections; caps hold.

Integration coverage stays where it is: the picker is TTY-only, so `State` is
the seam that gets tested.

## Not doing

- No multi-select delete. One workspace at a time; `ws -rm a b c` exists.
- No `--force` from the picker (see above).
- No editing from the info page. It shows; the agent edits.
