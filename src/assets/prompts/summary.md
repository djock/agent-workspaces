---
model: claude-sonnet-5
---

Generate an intelligent summary of this ws workspace by synthesizing its documentation.

You are working in a **ws** workspace. Write `.ws/summary.md` — a concise, high-signal overview a future session (or a different agent) can read first to get oriented.

## Sources (read these)
- `.ws/README.md` — objective and outcome
- `.ws/notebook/notebook.*.md` — per-actor lab notebooks (findings, decisions)
- `.ws/timeline.jsonl` — lifecycle events
- Recent git history of the workspace

## Write `.ws/summary.md` with
1. **What this workspace is for** (1–2 sentences from the objective).
2. **Current state** — what's done, what's in progress, what's blocked.
3. **Key decisions & findings** — distilled from the notebooks, with the *why*.
4. **Next steps** — concrete, actionable.

Keep it tight. Prefer specifics over generalities. Do not invent facts not present in the sources. Overwrite any existing `.ws/summary.md`.
