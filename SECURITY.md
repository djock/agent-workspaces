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
- **writes into your git repositories** — worktree creation and `--merge`.

## Known weaknesses in the current release

Stated plainly rather than left for a reader to discover:

- **Releases are authenticated only by TLS.** Assets carry SHA-256 checksums, but
  nothing is signed, and the updater bootstrap does not verify a signature. A
  compromised release host is not something `ws` currently detects.
- **A `hooks.toml` entry runs your command inside the agent's context**, on every
  matching event, with the hook payload on stdin. That is the point of the
  feature, and it is also its whole risk surface. Two deliberate limits: the file
  is read only from ws's own config directory — never from a workspace or a
  repository, so cloning a project cannot introduce a hook — and `ws hooks check`
  prints exactly what would be registered without writing anything, so an entry
  can be reviewed before `ws setup` acts on it. ws validates that each command
  exists and is executable; it does not and cannot vet what the command does.
- **Secret redaction is a safety net, not a guarantee.** It rewrites
  `NAME=VALUE` assignments whose name **and** value both look credential-shaped,
  in files the agent writes through a hook (Claude's `Write`/`Edit`/`MultiEdit`/
  `NotebookEdit`, Codex's `Write`/`Edit`/`apply_patch`), and only inside the
  workspace root. It
  cannot see a credential embedded in prose, in JSON or YAML, in a URL
  (`DATABASE_URL=postgres://user:pw@host`), in a command line, or written by a
  tool the hook does not match. Do not rely on it to keep a secret out of a file
  you care about. What happens when it can't act, stated plainly rather than
  left implicit:
  - **Secret store unavailable** (no `$WS_SECRETS_PASSWORD` for the `file`
    backend so a hook would have to prompt, or the store otherwise fails to
    open) — the file is left with its plaintext untouched, and a warning is
    written to both stderr and `.ws/local/log/session.log`. This used to be a
    silent no-op; it no longer is.
  - **A value fails to reach the store mid-file** — that line is left as
    plaintext rather than replaced with a placeholder for a value that was
    never actually stored; reported the same way.
  - **The file is outside the workspace root** — skipped, noted only in the
    session log (not stderr, so an agent's routine writes to `/tmp` or `$HOME`
    don't train you to ignore the channel that matters).
  - **The rewrite itself fails after values were already stored** — reported
    loudly on stderr, because the secret is now in the store while the
    plaintext is still on disk; remove it by hand.
  - **`ws -secrets restore <file>` meets a name the store doesn't have** — that
    placeholder is left byte-for-byte in the file and the command exits
    non-zero, rather than silently treating "missing" as "resolved".
- **`ws -task add` text is stored in plaintext** in `.ws/queue/tasks.jsonl`, which
  is git-tracked and searchable by `ws -search`. It is prose you wrote, not
  captured output, but do not put a credential in a task.
- **The keyring backend's name index is plaintext.** Secret *values* live in the
  OS keyring; the *names* stored for a workspace also live in
  `<config dir>/secrets/<workspace>.keyring-index` in plaintext, because the
  keyring API itself has no way to list what it holds. That index's
  read-modify-write is now interprocess-locked, the same as every other shared
  state file `ws` writes.
- **The Codex hook contract is verified by hand, not by CI.** See
  `docs/2026-07-27-codex-hook-contract-verified.md`. A Codex upgrade could silently
  disable the hooks, including secret redaction.

`docs/2026-07-27-cs-vs-ws-independent-audit.md` carries an older, fuller list,
including items not security-relevant. It predates the refocus in
`docs/plans/2026-07-28-ws-refocus.md`, which removed the unattended drain, tmux
spawning, cross-workspace mail and the unredacted shell-command log entirely, so
some of what it describes no longer exists.

## Scope

In scope: anything that lets a third party read your secrets, execute code you did
not intend, or corrupt or destroy workspace data or your git repositories.

Out of scope: what a command you put in your own `hooks.toml` chooses to do;
damage caused by `--force` overriding a lock you were told about; and behaviour of
the agent CLIs themselves (report those to Anthropic or OpenAI).
