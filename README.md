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
- Installs hooks and reusable prompts for both supported agents.
- Configures matching Claude and Codex status bars with model, branch, context,
  5-hour usage, and weekly usage.
- Shows available usage-limit information.

`ws` is still under active development. Back up important work and review changes before relying on it for critical projects.

## Requirements

- macOS on Apple Silicon for the prebuilt `v0.1.0` release.
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and/or [Codex](https://developers.openai.com/codex/cli).
- [GitHub CLI](https://cli.github.com/) authenticated with access to the private repository.
- Rust and Cargo only if you choose to build from source.

## Install

Clone the private repository and run the installer:

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
ws                         Open the workspace dashboard
ws -list                  List active workspaces
ws <name> -claude         Open with Claude Code
ws <name> -codex          Open with Codex
ws <name> --fresh         Start a fresh agent session
ws <name> --handoff       Include the latest handoff
ws -search <text>         Search workspace content
ws -limits                Show known usage limits
ws -doctor                Check the installation
ws setup                  Install or refresh hooks, prompts, and status bars
ws --version              Show the installed version
```

Run `ws --help` for the complete command summary.

## Update and uninstall

```sh
ws -update
ws -uninstall
```

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

Releases use semantic versioning. A tag such as `v0.1.0` runs the release workflow and creates a draft GitHub release with a SHA-256 checksum file and an Apple Silicon binary archive.

## License

MIT. See [LICENSE](LICENSE).
