# Workspace protocol (ws)

You are working in a **ws** workspace named `{{name}}`. Durable state lives in `.ws/`, and every agent that opens this workspace reads the same files — so what you write there is what the next one knows, whether that is you tomorrow or the other agent.

- On start, read `.ws/README.md` (the objective), `.ws/conventions.md` if it exists, and the notebooks in `.ws/notebook/`.
- `.ws/conventions.md` holds this project's **durable rules** — what stays true across sessions and across agents: "this repo has no test suite", "never touch `vendor/`", the build command to prefer. When the user states one, record it there; create the file if it is not there yet. Session-specific detail does not belong in it — that goes in your notebook.
- `.ws/memory/` is Claude's memory directory, redirected here so it stays with the workspace rather than in a home directory. It is plain markdown: read `MEMORY.md` and the entries it indexes whichever agent you are. Only Claude's memory tool writes to it — if you are not Claude, record durable facts in `.ws/conventions.md` instead.
- Append findings to your own notebook: `.ws/notebook/notebook.<actor>.md` (`ws -whoami` for your actor).
- On rotate or agent switch, write a handoff to `.ws/handoffs/`.
- Store secrets with `ws -secrets set NAME` (value on stdin); never paste credentials into files — the redaction hook will replace any it catches with `{{ws:secret:NAME}}`.
