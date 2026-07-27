# Security

## Reporting

Report suspected vulnerabilities privately via GitHub's **Report a vulnerability**
button on the Security tab, or by email to the address on the maintainer's GitHub
profile. Please do not open a public issue first.

There is no funded bounty and no guaranteed response window; this is a
single-maintainer project. Say so in the report if you intend to disclose publicly
on a fixed date, so the fix can be scheduled against it.

## What `ws` touches, and why that matters

`ws` is not sandboxed. Installing it means accepting that it:

- **writes into your agents' configuration** — `~/.claude/settings.json` and
  `~/.codex/hooks.json` gain hook registrations, and status-line commands are
  replaced (the prior value is backed up and restored on uninstall);
- **installs hooks that run on every agent session event**, as your user, with no
  sandbox;
- **stores credentials** — in your OS keyring, or in an AES-256-GCM file under the
  ws config directory keyed by a password you supply;
- **runs an agent unattended** when you ask it to (`ws -queue drain`,
  `ws -spawn --task`), which means an agent editing files and running commands
  with nobody watching;
- **writes into your git repositories** — worktree creation and `--merge`.

## Known weaknesses in the current release

Stated plainly rather than left for a reader to discover:

- **Releases are authenticated only by TLS.** Assets carry SHA-256 checksums, but
  nothing is signed, and the updater bootstrap does not verify a signature. A
  compromised release host is not something `ws` currently detects.
- **The unattended drain is a large trust surface.** It never passes a
  permission-escalation flag (there are tests asserting that), has a
  consecutive-failure circuit breaker and a hard iteration cap, and refuses to
  re-run a task orphaned by a crash. It is still an agent acting without
  supervision, and the cap exists because a drained agent can queue more work for
  itself.
- **Secret redaction is a safety net, not a guarantee.** It rewrites
  `NAME=VALUE` assignments whose name looks credential-shaped, in files the agent
  writes through a hook. It cannot see a credential embedded in prose, in JSON, in
  a command line, or written by a tool the hook does not match. Do not rely on it
  to keep a secret out of a file you care about.
- **Command history is logged unredacted.** `PreToolUse` appends shell commands to
  `.ws/local/log/session.log`, truncated but not filtered. A credential passed on
  a command line lands there in plaintext.
- **The Codex hook contract is verified by hand, not by CI.** See
  `docs/2026-07-27-codex-hook-contract-verified.md`. A Codex upgrade could silently
  disable the hooks, including secret redaction.

`docs/2026-07-27-cs-vs-ws-independent-audit.md` carries the full, unflattering
list, including items not security-relevant.

## Scope

In scope: anything that lets a third party read your secrets, execute code you did
not intend, or corrupt or destroy workspace data or your git repositories.

Out of scope: consequences of running an agent you chose to run unattended; damage
caused by `--force` overriding a lock you were told about; and behaviour of the
agent CLIs themselves (report those to Anthropic or OpenAI).
