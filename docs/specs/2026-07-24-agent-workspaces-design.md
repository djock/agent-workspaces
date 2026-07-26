# agent-workspaces (`ws`) — Design

Date: 2026-07-24
Status: draft for user review
Repo: `agent-workspaces` · Binary: `ws` · Concept: **workspace** · Metadata dir: `.ws/` · Root: `~/.agent-workspaces/`

## 1. Purpose

`ws` is a from-scratch, agent-agnostic replacement for `cs` (Claude Code session
manager). It gives every piece of work a named, persistent home — a **workspace** —
holding notes, artifacts, secrets, and any number of agent conversations, and it can
launch any supported coding agent (Claude Code, Codex CLI, Gemini CLI) inside that
home interchangeably.

A workspace deliberately avoids the word "session": agents use "session" to mean one
conversation, while a workspace contains many conversations across many agents.

### Goals

- One Rust binary, zero runtime dependencies beyond git and the agents themselves.
- Silent by default: no interactive questions at launch (configurable back on).
- Agent-agnostic core: all durable state is plain files any agent can read.
- Keep the cs features the user kept (catalog below); cut what was cut.
- Interchange agents on one workspace, bridged by notes/handoffs (transcripts are
  agent-proprietary and do not transfer).
- Limit-aware handoff: know when Claude's 5-hour/weekly windows are closing and save
  work before the wall, so Codex can continue.

### Non-goals

- Mid-conversation transfer of chat history between agents (impossible: each agent
  can only load its own transcript format).
- Distribution to other users (signed releases, checksummed self-update). `ws -update`
  is git-pull-and-rebuild.
