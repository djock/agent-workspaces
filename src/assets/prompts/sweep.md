---
model: claude-sonnet-5
---

Distill this ws workspace into durable memory entries with a strict bar.

You are in a **ws** workspace. Claude's persistent memory is redirected to `.ws/memory/` (index `MEMORY.md` + `<bucket>_*.md` files). Your task: review the conversation and the workspace, then write only durable facts worth carrying into every future session.

## The bar
- **The memory buckets (`.ws/memory/`) are forever.** Bar: very strict. Default: write nothing.
- Save only what is (a) durable, (b) not already obvious from the code, git history, or README, and (c) useful to a future session.

## Buckets
- **user** — who the user is (role, expertise, durable preferences).
- **feedback** — how you should work here (a correction or confirmed approach). Include the *why*.
- **project** — ongoing goals/constraints not derivable from the code.
- **reference** — pointers to external resources (URLs, tickets, dashboards).

## Steps
1. Review the whole conversation, not just the last turn.
2. For each candidate fact, check it clears the bar and isn't a duplicate of an existing entry — update the existing file rather than adding a near-duplicate.
3. Write each as one file with frontmatter (`name`, `description`, `metadata.type`), then add a one-line pointer to `.ws/memory/MEMORY.md`.
4. Delete any entry you now know to be wrong.

If nothing clears the bar, say so in one line and write nothing.
