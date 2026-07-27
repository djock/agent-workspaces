# Changelog

All notable changes to this project are documented here.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Security

- Fix a command injection in `ws -spawn`. The tmux command was built as a single
  shell string, which tmux runs through `sh -c`, so shell metacharacters in a
  workspace name were executable; `-adopt` never validated names, making it
  reachable. The command is now passed to tmux as argv, which tmux `execvp`s
  directly. Also fixes `-spawn` for install paths containing a space.
- Validate workspace names against an allowlist (letters, digits, `-`, `_`, `.`,
  `@`), enforced in `contract::init` **and** `registry::register` so `-adopt`,
  `migrate-cs` and worktree creation cannot bypass it. The previous denylist
  admitted spaces, `;`, `$`, backticks, quotes, newlines and control characters.
- Fix secret redaction on Codex, which never ran. Hook matchers are now resolved
  per agent through `Agent::tool_matcher`: Codex reports a file edit as
  `apply_patch`, so Claude's `Write|Edit` matcher could never fire. The handler
  also reads `apply_patch`'s patch envelope, since those payloads carry no
  `tool_input.file_path`, and redacts every file a multi-file patch writes.
  Verified against Codex CLI 0.145.0 — see
  `docs/2026-07-27-codex-hook-contract-verified.md`.
- Stop discarding redaction failures. A failed file rewrite (secret stored but
  plaintext still on disk) and a failed manifest write are now reported on
  stderr instead of being swallowed.
- Refuse to overwrite a corrupt `artifacts/MANIFEST.json` instead of resetting it
  to `{}`, which silently discarded every recorded redaction (M7).
- Close the lock-acquisition race. `acquire` tested `exists()` and then wrote, so
  two processes could both take a workspace. The lock file is now created with
  `O_CREAT|O_EXCL`, and the stale-reclaim path goes back through the same
  primitive so two callers that both judge a lock stale cannot both win.

### Added

- `ws -conversations [<name>]` shows conversation lineage: which agent session
  replaced which and why, and where work moved between Claude and Codex. Launch
  now records a `rotated` timeline event carrying from/to/reason, and
  `agent-switch` records `from` and the handoff seeded, not just `to`.
- Interprocess transactions (`txn::transaction`) over every shared
  read-modify-write: the registry, config, `workspace.toml`, `state.toml` and the
  encrypted secret store. An atomic rename makes a write all-or-nothing but does
  not stop two processes each reading, changing and renaming — the second
  silently discarding the first.
- Linux is built and tested in CI (`ubuntu-24.04`), and releases now publish a
  statically linked `x86_64-unknown-linux-musl` binary alongside Apple Silicon.
  `install.sh` resolves the asset for the host instead of refusing anything that
  is not Darwin/arm64.

### Changed

- `ws --help` now documents the whole command surface, including `-limits`,
  `-doctor`, `-secrets`, `setup` and every launch flag. A test fails if a command
  exists that the help text does not mention.
- `config set statusline false` now actually prevents `ws setup` from registering
  a status line. It was previously settable and read nowhere.

### Removed

- `prompt_on_launch` and `nerd_fonts` config keys. Both were settable, listed by
  `config list`, and read nowhere, so setting them reported success and did
  nothing. `config set` now rejects them. Existing `config.toml` files still load;
  the stale keys are ignored.
- The `-resume` launch flag, which parsed and did nothing — resuming is the
  default. It is now an error explaining that, rather than a silent no-op.

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
