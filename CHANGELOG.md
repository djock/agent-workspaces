# Changelog

All notable changes to this project are documented here.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/djock/agent-workspaces/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/djock/agent-workspaces/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/djock/agent-workspaces/releases/tag/v0.1.0
