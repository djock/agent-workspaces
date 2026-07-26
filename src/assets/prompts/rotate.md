---
model: claude-sonnet-5
---

Rotate this ws conversation: write a handoff so a fresh conversation can continue.

Context grows heavy or a work phase is done — capture the state so the next conversation starts clean.

## Write a handoff to `.ws/handoffs/<UTC-timestamp>.md` containing
1. **Objective** — what this workspace is for (one line).
2. **Where things stand** — done / in-progress / blocked, with file:line pointers.
3. **Next action** — the single most useful thing to do next, concretely.
4. **Watch out for** — traps, gotchas, decisions already made (and why) so they aren't relitigated.
5. **How to resume** — the exact command(s), and which files to read first (`.ws/README.md`, `.ws/notebook/`, this handoff).

Also append any fresh findings to your own notebook (`.ws/notebook/notebook.<actor>.md`; run `ws -whoami` for your actor). Keep the handoff self-contained: assume the next agent has zero prior context. Finish by telling the user the handoff path and how to reopen.
