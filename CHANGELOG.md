# Changelog

All notable changes to this project are documented here.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-07-27

- Add actor identity and contributor history with `ws -whoami` and `ws -who`.
- Add inter-workspace mail with message history and session-start surfacing.
- Add persistent task queues and unattended draining with crash reaping,
  circuit breaking, and an iteration cap.
- Add tmux spawning for interactive workspaces and queued task drains.
- Add git worktree workspaces with `base@feature`, safe `--no-ff` merge-back,
  conflict rollback, and live/dirty repository guards.
- Isolate interactive and unattended agent session lineages where the agent
  CLI permits it, and refuse ambiguous legacy Codex session ownership.
- Treat `@` in a bare workspace argument as a worktree separator. Existing
  adopted workspaces whose literal names contain `@` cannot be launched by
  name in this release.

## [0.1.2] - 2026-07-26

- Allow Claude to stop silently instead of emitting the invalid Stop-hook decision `approve`.
- Configure matching Claude and Codex status bars with one model label, branch,
  context usage, and both 5-hour and weekly limits, without a folder path.

## [0.1.1] - 2026-07-26

- Quote hook and status-line executable paths so macOS configuration paths containing spaces work.
- Keep hook setup idempotent for both old unquoted and new quoted registrations.
- Silence expected process-probe errors when reclaiming stale workspace locks.

## [0.1.0] - 2026-07-26

- Create, resume, adopt, list, search, tag, archive, and remove workspaces.
- Launch Claude Code and Codex from the same persistent workspace.
- Carry the latest handoff into a fresh session or an agent switch.
- Install hooks, prompts, and Claude status lines.
- Track agent sessions, workspace memory, timeline events, and known limits.
- Store workspace secrets with keyring and encrypted-file backends.
- Provide an interactive terminal dashboard.
- Add installation, update, uninstall, diagnostics, CI, and release packaging.

[Unreleased]: https://github.com/djock/agent-workspaces/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/djock/agent-workspaces/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/djock/agent-workspaces/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/djock/agent-workspaces/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/djock/agent-workspaces/releases/tag/v0.1.0