- Companion skills (voice, prose-hygiene guidance, merge-gates) — cut (#42).
- Checkpoints (#17) — cut; git history of the workspace covers it.
- Web/mobile agents (ChatGPT web) — only CLI agents can be launched into a directory.

### Design principle (from user feedback)

**Silent by default, informative when asked, interactive only when the user
initiates.** The launch prompt exists but ships disabled; the TUI appears only on
request; hooks never block the user's flow (only the agent's, where intended).

## 2. Feature scope (traceability to the cs catalog)

Kept: A1–A10 (A3 locking in minimal form), A11 TUI (redone, simpler);
B12–B16, B18–B20 (B17 checkpoints cut); all C21–C30 as multi-agent adapters;
D31–D32; E33–E37; F38–F41 (F39 simplified, F40 simplified, F42 cut).
New: limit-aware handoff; per-workspace agent choice; config system with
`prompt-on-launch` toggle (default off).

## 3. Architecture overview

```
┌─────────────────────────── ws (one Rust binary) ───────────────────────────┐
│ CLI (clap)          TUI (ratatui)          statusline mode (stdin JSON)    │
│        └──────────────┬──────────────────────────┘                         │
│                  core library                                              │
│  workspace store · contract · config · locks · search · secrets · queue    │
│  mail · worktrees · limits · timeline · actors · doctor · migrate          │
│                       │                                                    │
│                 agent adapters (trait Agent)                               │
│         claude ─ codex ─ gemini  (launch, resume, context file,            │
│         hooks install, prompt-ware install, headless run, limits source)   │
└────────────────────────────────────────────────────────────────────────────┘
   reads/writes only: ~/.agent-workspaces/<name>/.ws/**  +  agent config dirs
```

Three executable faces of the same binary:
- `ws …` — the CLI.
- `ws` with no args — interactive picker; `ws -tui` — the full dashboard.
- `ws statusline` — invoked by Claude Code as its statusline; doubles as the
  rate-limit sensor.

Hook scripts and prompt files are **embedded in the binary** (`include_str!`) and
materialized to disk by `ws setup`; nothing is fetched from the network.

## 4. The workspace contract (`.ws/`)

Each workspace is a git repository. Layout:

```
~/.agent-workspaces/<name>/
├── (user's working files — the workspace root is the agent's cwd)
├── CLAUDE.local.md | AGENTS.md | GEMINI.md   # generated context files, managed blocks
└── .ws/
    ├── workspace.toml        # identity: name, created, tags, color, default agent,
    │                         # status text, archived flag
    ├── README.md             # objective (auto-captured from first prompt) + outcome
    ├── notebook/
    │   ├── NOTES.md          # index of notebook entries (loaded at agent start)
    │   └── notebook.<actor>.md  # per-actor lab notebook (append-only narrative)
    ├── memory/               # agent-native memory redirect target (Claude only)
    ├── artifacts/
    │   └── MANIFEST.json     # tracked reusable scripts/configs
    ├── handoffs/             # rotation + agent-switch handoff documents
    ├── mail/                 # inter-workspace messages (one JSON file per message)
    ├── queue/
    │   ├── tasks.jsonl       # pending/completed walk-away tasks
    │   └── journal.log       # drain runs, breaker trips
    ├── timeline.jsonl        # events: created, opened, closed, rotated, agent-switch
    ├── plans/
    └── local/                # NEVER committed (gitignored)
        ├── lock              # PID + heartbeat mtime
        ├── state.toml        # per-agent conversation ids (claude uuid, codex id…)
        ├── limits.json       # latest rate-limit snapshot per agent
        └── log/session.log   # bash audit + tool failures
```

Rules:
- Everything under `.ws/` except `local/` is committed; `.ws/local/` and secrets
  never are (enforced by generated `.gitignore` + doctor check).
- Notebook files use git `merge=union` attributes so parallel worktrees merge
  without conflicts.
- The contract is versioned (`contract_version` in workspace.toml) so future `ws`
  versions can migrate.

## 5. Config system

Global: `~/.config/ws/config.toml`. Per-workspace override: `.ws/workspace.toml`
`[config]` table. CLI: `ws config list|get <key>|set <key> <value>` (global) and
`ws config set --workspace <key> <value>`.

```toml
default_agent   = "claude"       # claude | codex | gemini
prompt_on_launch = false         # the [Y/n/r/d] question; OFF by default (user decision)
limit_warn_5h   = 85             # % of 5-hour window that triggers handoff warning
limit_warn_week = 90             # % of weekly window
theme           = "auto"         # auto | light | dark
statusline      = true           # register ws statusline in Claude Code settings
nerd_fonts      = false
sessions_root   = "~/.agent-workspaces"   # overridable, also via WS_ROOT env
```

## 6. Agent adapter layer

```rust
trait Agent {
    fn id(&self) -> &'static str;                  // "claude" | "codex" | "gemini"
    fn binary(&self) -> &str;                      // claude | codex | gemini
    fn is_installed(&self) -> bool;
    fn context_file(&self) -> &str;                // CLAUDE.local.md | AGENTS.md | GEMINI.md
    fn launch(&self, ws: &Workspace, mode: LaunchMode) -> Command;  // resume/fresh
    fn headless(&self, ws: &Workspace, prompt: &str) -> Command;    // claude -p | codex exec | gemini -p
    fn install_hooks(&self, ws: &Workspace) -> Result<()>;
    fn install_prompts(&self) -> Result<()>;       // /summary /wrap /sweep /rotate
    fn conversation_id(&self, ws: &Workspace) -> Option<String>;    // from .ws/local/state.toml
    fn limits(&self, ws: &Workspace) -> Option<LimitsSnapshot>;
}
```

Per-agent notes:

| Concern | Claude Code | Codex CLI | Gemini CLI |
|---|---|---|---|
| Context file | `CLAUDE.local.md` (not committed) | `AGENTS.md` (managed block) | `GEMINI.md` (managed block) |
| Resume | `--resume <uuid>` (uuid recorded in state.toml) | `codex resume` / `--last` (verify exact flags at impl.) | verify at impl.; fallback: fresh + notebook |
| Hooks | `~/.claude/settings.json`, full event set | `.codex/hooks.json`, same event names as Claude; experimental → doctor enables | `settings.json` hooks, ~12 events; map nearest equivalents |
| Headless (queue) | `claude -p` | `codex exec` | `gemini -p` |
| Limits source | statusline JSON (`rate_limits.five_hour/seven_day` used_percentage + resets_at) — official | `/status` parse or usage files — best effort, verify | best effort, verify |
| Memory redirect | `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE` → `.ws/memory/` | n/a | n/a |

Context files are generated from one embedded template into each agent's file, inside
sentinel-marked managed blocks (`<!-- ws:begin -->…<!-- ws:end -->`) so user content
around them survives regeneration. All three carry identical instructions: the
workspace protocol (read README + notebook on start, append findings, handoff on
rotate/switch, secrets via `ws -secrets`).

Facts marked "verify at impl." get confirmed against live docs during that phase;
each has a stated fallback that works without the feature.

## 7. CLI surface

```
ws <name>                    create or resume workspace (silent; uses default agent
                             or the workspace's recorded agent)
   --agent claude|codex|gemini   launch with a specific agent (records it)
   --fresh                   new conversation (no prompt)
   --fresh --handoff         new conversation seeded from latest handoff
   --force                   override lock
ws <base>@<feature>          parallel feature worktree
ws <base> --merge <feature>  merge feature worktree back (git merge, union notes)
ws                           interactive picker (fzf-style, built-in)
ws -tui                      full dashboard

ws -list|-ls [--tag t] [--archived]     ws -live
ws -search <query> [--include-archived]
ws -adopt [<name>]           adopt current directory as a workspace
ws -rm <name>… [--force]     ws -archive <name>… / -unarchive <name>…
ws -tag add|rm|list [<name>] ws -status "<text>" | --clear
ws -queue add "task" | list | rm <n> | clear | log
ws -msg <workspace> "body" [--kind notify|task|text|result] | ws -msg | ws -msg log
ws -spawn <name> [--task "…"]          (tmux window)
ws -secrets set|get|list|rm|purge|export|backend
ws -limits                   show all agents' known windows + reset countdowns
ws -usage                    per-workspace token usage (Claude source first)
ws -conversations            conversation chain / lineage
ws -whoami / -who            actor identity / contributors
ws -doctor                   health checks
ws config …                  see §5
ws setup                     install/refresh hooks, prompts, statusline, completions
ws -update                   git pull + cargo build + reinstall (personal tool)
ws completions zsh|bash|fish
ws statusline                (internal: Claude Code statusline entry)
ws migrate-cs [<name>…|--all]  convert ~/.claude-sessions sessions
```

Launch flow (`ws <name>`):
1. Resolve/create workspace dir; validate name; init contract if new.
2. Acquire lock (minimal: PID + heartbeat file; stale = dead PID → silently reclaim;
   collision → one-line error naming the live terminal, suggest `--force`).
3. Regenerate context files if template changed; export env
   (`WS_WORKSPACE`, `WS_ROOT`, memory redirect for Claude).
4. If `prompt_on_launch=true` ask `[Y/n/r/d]`; else resume silently.
5. Record timeline event; exec the agent's command (terminal tab title + color set).

## 8. Hooks (ported behaviors, per-agent installation)

One set of behaviors, installed into each agent's hook system by the adapter.
Scripts are self-contained POSIX shell written by `ws setup` to `~/.config/ws/hooks/`
(no python3/jq — JSON parsing is done by calling `ws` itself in helper mode, e.g.
`ws internal hook-payload <field>`, keeping the zero-dependency promise).

| Behavior (cs #) | Events | Claude | Codex | Gemini |
|---|---|---|---|---|
| Session-start context injection (21) | SessionStart | ✓ | ✓ | ✓ (nearest event) |
| First-prompt → README objective + scope note (12/25 slim) | UserPromptSubmit | ✓ | ✓ | ✓ |
| Bash audit log (22) | PreToolUse | ✓ | ✓ | ✓ |
| Tool-failure log (28) | PostToolUseFailure | ✓ | if event exists | if event exists |
| Notebook reminder, 5-min cooldown (14) | Stop | ✓ | ✓ | nearest |
| Shadow-git autosave (23) | PostToolUse(Write/Edit) | ✓ | ✓ | nearest |
| Auto-approve `.ws/` writes (26) | PermissionRequest | ✓ | if supported | if supported |
| Subagent context (27) | SubagentStart | ✓ | if supported | n/a |
| Secret redaction (32) | PostToolUse(Write) | ✓ | ✓ | ✓ |
| Artifact tracking (20): record new scripts/configs in MANIFEST.json (tracking, not cs's silent redirection) | PostToolUse(Write) | ✓ | ✓ | ✓ |
| Limit warning → "write handoff now" (new) | Stop (reads limits.json) | ✓ | best effort | n/a |
| Prose lint on notes (24) | Stop | ✓ | ✓ | nearest |
| Timeline + index regen (4/19) | SessionEnd | ✓ | ✓ | nearest |

Scope grounding (25) is deliberately slimmed to "record first prompt as objective";
the full repo-grepping context injection is dropped from v1 (noise > value; can
return later). Where an agent lacks an event, the behavior degrades to instructions
in the context file (soft enforcement) — never an error.

## 9. Prompt-ware

`/summary`, `/sweep`, `/wrap`, `/rotate` ported from cs nearly verbatim, stored once
in the repo, installed by `ws setup` into each agent's custom-command location
(Claude: `~/.claude/commands/`; Codex: prompts dir; Gemini: custom commands —
exact paths confirmed at impl.). All write to `.ws/` paths only.

## 10. Limit-aware handoff (new)

1. **Sense** — `ws statusline` receives Claude Code's statusline JSON on every
   update and writes `rate_limits` (both windows: used %, resets_at) to
   `.ws/local/limits.json` (and a global copy for the TUI). Codex/Gemini sensors are
   best-effort later; absence degrades gracefully.
2. **Show** — statusline segment `5h 43% · wk 61%`, escalating colors at thresholds;
   `ws -limits` and the TUI show all workspaces/agents with reset countdowns.
3. **Save & stop** (config `limit_action`, default `"handoff-stop"`; `"warn"` for
   warning-only) — when a threshold is crossed, the Stop hook issues a blocking
   directive to the agent: *finish the current step only — no new tasks — update the
   notebook, write a handoff to `.ws/handoffs/`, then stop and tell the user work is
   saved.* This runs while budget remains, so saving happens before the wall. Writing
   the handoff drops a guard marker in `.ws/local/`; from then on the Stop hook lets
   turns end normally, and the UserPromptSubmit hook prefixes any further prompt with
   a one-line notice that the limit guard is active (the user can keep going anyway —
   it's their budget — but never unknowingly).
4. **Tell the user how to continue** — three channels, all carrying the exact
   command:
   - the agent's final message (the directive requires it): "Work saved to
     `.ws/handoffs/<ts>.md`. Continue with: `ws <name> --agent codex`  — or Claude's
     5h window resets in 1h20m."
   - a macOS desktop notification from the hook (`osascript`), so it's seen even if
     the terminal is in the background;
   - the statusline pinned to `LIMIT · ws <name> --agent codex` until the window
     resets or the agent is switched.
5. **Switch** — `ws <name> --agent codex` starts Codex fresh on the same workspace;
   its context file tells it to read the latest handoff + notebook and continue the
   work. Quotas are independent per provider, which is the point. The guard marker
   clears on agent switch or window reset.

## 11. Secrets (D31–D32)

- Backends v1 (config `secrets_backend = "auto" | "keyring" | "file"`):
  1. **keyring** — the `keyring` crate's native OS vault: macOS Keychain, Linux
     Secret Service (GNOME Keyring/KWallet), Windows Credential Manager. Service
     `ws:<workspace>`, account = secret name.
  2. **file** — encrypted file (`~/.config/ws/secrets/<workspace>.enc`, authenticated
     encryption, master password from `WS_SECRETS_PASSWORD` or hidden prompt) for
     headless Linux (servers, Raspberry Pi over SSH) where no keyring daemon runs.
  `auto` probes the keyring and falls back to file. `backend` reports the active one.
  Out of scope: cs's age-key export/import sync between machines.
- CLI: `set` (value from stdin or hidden prompt — never argv), `get`, `list`, `rm`,
  `purge` (confirm), `export` (prints `export NAME=…` lines).
- Redaction hook: on file writes matching secret patterns (`.env`, `*_KEY=`,
  `TOKEN=`, etc.) store the value, replace with `{{ws:secret:NAME}}`, note it in
  MANIFEST.json. Context files instruct agents to use `ws -secrets` proactively.

## 12. Orchestration (E33–E37)

- **Worktrees** (33): `ws base@feature` = `git worktree add` + minimal `.ws/`
  bootstrap referencing the base; `--merge` = run `git merge --no-ff` with union
  merge for notes, then remove the worktree. cs's record-fusion complexity is
  replaced by the union-merge attributes.
- **Queue** (34): `-queue add` appends to tasks.jsonl; draining runs the workspace's
  agent headless (`claude -p` / `codex exec`) one task at a time; circuit breaker
  stops after 2 consecutive failures; journal logs everything. Drain runs only when
  explicitly started (`ws -queue drain` or `-spawn --task`), never surprises.
- **Mail** (35): JSON files in the target's `.ws/mail/`; unread mail is surfaced by
  the session-start hook; `-msg log` reads history.
- **Spawn** (36): opens `ws <name>` in a tmux window of a `ws` tmux session;
  `--task` seeds the queue and starts draining. Requires tmux; error out plainly if absent.
- **Actors** (37): actor slug from git user.email (fallback: whoami); per-actor
  notebook files; `-whoami`, `-who` (from git history of `.ws/`).

## 13. TUI (A11, redone)

ratatui + crossterm, launched only on request. v1 screens, deliberately small:

1. **Workspace list** — columns: name, agent icon, live dot, status text, tags,
   last activity, limits (when known). Type-to-filter. Enter = open in current
   terminal (replacing the TUI). `a`rchive, `t`ag, `s`tatus, `r`emove (confirm), `q`uit.
2. **Detail pane** — README objective, latest notebook entries, queue/mail counts,
   conversation chain.

Agent info in the TUI is a first-class requirement (user request): per-workspace
agent, per-agent limit state. Everything else waits until wanted ("we'll see as we
go"). Theme: `auto` uses OS appearance + `COLORFGBG`; no tmux DCS passthrough
gymnastics (config override wins).

## 14. Remaining F-group features

- **Doctor** (38): checks — agents installed/versions; hooks registered & enabled
  (Codex hooks experimental flag!); context files present with managed blocks;
  Keychain reachable; `.ws/local` gitignored; locks stale; statusline registered;
  contract version. Exit non-zero on failures.
- **Update** (39, simplified): `ws -update` = `git -C <repo> pull && cargo build
  --release && install`, then `ws setup` to refresh hooks/prompts.
- **Theme** (40, simplified): see §13.
- **QoL** (41): clap-generated completions (workspace names completed dynamically);
  NO_COLOR respected; tab title + color per workspace (OSC escapes, small module).

## 15. Migration from cs

`ws migrate-cs [--all]`: for each `~/.claude-sessions/<name>`: copy (never move)
into `~/.agent-workspaces/<name>`; map `.cs/README.md` → `.ws/README.md`,
`narrative.*.md` → `notebook/notebook.*.md`, memory/, artifacts/, handoffs/,
timeline; generate workspace.toml + context files; secrets stay in Keychain (cs
namespaces them per session — migrator re-stores under `ws:<name>` reading via
`cs -secrets` if available, else documents manual step). cs remains untouched and
usable; both can coexist while confidence builds.

## 16. Implementation shape

Rust 2021, single crate, workspace-free. Key dependencies (all compile into the one
binary): `clap` + `clap_complete`, `ratatui`, `crossterm`, `serde`/`serde_json`,
`toml`, `anyhow`, `dirs`, `keyring`, `grep` + `ignore` (ripgrep's libraries, for
`-search`), `chrono`. Git operations shell out to the system `git` (present on any
dev Mac; avoids libgit2 weight).

```
src/
├── main.rs  cli.rs                    # clap tree, dispatch
├── workspace.rs contract.rs config.rs lock.rs actors.rs timeline.rs
├── agents/{mod,claude,codex,gemini}.rs
├── hooksetup.rs prompts.rs            # embedded assets + installers
├── statusline.rs limits.rs
├── secrets.rs search.rs queue.rs mail.rs worktree.rs
├── doctor.rs migrate.rs update.rs
├── tui/{mod,list,detail,theme}.rs
└── assets/ (hooks/*.sh, prompts/*.md, context-template.md)
```

Error handling: `anyhow` with user-facing messages; hooks must never break the
agent (every hook script ends `exit 0` except intentional blocking feedback).
Logging: `.ws/local/log/`. All output honors NO_COLOR and non-tty.

Testing: unit tests per module; integration tests against temp dirs (`assert_cmd`)
covering the full CLI surface; TUI via ratatui `TestBackend` snapshots; a fake-agent
shim (shell script standing in for claude/codex) to test launch/resume/headless
without real agents; hook scripts tested by piping recorded event JSON.

## 17. Phased delivery

Each phase ends runnable and tested; order chosen so daily value lands first.

1. **Core** — contract, config, create/resume/list/rm/adopt, lock, context-file
   generation, Claude adapter (launch/resume, memory redirect), tab title/color.
   *Usable as a daily driver for Claude from here.*
2. **Protocol** — prompts (/summary /wrap /sweep /rotate), session-start/objective/
   notebook-reminder/audit hooks for Claude, timeline, README auto-objective.
3. **Statusline + limits** — `ws statusline`, limits.json capture, `-limits`,
   threshold warning hook.
4. **Codex adapter** — launch/resume, AGENTS.md, hooks.json install, prompt install,
   interchange flow (`--agent`, handoff seeding), doctor checks for both agents.
5. **Secrets** — Keychain backend, CLI, redaction hook.
6. **Search, tags, archive, status, migration** — `-search`, `-tag`, `-archive`,
   `-status`, `migrate-cs`.
7. **TUI** — list + detail with agent/limit columns.
8. **Orchestration** — worktrees, queue + headless drain, mail, spawn.
9. **Polish** —  doctor completeness, `-usage`, prose lint port,
   shadow-git autosave, completions, `-update`.

## 18. Open items to verify during implementation (each has a fallback)

- Codex: exact resume flags; hooks.json schema details; machine-readable limits.
  Fallbacks: fresh-with-handoff launch; skip unsupported hooks; display-only limits.

- Claude statusline JSON field names re-verified against the installed version.
