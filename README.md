# ws

> **Acknowledgment:** `ws` is inspired by [cs (claude-sessions)](https://github.com/hex/claude-sessions). All credit and thanks are due to its authors and contributors for the original idea. `ws` is an independent work-in-progress built to support both Claude Code and Codex.

`ws` gives each project a named, persistent workspace. The workspace keeps its code, notes, memory, handoffs, and agent state together so you can leave and return later.

Most importantly, the same workspace works with both Claude Code and Codex. If your Codex limit runs out, you can create a handoff and continue the work with Claude without starting the project again.

## What it does

- Creates and resumes named workspaces.
- Launches either Claude Code or Codex in the same workspace.
- Keeps durable notes and handoffs in the workspace.
- Switches agents while preserving the project context.
- Lists, searches, tags, archives, and removes workspaces.
- Creates git worktree workspaces with `ws <base>@<feature>` and merges them back.
- Records who did what: actor identity (`-whoami`) and a per-actor summary of the
  workspace timeline (`-who`).
- Captures tasks without interrupting the agent — `ws -task add`, or `/ws:task`
  from inside a session.
- Writes handoffs for the next session with `ws -rotate`, and shows conversation
  lineage with `ws -conversations`.
- Lets you define **your own hooks** in one file that apply to both agents
  (`hooks.toml`; see [User-defined hooks](#user-defined-hooks)).
- Stores secrets in your keyring or an encrypted file, and redacts
  `NAME=VALUE`-shaped credentials out of files the agent writes through its
  file-edit tools, scoped to the workspace root. Redacted values can be put
  back with `ws -secrets restore <file>`.
- Installs hooks and reusable prompts for both supported agents.
- Configures matching Claude and Codex status bars with model, branch, context,
  5-hour usage, and weekly usage. Claude's is drawn by ws as colored blocks, each
  gauge escalating to amber and red on its own value; Codex renders its own
  built-in segments, so ws only chooses which appear.
- Gives each workspace a color, drawn as the terminal tab background (iTerm2 and
  WezTerm) and as a chip at the head of the status bar, so panes are
  distinguishable at a glance. Allocated at creation and changed with
  `ws -color <color>`.
- Shows available usage-limit information.

## Status and known limitations

`ws` is young and under active development. Back up important work and review
changes before relying on it for critical projects. Being specific about what
that means, because "under active development" is not an honest summary on its
own:

- **Most shared state is now transactionally locked, but not all of it.** The
  registry, config, `workspace.toml` (including tags), `state.toml`, and both
  secret-store backends (the encrypted file and the keyring name index) hold an
  interprocess lock across their whole read-modify-write. Append-only files
  (`timeline.jsonl`, the queue) rely on `O_APPEND` instead, which is correct for
  appends but means anything that ever needs to *rewrite* one of them would need
  a lock added first. Notebooks are not part of that append discipline at
  all — `ws` never appends to one itself, they are free-form files the agent
  edits directly — and are kept safe across a worktree merge only by
  `merge=union` in `.ws/.gitattributes`, which takes the union of both sides'
  lines rather than conflicting.
- **Secret redaction is a heuristic, not a scanner.** It only looks at
  `NAME=VALUE` lines in files an agent writes through a file-edit tool (Claude's
  `Write`/`Edit`/`MultiEdit`/`NotebookEdit`, Codex's `Write`/`Edit`/`apply_patch`), and only
  redacts when both the name and the value look credential-shaped. It does not
  see a secret embedded in a JSON or YAML value, in a URL
  (`DATABASE_URL=postgres://user:pw@host`), or in a file a Bash heredoc wrote
  instead of a file-edit tool. With the `file` secrets backend, a hook has no
  terminal to prompt on: if `$WS_SECRETS_PASSWORD` is not set, redaction reports
  itself unavailable (stderr and the session log) rather than skipping without a
  trace.
- **Secrets stored with the keyring backend before v0.6.3 are gone.** Up to and
  including v0.6.2, `ws` linked the `keyring` crate with no platform feature, and
  that crate then falls back to an in-memory mock: `ws -secrets set` reported
  success, the name was written to the on-disk index, and the value was discarded
  when the process exited. Nothing ever reached the OS vault, so nothing can be
  recovered. The symptom is a name that `ws -secrets list` shows but
  `ws -secrets get` cannot resolve — from v0.6.3 that case says so explicitly
  instead of reporting "no such secret". Store those values again. The encrypted
  `file` backend was never affected.
- **Codex session identity depends on Codex's hooks being trusted.** ws records
  the session id that Codex reports in its `SessionStart` hook payload and later
  resumes it with `codex resume <uuid>` — exact, not a `--last` guess. But Codex
  requires hooks to be trusted via `/hooks` before they fire, and until they are,
  ws has no id to record: every launch starts a fresh session and says so.
  `ws -doctor` reports this.
- **Linux is built and tested in CI but has had no human use.** Every
  macOS-specific call is `#[cfg]`-gated and the suite runs on `ubuntu-24.04`, and
  releases now ship a statically linked `x86_64-unknown-linux-musl` binary
  alongside Apple Silicon. Treat Linux as working-but-unproven; macOS arm64 is the
  platform that has actually been used. The **keyring backend specifically is
  covered by nothing on Linux**: its tests need a live Secret Service, the CI
  runner is headless, and they skip there rather than fail — so a regression in
  the Linux vault path can reach a tag with CI green. That is the shape of the
  bug fixed in v0.6.3, which is why it is called out rather than assumed rare.
- **Releases are attested but not signed.** The workflow generates GitHub build
  provenance, so `gh attestation verify <archive> --repo djock/agent-workspaces`
  confirms which workflow and commit built an artifact. What is missing is the
  minisign signature over `SHA256SUMS`: `MINISIGN_SECRET_KEY` is not set in the
  repository, so the workflow warns "this release will be UNSIGNED" and
  continues. Every release through v0.6.3 is unsigned, and the updater bootstrap
  is unverified.
- **A workspace whose name contains `@` cannot be launched by name**, because
  `@` means "worktree" in a bare argument.
- **The Codex hook contract is not covered by CI.** It was verified by hand
  against Codex CLI 0.145.0 (`docs/2026-07-27-codex-hook-contract-verified.md`);
  a Codex upgrade could break it silently.

`docs/2026-07-27-cs-vs-ws-independent-audit.md` is the older gap list against
`cs`. It predates the refocus described in
`docs/plans/2026-07-28-ws-refocus.md`, which deleted the features ws had copied
rather than needed — the dashboard, the unattended queue drain, cross-workspace
mail, tmux spawning, and the `cs` importer — so parts of it describe code that no
longer exists.

## User-defined hooks

ws ships six hooks of its own. To add your own, write
`~/.config/ws/hooks.toml` (or `$XDG_CONFIG_HOME/ws/hooks.toml`):

```toml
[[hook]]
event   = "PostToolUse"           # required
tool    = "file-write"            # optional: "shell" | "file-write"; omit = every tool
command = "~/bin/my-hook.sh"      # required; must exist and be executable
timeout = 30                      # optional, seconds, default 10
agents  = ["claude", "codex"]     # optional, default both
```

Then `ws setup`.

The point of declaring it here rather than editing each agent's config by hand is
that **`tool` is resolved per agent**: `"file-write"` becomes
`Write|Edit|MultiEdit|NotebookEdit` for Claude and `Write|Edit|apply_patch` for
Codex. Written by hand you would have to know both vocabularies and keep them in
step; here you write it once.

Your command receives the hook payload on **stdin** and inherits
`WS_WORKSPACE`, `WS_DIR`, `WS_ROOT` and `WS_AGENT`. To read a payload field
without needing `jq`:

```sh
tool="$(ws internal hook-payload tool_name)"
```

Fields: `session_id`, `cwd`, `source`, `prompt`, `tool_name`, `command`,
`agent_id`.

- `ws hooks check` validates the file and prints exactly what would be
  registered, **writing nothing**.
- `ws hooks list` shows what is registered for each agent, built-in and yours.
- An event an agent cannot fire is skipped for that agent and reported, never
  silently written (Codex has no `PostToolUseFailure`).
- An invalid entry refuses the whole install rather than half-registering it.

`hooks.toml` is read **only** from your config directory, never from a workspace
or repository — a hook runs a command on every matching event, so a repo-local
hook file would let a cloned project execute code the moment you opened it.

## Requirements

- macOS on Apple Silicon, or Linux x86_64, for the prebuilt release.
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and/or [Codex](https://developers.openai.com/codex/cli).
- [GitHub CLI](https://cli.github.com/), authenticated. The repository is public
  and `gh release download` works without a login, but `install.sh` checks
  `gh auth status` and stops if it fails — the check predates the repo going
  public and has not been relaxed yet.
- Rust and Cargo only if you choose to build from source.

## Install

Clone the repository and run the installer:

```sh
gh auth login
gh repo clone djock/agent-workspaces
cd agent-workspaces
./install.sh
```

The installer downloads the latest prebuilt release to `~/.local/bin/ws`, runs `ws setup`, and then runs `ws -doctor`.

If `~/.local/bin` is not on your `PATH`, add this to `~/.zshrc`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

To build and install with Cargo instead:

```sh
./install.sh --build-from-source
```

You can also install directly from source:

```sh
cargo install --path . --locked
ws setup
```

## Use

Create a workspace and start with Codex:

```sh
ws my-project -codex
```

Return to it later:

```sh
ws my-project
```

Start a fresh Claude session in the same workspace and include the latest handoff:

```sh
ws my-project -claude --fresh --handoff
```

Switching from one agent to the other automatically points the new agent at the latest available handoff. For the smoothest continuation, ask the current agent to write a concise handoff before switching.

Useful commands:

```text
ws                             Pick a workspace from a list (see the keys below)
ws -pick                       Same, explicitly
ws -list                       List active workspaces, most recently used first
ws <name> -claude              Open with Claude Code
ws <name> -codex               Open with Codex
ws <name> --fresh              Start a fresh agent session
ws <name> --handoff            Include the latest handoff
ws <name> --force              Take over a workspace another process holds
ws <base>@<feature>            Create a git worktree workspace, or open it
ws <base>@<feature> --merge    Merge it back and remove it
ws -search <text>              Search workspace content
ws -adopt [<name>]             Adopt the current directory
ws -rm | -archive | -unarchive Remove or hide workspaces
ws -tag | -status              Label a workspace
ws -color <color>              Set its tab and status-bar color
ws -whoami | -who [<name>]     Your actor slug; who did what, from the timeline
ws -conversations [<name>]     Conversation lineage: rotations and agent switches
ws -rotate [<name>]            Write a handoff skeleton for the next session
ws -task add|list|rm           Capture tasks without interrupting the agent
                               (the agent is asked about them when a turn ends)
ws -secrets set|get|list|...   Manage workspace secrets (`ws -secrets help`)
ws -limits                     Show known usage limits
ws -doctor                     Check the installation
ws setup                       Install or refresh hooks, prompts, and status bars
ws config list|get|set         Read or change configuration
ws hooks list|check            Show or validate hook registration
ws -update | -uninstall        Update or remove ws
ws --version                   Show the installed version
```

`ws --help` documents the full surface, including every launch flag. A test reads
the command tokens straight out of the parser's match arms and fails if one of
them is missing from the help text, so a newly added command cannot ship
undocumented. (It used to compare against a hand-written list, which could only
check commands someone had remembered to add to it — `ws -secrets help` shipped
missing from the help exactly that way.)

In the picker, `enter` opens the highlighted workspace and the other keys act on
it too:

```text
↑↓ / j k    move            i    info page for this workspace
enter       open            d    delete it, after a confirmation
/           filter          a    archive or unarchive it
q / esc     quit            A    show or hide archived workspaces
```

Deleting from the picker is the same operation as `ws -rm` without `--force`: a
workspace another process is running is refused rather than pulled out from
under it, and an adopted project loses only its `.ws/` — the source tree stays.
The confirmation says which of the two is about to happen.

`ws -task add` only records a task; nothing runs it for you. That is deliberate —
the point is to write a thought down without derailing the one you are having.
Inside a session, `/ws:task` does the same thing without you leaving the agent.

## Update and uninstall

```sh
ws -update
ws -uninstall
```

Opening a workspace also tells you when a newer `ws` has been released:

```
▌ Update available: 0.4.0 → 0.6.0 (ws -update)
▌   0.6.0  Opening a workspace now says when a newer ws exists.
▌   0.5.0  Every workspace now has a color.
```

One headline per release you would be getting, read from the changelog. The
lookups are cached in `~/.cache/ws/` (or `$XDG_CACHE_HOME/ws/`) — the release for
an hour, the notes until a newer release appears. A launch that is already
current prints nothing, and every failed lookup is silent. `ws -update --check`
prints the same list on demand; `WS_NO_UPDATE_CHECK=1` switches the check off.

Uninstalling removes the installed binary and integrations owned by `ws`. It does not remove your workspaces or their contents.

## Where data lives

- Workspaces: `~/.agent-workspaces/` by default
- Configuration and registry: the platform configuration directory under `ws/`
- Per-workspace metadata: `.ws/` inside each workspace

The workspace root can be changed with:

```sh
ws config set sessions_root /absolute/path/to/workspaces
```

## Development

```sh
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

Releases use semantic versioning. A tag such as `v0.1.0` runs the release
workflow, which builds both targets and creates a **draft** GitHub release
carrying `install.sh`, a `SHA256SUMS` file, and one archive per target
(`aarch64-apple-darwin` and `x86_64-unknown-linux-musl`). Drafts are published by
hand, so an unsigned or half-uploaded release is never public. `docs/releasing.md`
has the full procedure.

## License

MIT. See [LICENSE](LICENSE).
