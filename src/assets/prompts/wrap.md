---
model: claude-sonnet-5
---

Wrap up this ws workspace: distill durable memory, then write a summary.

Run two passes in sequence:

1. **Memory pass** — follow the `/ws:sweep` instructions: distill durable facts into `.ws/memory/` with a strict bar (default: write nothing).
2. **Summary pass** — follow the `/ws:summary` instructions: synthesize `.ws/summary.md` from the README, notebooks, timeline, and git history.

Then complete the **Outcome** section of `.ws/README.md`: a few sentences on what this workspace accomplished, its final state, and anything left for next time.

Report back a one-paragraph recap: what you saved to memory (or that you saved nothing), and that the summary and outcome are written.
