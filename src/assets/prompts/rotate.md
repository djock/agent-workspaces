---
model: claude-sonnet-5
---

Rotate this ws conversation: write a handoff a fresh conversation can continue from.

1. Run `ws -rotate`. It writes a skeleton under `.ws/handoffs/` and arms it, so the next fresh conversation in this workspace starts from it.
2. Before filling it in, list every fact this conversation produced that is written nowhere else: ids, exact readings and error text, counts, the order things happened, what the user said, asked for or corrected. Those go in **Conversation-only facts**, word for word. A fact that also lives in a commit, spec or notebook gets a path, not a restatement.
3. Fill every section of the skeleton:
   - **Next step**: the first action, with every fact it needs (command, path, branch), even if it is also written elsewhere. If it needs a clean worktree, make its first step "commit the handoff".
   - **Where things stand**: label each claim `measured` (you ran it and saw the output; include the command) or `assumed`.
   - **Rejected alternatives**: what lost and why, so it is not re-tried.
   - **Watch out for**: traps and decisions already made.
4. Keep the user's words close to verbatim. Condense your own.
5. Never copy a credential or personal data into the handoff. Name the secret, not its value.
6. Append fresh findings to your own notebook (`.ws/notebook/notebook.<actor>.md`; `ws -whoami` names the actor).

End by telling the user the handoff path. Your last line to them is exactly: "Type /clear to continue in a fresh conversation." On Codex, say instead: "Open the workspace again with `ws <name>` to continue."
