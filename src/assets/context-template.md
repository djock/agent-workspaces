# Workspace protocol (ws)

You are working in a **ws** workspace named `{{name}}`. Durable state lives in `.ws/`.

- On start, read `.ws/README.md` (objective) and the notebooks in `.ws/notebook/`.
- Append findings to your own notebook: `.ws/notebook/notebook.<actor>.md`.
- On rotate or agent switch, write a handoff to `.ws/handoffs/`.
- Store secrets via `ws -secrets` — never write credentials into files.
- Store secrets with `ws -secrets set NAME` (value on stdin); never paste credentials into files — the redaction hook will replace any it catches with `{{ws:secret:NAME}}`.
- Memory (Claude) is redirected to `.ws/memory/`.
