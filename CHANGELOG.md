# Changelog

All notable changes to this project are documented here.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **The picker says what each live agent is doing.** Claude Code publishes a
  record per running session under `~/.claude/sessions/`; the row now reads
  `busy` or `waiting (input needed)` instead of the status text you set last
  week. Three live workspaces used to render as three identical rows, with the
  objective the only thing telling them apart.

  A record outlives a crash, so neither half of the check is optional: the pid
  must still be alive *and* the process must still report the start time the
  record holds, or a pid the kernel has since handed to something else keeps a
  dead session looking busy. Costs a flat two passes however many sessions are
  running — one directory read, one `ps` — because the picker repaints on every
  keystroke. A host that publishes no records simply shows no state.

- **Prompt rewriting on `ctrl+g`, opt-in with `ws config set rewrite true`.**
  Type a rough prompt, press `ctrl+g`, get a precise one in the composer to
  review and send yourself.

  This looked impossible: no hook output can replace prompt text, a blocking hook
  kills the turn before the model is called, and additional context only ever
  appends. The route is Claude Code's own `chat:externalEditor` — it writes the
  composer buffer to a temp file, runs `$EDITOR` on it, and replaces the composer
  with what comes back. ws points `$EDITOR` at a shim.

  The shim runs for *every* `$EDITOR` invocation in the session, so it must not
  break the editor: anything that is not a composer buffer goes to the editor you
  configured, captured at launch. Told apart by containment in the OS temp
  directory, a property ws can check, rather than by a filename pattern, which
  would be a guess about a private implementation detail. Empty buffers, `/`,
  `!` and `#` commands, and buffers holding a paste or image placeholder pass
  through untouched — the buffer holds placeholders, not the pasted bodies, so
  rewriting one destroys the attachment.

  `WS_REWRITE_CMD` (stdin to stdout) replaces the rewriter; otherwise `claude -p`
  runs it, hermetically — neutral working directory, session context variables
  stripped — since a nested agent that inherits the project's `CLAUDE.md` answers
  about the project rather than about the sentence. Every failure path leaves the
  text exactly as typed.

  Off by default: taking over `$EDITOR` for a whole session is intrusive enough
  to be asked for rather than assumed.

- **`ws <base> -features` says which feature worktrees can merge, and why not.**
  One line per `base@*` workspace: whether it merges, how many commits it would
  bring in, and the first thing standing in the way, with `--porcelain` for a
  script.

  The readiness it shows is computed by the same function `--merge` refuses
  through, so the screen cannot promise a merge that then refuses, nor report a
  blocker that would have gone straight through. A description of a rule that
  lives beside the rule is a second implementation of it, free to drift.

  A refused merge now reports *every* blocker rather than the first: fixing one
  only to be refused for the next is the slow way to find out there were three.

- **Messages between workspaces: `ws -msg`.** Two agents working on related
  things — a service and its client, a refactor and the tests for it — had no way
  to tell each other anything. `ws -msg <workspace> "<body>"` delivers a message;
  the receiving agent is told on its next prompt and keeps being told until
  someone reads it, since a message arriving mid-turn is otherwise announced at a
  moment nobody is looking and never again. `ws -msg` reads and clears, `ws -msg
  log` shows the whole exchange, and the status line carries `✉ N` while anything
  is unread.

  `-` reads the body from stdin, which is what makes a multi-KB handoff practical:
  a body that size does not belong in argv, where every `ps` on the machine can
  read it. `--kind task` also queues the body in the recipient, because reading a
  message consumes it and work that survives being read is what a queue is for.
  Every message carries a thread id and `--reply <thread>` answers in it.

  Each message is its own file, staged in `tmp/` and renamed into `new/`. That
  shape is the whole design: cs shipped the append-to-a-shared-file version first
  and measured four concurrent senders leaving 112 of 200 lines intact, with the
  torn lines silently dropped. A rename cannot interleave. Unread is `new/*.json`
  — one definition, so the badge, the digest and the reader cannot disagree.

  Mail lives in `.ws/local/mail/`, which is gitignored: it is addressed to a
  running agent on this machine, not to whoever clones the repository next month.
  This reverses part of the 0.3.0 refocus, which removed an earlier `-msg` along
  with `-tui`, `-spawn` and `-queue`.

- **Crash recovery.** Every turn end saves the working tree — tracked edits and
  untracked files alike — as a git commit beside your branch at
  `refs/ws/session/<conversation>`, built through a private index so your own
  index, HEAD, branches and stash are untouched and the snapshots stay out of
  `git log`. A session that ends normally deletes its own snapshot, so whatever
  is left is a session that did not, and the next launch says so and hands you
  the commands to inspect, restore or discard it.

  Nothing is restored automatically: a launch can be unattended, and a
  crash-recovery feature that writes over the working tree without being asked is
  worse than the crash. When HEAD has moved since the snapshot was taken the
  whole-tree restore is not offered at all — only the per-file one — because
  replaying an old tree over work since committed or rebased is the outcome worse
  still.

  One ref per conversation, not one per repository: a linked worktree shares its
  parent's ref namespace, so two sessions on one checkout would otherwise read,
  write and delete each other's snapshots. A snapshot whose recorded process is
  still alive is a live session's state, never a crash. Unchanged trees write
  nothing, and snapshots from dead sessions are swept after a fortnight.

- **Every verb answers `-h`/`--help` with its own usage.** Asking a verb for help
  sent the flag into that verb's argument parser, which read it as data: `ws -tag
  --help` answered `unknown -tag subcommand: --help`, pointing the reader at a
  subcommand problem when they had asked for documentation. The text is derived
  from `ws -h`'s own lines rather than written a second time, so the two surfaces
  cannot drift apart, and a test reads the dispatch and fails if a verb stops
  answering or has no line to answer with. `ws -secrets` is exempt and still
  forwards to its own eleven-subcommand reference.

  Only the first token after the verb counts, so `ws -task add "fix --help
  handling"` still queues the task.

### Changed

- **An unknown command offers the whole vocabulary, derived from the dispatch.**
  The suggestion list named five verbs of eighteen and had no way to learn about
  a nineteenth. It now prints every documented verb, leads with anything sharing
  a prefix with what was typed, and is checked against the parser by a test.

### Fixed

- **`ws -doctor` stops reporting healthy on the states it exists to catch.** Hook
  registration was decided by searching the whole config file for the hooks
  directory, which passed in three ways it should not have: one mention anywhere
  counted for all five hooks, so a half-registered config (an interrupted
  `setup`, or an older ws) reported everything fine while four hooks never fired;
  a path in an unrelated key — a permission rule, a stale entry — satisfied it;
  and a `settings.json` that is not valid JSON still matched, though the agent
  cannot parse it either and therefore runs *no* hooks at all. Registration is
  now audited per event against the parsed document, an unparseable config is a
  failure that says the agent cannot read it, and a partial registration names
  the events that are missing.
- **A value that is not a process id no longer reads as a live lock holder.**
  `kill -0 0` succeeds for every caller — pid 0 is the caller's own process
  group — and above `i32::MAX` there is no pid at all, with Linux wrapping
  `4294967295` onto `-1`, meaning every process the caller may signal. A lock
  file holding either was a workspace that could never be reclaimed without
  `--force`.
- **The shim check looks at every shim.** It probed `session-start.sh` alone and
  reported "present" for a hooks directory holding only that file, which is what
  a `setup` interrupted part-way leaves behind. None present is still the quiet
  "run `ws setup`"; *some* present is a failure, because a registration pointing
  at a script that is not there fails at hook time, where nobody is reading.

### Breaking

- **`ws -secrets export` namespaces every variable.** A secret named `api_key`
  now exports as `WS_SECRET_API_KEY`, not `API_KEY`. Update anything that evals
  the output and reads the bare name.

  This closes a code-execution hole rather than tidying the format. A secret
  name became a shell variable name directly, so a name like `path`, `editor`,
  `ps1` or `ld_preload` landed on the real `PATH`, `EDITOR`, `PS1` or
  `LD_PRELOAD` when the documented `eval "$(ws -secrets export)"` ran. Refusing
  dangerous names was tried first and does not hold: they are ordinary words,
  and enumerating them leaks. The namespace removes the collision entirely.

### Security

- Two names that would export as the same variable (`api_key` and `api-key`,
  since `-` is legal in a secret name and illegal in a shell identifier) are
  refused rather than both emitted — `eval` takes the last assignment, so
  emitting both lets one silently shadow the other.
- The name rule is applied again at export, so a store written by an older `ws`,
  by hand, or by a restore cannot emit a name that today's `set` would refuse.
  Export is where a name becomes executable text.
- **A refused export emits nothing at all.** It previously printed assignments
  as it went, and `eval "$(...)"` applies whatever reached stdout regardless of
  the exit status behind it — so a refusal could still have applied the values
  it was refusing. The whole output is now built before a byte is printed.

- **The installer stops skipping the signature check in silence.** The whole
  authenticity block was wrapped in `[ -n "$MINISIGN_PUBKEY" ]`, and no key is
  baked in yet — so on a stock install the check was not performed, nothing said
  so, and the only verification output was the checksum pass below it. A run
  whose output reads "checksum OK" and nothing else reads as verified. It now
  says on every unsigned install that authenticity was *not* checked, and that
  the checksum proves the download arrived intact rather than that it came from
  the ws authors.

  A release that *does* ship `SHA256SUMS.minisig` while the installer carries no
  key now refuses outright rather than warning: a signature nobody verifies is
  exactly what this gate exists to catch, and a stripped key is what a tampered
  installer looks like. `--allow-unsigned` still gets past it, typed.

  `tests/install_sh.rs` drives the real script end to end against a fabricated
  release with `gh` stubbed on PATH — the gates had no tests at all, which is
  how a block that never ran stayed invisible.

## [0.8.0] - 2026-08-14

### Added

- Durable project rules now have a shared home: `.ws/conventions.md`. The
  managed block in `CLAUDE.local.md` and `AGENTS.md` tells both agents to read it
  on start and to record a lasting rule there when you state one — so "this repo
  has no test suite" is said once, not once per agent. The block also now points
  both agents at `.ws/memory/` as readable markdown; it was described as Claude's
  redirect target and nothing told Codex it could read it.

  The file is created on demand rather than by `contract::init`. Seeding it at
  creation would put an untracked `.ws/conventions.md` in every worktree of a
  workspace that predates it, and an untracked file counts as dirt — so
  `ws <base>@<feature> --merge` would refuse until it was committed.

### Fixed

- `ws <name> -codex` now records the agent even when the workspace had none
  recorded. `default_agent` was only written on a *switch*, and a switch needs
  something to differ from — so a `workspace.toml` written before the key
  existed, hand-edited, or restored without it ran the agent you asked for and
  then forgot it, and the next bare `ws <name>` fell back to the global default.
  The backfill reads the identity file back after the workspace is opened, so
  creating a workspace with a flag is still not treated as a switch and writes
  no `agent-switch` event.
- A `base@feature` worktree inherits the agent of the workspace it came from
  instead of the global config default. A worktree is the same project on a
  branch; a Codex workspace's worktrees were opening Claude. The value is only
  written when it actually differs, since `.ws/workspace.toml` is tracked and an
  unconditional write would leave every new worktree dirty.

## [0.7.0] - 2026-08-12

### Changed

- The picker and `ws -list` order workspaces by when they were last used, most
  recent first, instead of alphabetically by name. The registry is a `BTreeMap`,
  so the old order was an accident of the storage rather than a decision, and it
  put the workspace you were in a minute ago wherever its name happened to fall.
  Ties fall back to the name, so the order is stable between runs, and a
  workspace whose age cannot be read (never touched, or a missing `.ws/`) sorts
  last rather than first.
- Opening a workspace now counts as using it: `launch` stamps
  `.ws/local/last-opened`, which the activity column reads alongside `README.md`,
  the timeline, notebooks and handoffs. Without it a session where the agent
  wrote nothing left no trace at all, and the workspace you had open yesterday
  ranked below one you last edited in June. The file is under `local/`, which
  `.ws/.gitignore` already excludes, so this adds no per-launch git churn.

## [0.6.5] - 2026-08-11

### Added

- `ws -doctor` reports when the current workspace's `.ws/` is gitignored. That
  costs three things silently: `contract::init`'s `git add -- .ws` fails so no
  init commit is recorded, notebooks and handoffs never reach anyone cloning the
  repo, and `merge=union` in `.ws/.gitattributes` cannot apply, because a merge
  driver only runs on tracked files. It is reported as a note, not a failure —
  ignoring `.ws/` is the right call for a public repository whose working notes
  are not meant to ship, which is what this repository does.

### Fixed

- The workspace lock could be held by several processes at once. Acquiring it
  created the lock file and wrote its body as two steps, so between them the
  file existed with zero bytes — and an empty file is valid TOML with no `pid`,
  which the takeover path reads as a *stale* lock. A second `ws` arriving in
  that window therefore deleted the live holder's lock and claimed the
  workspace. Sixteen racing threads produced up to nine simultaneous "holders".
  The body is now written to a private temp file and `hard_link`ed into place:
  `link` still fails if the lock exists, but the name it publishes already has
  its content, so the lock file is never observable empty. (`rename`, this
  codebase's usual atomic publish, is wrong here — it replaces the destination,
  which is the theft this guards against.) The existing contention test caught
  this at roughly one run in three, which read as flakiness; it now runs enough
  rounds to make a regression certain rather than occasional.
- `ws -secrets` no longer prompts for the file backend's master password where
  no terminal can answer. `secrets()` opened the store before dispatching, and
  `open` builds the file backend's password eagerly, so every subcommand
  authenticated and the `/dev/tty` open surfaced raw as `Device not configured
  (os error 6)` — an errno from which the `$WS_SECRETS_PASSWORD` way out is
  undiscoverable. Two changes: `ws -secrets backend` reports the configured
  backend without opening a store (it decrypts nothing, so it must not
  authenticate, and it now answers outside a workspace too), and the
  password-needing subcommands check for a usable terminal first and otherwise
  fail naming `$WS_SECRETS_PASSWORD`. Interactive use is unchanged — the check
  reads `/dev/tty` exactly as rpassword does, so `ws -secrets set K < value`
  from a terminal still prompts. This only ever affected
  `secrets_backend = file`; the default `auto` prefers the OS vault where one
  works and never prompted.

## [0.6.4] - 2026-08-10

### Fixed

- `ws --help` lists `ws -secrets help`, which shipped in 0.6.3 undocumented.
- `help_covers_every_command` reads the command tokens out of the parser's match
  arms instead of comparing against a hand-written list. The old list could only
  confirm that commands someone had remembered were documented — the one thing it
  could not do was notice a new one, which is how `-secrets help` shipped missing.
  Aliases (`-h`, `-V`, ...) are exempt by name, so an exemption is deliberate.

### Documentation

- The README no longer calls the repository private; it is public. `install.sh`
  still requires `gh auth status` to pass, which is now stated as the installer
  limitation it is rather than implied to be a privacy requirement.
- The release description lists what a release actually carries — `install.sh`,
  `SHA256SUMS`, and both target archives — not just an Apple Silicon binary.
- "Releases are not yet authenticated beyond TLS" is corrected: build provenance
  attestation is generated and verifiable; the minisign signature is what is
  missing, because `MINISIGN_SECRET_KEY` is unset.
- Known limitations record the pre-0.6.3 keyring data loss, and that the Linux
  keyring path is covered by no test at all (its tests skip on a headless runner).

## [0.6.3] - 2026-08-10

There are no 0.6.1 or 0.6.2 releases. Both tags were cut while the Linux side
of the keyring fix was still wrong — 0.6.1 would not build against the system
D-Bus library, and 0.6.2 failed its own test run on a headless runner with no
Secret Service — so neither produced artifacts.

### Fixed

- **The keyring backend never stored anything.** `keyring` was declared without
  a platform feature, and with none enabled that crate falls back to an
  in-memory mock: `ws -secrets set` returned success, the name was written to
  the on-disk index so `ws -secrets list` kept showing it, and the value was
  discarded when the process exited. Every `ws -secrets get` in a later process
  answered "no such secret". The real OS vault is now linked on macOS, Windows
  and Linux, and a build for a target with no store fails rather than falling
  back to the mock.

  **Secrets stored by 0.6.0 or earlier under the keyring backend are gone and
  cannot be recovered** — they never reached the vault. Names still listed by
  `ws -secrets list` must be stored again. The file backend was never affected.

- `ws -secrets get` distinguishes a name the store lists but cannot resolve —
  the fingerprint of the loss above — from a name that was never stored, rather
  than reporting both as "no such secret".
- `ws -secrets help` prints the subcommands instead of failing with "unknown
  -secrets subcommand: help", as does a bare `ws -secrets`, and an unrecognised
  subcommand now lists the valid ones. None of these need a workspace or the
  master password.

## [0.6.0] - 2026-08-05

### Added

- The picker acts on the workspace you have highlighted, not just opens it:
  `i` shows an info page (path, status, usage, tags, objective, notebook tail,
  recent timeline), `d` deletes after a confirmation, and `a` archives or
  unarchives. The info page is drawn in flow like every other frame — no
  alternate screen — and is trimmed to fit a short window.
- Deleting from the picker is `ws -rm` without `--force`: a workspace another
  process holds is refused by pid, and the confirmation states which of the two
  outcomes applies — a managed workspace is deleted whole, an adopted project
  keeps its source and loses only `.ws/`.

### Fixed

- The letters `j k q d a` could not be typed into the picker's filter: the
  terminal layer resolved them to commands before the filter saw them, so `/al`
  toggled archived instead of finding "alpha". What a printable key means is now
  decided where it is known whether a filter is being typed.
- Modified keys are no longer read as the bare letter. Ctrl-D arrived as `d`,
  which with the new binding would have opened a delete confirmation; it now
  leaves the picker, like Ctrl-C.

- The Stop hook no longer blocks a turn that is only ending because a previous
  Stop hook blocked it. Both agents send `stop_hook_active` on such a
  continuation and ws ignored it, so the notebook reminder, the limit handoff
  and the task prompt could each re-enter the turn they had just interrupted —
  which is what kept pulling long Codex runs back into notebook bookkeeping.

### Changed

- Picker keys: `d` is delete (it was "toggle detail" — that view is now the `i`
  info page) and `a` archives the selection (the show-archived filter it used to
  own moved to `A`).
- The notebook reminder now stays quiet for 30 minutes after it fires, not 5.
  At five minutes an hour of uninterrupted work was interrupted a dozen times.
  What counts as a freshly written notebook is unchanged at 5 minutes.

### Added

- `notebook_prompt` config key (default `true`). `ws config set notebook_prompt
  false` switches the notebook reminder off entirely, the way `task_prompt`
  already did for the task prompt.

## [0.5.0] - 2026-08-05

### Added

- Opening a workspace now prints `Update available: <current> → <latest>
  (ws -update)` when a newer release exists, matching what `cs` shows on session
  open. The GitHub lookup is cached for an hour under `~/.cache/ws/update-check`
  (or `$XDG_CACHE_HOME/ws/`), so at most one launch an hour pays for it; a failed
  lookup is recorded too, so a machine with no `gh` or no network does not retry
  on every launch. A launch already on the latest release prints nothing, every
  failure path is silent, and `WS_NO_UPDATE_CHECK=1` switches the check off
  entirely. `ws -update --check` refreshes the same cache.
- The notice lists what you would be getting: one headline per release newer
  than the installed one, read from the published `CHANGELOG.md` (up to five,
  then "… and N earlier versions"). Cached per pending version alongside the
  release check, so it costs one extra lookup per new release and none after
  that; an unreachable changelog leaves the version notice intact. The same
  list prints under `ws -update --check`.

### Changed

- The launch prompt now reads `Resume previous conversation in <name>? [y/N]`
  instead of `Start a new conversation in <name>? [y/N]`, and **the default
  flipped with it**: pressing Enter now starts a fresh conversation, and only
  `y` resumes. A launch with no terminal to ask still resumes, so scripts are
  unaffected. An unresumed conversation is not lost — `ws -conversations` still
  lists it.

## [0.4.0] - 2026-07-29

### Added

- Every workspace now has a color. It is allocated at creation, written to
  `workspace.toml`, and shown two ways: as the terminal tab background (iTerm2
  and WezTerm, already supported but never populated) and as a filled chip
  carrying the workspace name at the head of the status bar. Workspaces created
  before this release are backfilled on their next launch.
- Launching a workspace another terminal holds now offers a choice instead of
  only an error: open one of its existing feature worktrees, force a second
  session, split off a new feature worktree, or cancel. Cancel is the default, so
  an unrecognised key does the harmless thing. Needs a terminal — a launch with
  no TTY still fails with the "in use by pid" error rather than rendering a menu
  into a pipe, and `--force` skips the menu entirely.
- `ws <base>@<feature>` now opens the worktree when it already exists, instead of
  failing with "already exists". Previously the first launch created it and every
  launch after that was a dead end, leaving the workspace unreachable by name.
- `ws <name>` now asks `Start a new conversation in <name>? [y/N]` when there is
  a previous conversation to resume. The default is No, so pressing Enter resumes
  and the common case stays one keypress. It only asks when there is something to
  resume, `--fresh` was not passed, and stdin is a terminal — a scripted launch
  resumes silently as before rather than blocking on a read. Disable with
  `ws config set resume_prompt false`.
- The Stop hook now surfaces captured tasks: when a turn ends with tasks waiting,
  it names the oldest and asks whether to start it, without starting anything.
  It fires once per *change* to the queue rather than once per turn — the stamp
  records the newest pending task id, so declining generally holds and the next
  prompt waits for something new to be captured. `ws config set task_prompt false`
  disables it. Known limitation: the stamp tracks the newest *pending* task, so
  removing one can make an older task the newest again and re-raise a prompt that
  had already been declined.
- `ws -color <red|blue|green|yellow|purple|orange|pink|cyan>` sets a workspace's
  color; `--clear` drops it and the next launch allocates a new one. Takes
  `--workspace <name>` like `ws -status`.

### Changed

- The Claude status line is now a bar of filled blocks rather than a line of
  middot-separated text. Each segment carries its own background: the workspace
  in its color, the model in periwinkle, the git branch in slate, and the three
  gauges on a quiet warm neutral that escalates to amber and then red on that
  gauge's own value. Blocks abut, so the change of background is the separator;
  neighbours that share a background get a hairline so they stay countable.
  `NO_COLOR` still produces the old middot line, unchanged.
- Context now has warning thresholds (amber at 50%, red at 80%), which it never
  had. The 5-hour window warns earlier than before (70/90, was 85/95). The weekly
  window is unchanged at 90/95: a weekly figure passing 70% is normal mid-week,
  and a block that sits amber for days stops being read.
- The color palette in `term.rs` was retuned from terminal primaries to Claude
  Code's theme tokens, so the tab background and the status-bar chip are the same
  color. `magenta` remains accepted as an alias for `purple`.

### Fixed

- `cargo test` failed with three errors when run from inside a ws workspace: the
  integration harness inherited `WS_WORKSPACE`, `WS_DIR` and `WS_AGENT` from the
  surrounding launch, so tests asserting behavior *outside* a workspace ran
  inside one. The harness now clears them.

## [0.3.0] - 2026-07-29

### Removed

- The full-screen dashboard (`ws -tui`), replaced by an inline arrow-key picker —
  see Added. It printed to an alternate screen, and `ratatui::init()` on a
  non-terminal panicked rather than erroring as documented.
- `ws -queue drain` and `ws -spawn`: the unattended headless worker and its tmux
  launcher. Capturing a task and *running* one are different features, and only
  the first was wanted.
- `ws -msg` and cross-workspace mail, including its injection into the next
  session's context.
- `ws migrate-cs`, the one-time `cs` session importer.
- `ws subagent-statusline`: a second status-line pipeline for one Claude-only pane.
- The `PreToolUse` bash-audit hook. It appended every shell command to
  `.ws/local/log/session.log`, which no ws code path ever read and `-search`
  deliberately excluded — a file with no reader is exhaust, not a feature. The
  shell tool kind survives for user-defined hooks.
- `ws config set --workspace`, which parsed, threaded a flag through the call
  chain, and then always errored "added in a later task".
- Dead surface: `Agent::has_prior_session`, `Agent::headless`/`headless_succeeded`
  (drain was the only caller), `WorkspaceRow.color` (no writer existed),
  `rows::list_workspaces`, and the lock file's unread `host`/`tty` fields.
- The `ratatui` dependency, which cut the build from 271 crates to 169.

### Added

- **User-defined hooks.** `hooks.toml` in ws's config directory declares your own
  hooks, and `tool = "shell" | "file-write"` is resolved to each agent's own tool
  names — one declaration, both agents. `ws hooks check` validates and prints what
  would be registered without writing anything; `ws hooks list` shows the current
  registration per agent. An event an agent cannot fire is skipped and reported
  rather than silently written.
- **An inline workspace picker.** Bare `ws` on a terminal moves a highlight
  through the list with the arrow keys; enter opens, `d` shows detail, `/` filters,
  `a` reveals archived. It draws where you are and leaves the list in scrollback —
  no alternate screen, no clear. Without a terminal it prints the list.
- `ws -task add|list|rm` and the `/ws:task` prompt: capture a task without
  interrupting the agent, defaulting to the workspace you are in.
- `ws -rotate [<name>]`: write a timestamped handoff skeleton naming the agent,
  actor, session id and objective, and record a `handoff-written` event.
- **Exact Codex session identity.** ws records the session id Codex reports in its
  `SessionStart` hook payload and resumes it with `codex resume <uuid>`. This
  replaces `codex resume --last` behind an ownership marker and an 81-line scan of
  `$CODEX_HOME/sessions` (up to 500 files × 64 KiB *per launch*) — all of it
  guesswork, which one bare `codex` run in the same directory could redirect
  permanently. With no recorded id, launch starts fresh and says so.
- **Conversation lineage that exists.** `conversations::record_rotation` had zero
  callers, so every `rotated` row `ws -conversations` could render described a
  shape production never wrote. The `SessionStart` hook now records rotations for
  both agents from one code path.
- `ws -who` summarises the timeline per actor — event count, distinct kinds, and
  time span — instead of ranking `git log` authors, which could not say what
  anyone did. It falls back to the commit ranking when there is no timeline yet.

### Fixed

- **`ws -rm` no longer turns a flag typo into a workspace name.** `ws -rm --forec
  myws` tried to delete a workspace literally called `--forec`, reported "no such
  workspace", exited **0**, and never touched `myws`.
- **`contract_version` can no longer be wrapped past the gate.** It was cast with
  `as u32`, so `4294967297` truncated to `1` and *passed* the check that exists to
  refuse it, while `-1` reported "created by a newer ws (contract v4294967295)".
- **`ws -conversations` no longer panics on a non-ASCII session id.**
  `&id[..12]` byte-sliced after a `len() > 12` guard that counts bytes.
- **The keyring secret backend no longer reports success while secrets remain.**
  `rm` dropped the name from the index whether or not the vault delete succeeded —
  leaving the value in the keychain with no name anywhere to reach it by — and
  `purge` swallowed both the vault delete and the index unlink.
- **Redaction fits inside its own hook timeout.** It called `get` + `set` per
  credential, each a full Argon2id derivation and whole-store re-encryption, so a
  `.env` with a few dozen secrets exceeded the 10-second hook bound — and the kill
  landed after values were stored but before the file was rewritten, with the
  warning never reaching anyone. It now decides first and writes once.
- **Every remaining shared read-modify-write is transacted:** the hook
  registrations in `~/.claude/settings.json`, `~/.codex/hooks.json` and
  `~/.codex/config.toml` plus both status-line backups (nine sites, on the user's
  own agent configuration), `.ws/artifacts/MANIFEST.json`, `.ws/README.md`, and
  the agent context file.
- **A hook and a built-in on the same event can coexist.** Registration ran in two
  passes and the second pass's "drop stale ws entries" deleted the group the first
  had just added, so adding a user hook on `Stop` silently removed ws's own.
- **`ws setup` no longer duplicates a user hook on every run.** Only the built-in
  script names were recognised as ws-owned, so a user shim read as foreign and was
  appended again each time.
- **Launch takes the workspace lock before creating the workspace**, so two
  simultaneous `ws newproj` cannot both run `contract::init` in the same
  repository. A refused creation cleans up the skeleton the lock created.
- **`--merge` checks the base workspace's lock too.** A merge rewrites the base's
  working tree, and only the feature side was ever checked.
- **`ws -uninstall` refuses to delete a cargo build artifact.** The name check
  passed for `target/debug/ws`, so running it in a checkout deleted the binary
  cargo had just built. Integrations are still unregistered.
- **`ws -doctor` distinguishes absent, unreadable and registered** instead of
  folding a read error into "not registered", and returns an error rather than
  calling `process::exit` from inside a `Result`.
- **`config set` validates `theme`, `limit_action` and `default_agent`.** Only
  `"warn"` was ever honoured for `limit_action`; every other value reported success
  and behaved as `handoff-stop`.
- **The task queue caps the serialized line, not the input text.** JSON escapes a
  control character to six bytes, so 8 KiB of them produced a ~49 KiB line — six
  times over the cap that exists to prevent a torn append.
- **Timestamps no longer fork `/bin/date`** and can no longer silently be the
  empty string, which was the `ts` under the timeline, the queue, lock bodies and
  the credential manifest — and the field `conversations` sorts on.
- **`ws -secrets` and every other command now agree on which workspace they mean.**
  `-secrets` used the current directory's name where the rest used the recorded
  one, so for a directory adopted under a different name it read and wrote a
  different store.
- **Hook shims are written atomically.** A hook firing during `ws setup` could
  `exec` a truncated script.
- A foreign hook in a sibling directory sharing the hooks-directory prefix
  (`<hooks_dir>-legacy/foo.sh`) is no longer deleted by `ws setup`; the match is on
  path components, not string prefixes.

### Changed

- The read policy every state file shares — absent → default, unreadable →
  refuse — is one helper (`io_read::read_or_absent`) instead of sixteen hand-rolled
  copies, so it can be checked in one place.
- One git wrapper for the crate. The previous three disagreed, and `contract.rs`'s
  reported stderr only — which for `git merge`-class failures, whose diagnostics go
  to stdout, produced errors reading `git … failed:` with no reason at all.
- `theme` resolves to ANSI escape codes for the picker rather than `ratatui`
  colours, so `config theme` keeps a real consumer.


### Security

- Transact the keyring secrets backend's name index. Its read-modify-write ran
  with no interprocess lock while the file backend's did, so two concurrent
  `ws -secrets set`/`rm` calls could lose one's update to the index.
- `ws -secrets get`/`rm` now validate the secret name the same way `set`
  already did, in both the file and keyring backends.
- `ws -secrets` no longer trusts `$WS_WORKSPACE` verbatim: it is validated
  against the same workspace-name allowlist as everything else, closing a path
  traversal (`WS_WORKSPACE=../../foo`) into the secrets directory.
- `FileStore::purge` no longer reports success when the underlying file
  removal fails; only a genuinely absent file counts as "nothing to purge".
- Secret redaction no longer fails silently when the secret store is
  unavailable (no `$WS_SECRETS_PASSWORD` for the `file` backend, or `open`
  otherwise fails). A file left unredacted for this reason now warns on
  stderr and is logged in `.ws/local/log/session.log`.
- Redaction now requires both a credential-shaped name **and** a
  credential-shaped value, instead of name alone. This removes false
  positives such as `PASSWORD_MIN_LENGTH=8`, `TOKENIZER=gpt2` and
  `SECRET_SCAN_ENABLED=true` while still catching
  `AWS_ACCESS_KEY_ID=AKIA…` and `GITHUB_PAT=github_pat_…`.
- Redaction is now scoped to the workspace root: a file the agent writes
  outside it (canonicalized, so symlinks and `..` can't spell their way out)
  is left alone and only noted in the session log.
- Claude's file-write hook matcher now covers `MultiEdit` and `NotebookEdit`
  in addition to `Write` and `Edit`, so redaction actually runs on those tool
  calls instead of silently skipping them (NotebookEdit payloads name their
  target `notebook_path`, which the hook now reads).
- Redaction refuses to overwrite a stored secret with a different value for
  the same name: the later file's line is left as plaintext and reported,
  because overwriting would make `ws -secrets restore` write one file's
  credential into another file.
- Redaction and `ws -secrets restore` rewrite files with their original
  permission mode preserved; previously the rewrite recreated a `0600` `.env`
  at the hook's umask (typically `0644`).
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
  read-modify-write: the registry, config, `workspace.toml` (including its
  tags), `state.toml`, the encrypted secret store and the keyring backend's
  name index. An atomic rename makes a write all-or-nothing but does not stop
  two processes each reading, changing and renaming — the second silently
  discarding the first.
- Linux is built and tested in CI (`ubuntu-24.04`), and releases now publish a
  statically linked `x86_64-unknown-linux-musl` binary alongside Apple Silicon.
  `install.sh` resolves the asset for the host instead of refusing anything that
  is not Darwin/arm64.
- `ws -secrets restore <file>` resolves every `{{ws:secret:NAME}}` placeholder a
  redacted file holds back to its stored value, preserving the file's original
  permissions. Names the store doesn't have are left in place and the command
  exits non-zero.
- A contract-version gate: `ws <name>` and other mutating commands refuse a
  workspace whose `workspace.toml` was written by a newer `ws` than the one
  running, naming both versions. `-list` and `-search` are exempt; equal,
  older, and legacy (no version recorded) workspaces all still open.
- `ws -queue add` rejects task text over 8192 bytes, so one oversized write
  can't tear under a concurrent `O_APPEND` and corrupt the whole queue.
  `ws -msg` caps message bodies at the same 8192 bytes — mail is one file per
  message rather than appended, so tearing isn't the risk, but an unbounded
  body is still unbounded shared per-workspace state another session reads.
- `ws -queue drain` now bounds a single task's run time — default 900 seconds,
  overridable with `WS_DRAIN_TIMEOUT_SECS` — killing and reaping a wedged
  headless agent instead of hanging the drain forever; the timeout counts as a
  failure toward the circuit breaker. Child stdout/stderr are now written to
  files under the workspace's local log directory rather than buffered in
  memory.
- `SessionStart` mail injection is capped at the 10 most recent unread
  messages and 16 KiB total. A truncated session gets a line naming how many
  older messages were skipped, pointing at `.ws/mail/` for the rest; every
  unread message is still marked seen either way.
- Add minisign-based release signature verification to `install.sh`, gated on
  `MINISIGN_PUBKEY`. That key ships empty, so verification does not run yet —
  releases remain authenticated by TLS and a SHA-256 checksum only, same as
  before. Publishing a key (see `docs/releasing.md`) turns on fail-closed
  verification, with `--allow-unsigned` as the explicit opt-out.

### Fixed

- Remove a panic in `ws <name> -claude` when `state.toml` becomes corrupt or
  unreadable between the two reads launch used to make of it; this now
  degrades to a fresh session instead of crashing.
- Break the Codex resume loop. Launch now does a bounded, env-overridable
  probe of `$CODEX_HOME/sessions` before resuming, and falls back to a fresh
  session (with a one-line notice) when no session exists on disk for this
  workspace's directory. Previously a Codex process that died at launch left a
  marker that every later `ws <name>` tried to resume, forever.
- `ws -tag add`/`ws -tag rm` no longer lose a concurrent update: the read and
  the write of the tag list are now one locked transaction instead of two.
- `ws -queue drain --reset` now deletes the circuit-breaker marker under the
  workspace lock instead of before acquiring it, closing a race against a
  concurrent `--reset` or a drain mid-trip.
- `ws <base>@<feature>` now validates the derived workspace name **before**
  creating anything in git, and rolls back the branch, worktree, and registry
  entry if a later step fails. Previously an invalid feature name (for example
  `api@$(x)`) could leave an orphaned branch and worktree that `ws` could
  neither see nor clean up.

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

[Unreleased]: https://github.com/djock/agent-workspaces/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/djock/agent-workspaces/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/djock/agent-workspaces/compare/v0.6.5...v0.7.0
[0.6.5]: https://github.com/djock/agent-workspaces/compare/v0.6.4...v0.6.5
[0.6.4]: https://github.com/djock/agent-workspaces/compare/v0.6.3...v0.6.4
[0.6.3]: https://github.com/djock/agent-workspaces/compare/v0.6.0...v0.6.3
[0.6.0]: https://github.com/djock/agent-workspaces/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/djock/agent-workspaces/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/djock/agent-workspaces/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/djock/agent-workspaces/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/djock/agent-workspaces/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/djock/agent-workspaces/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/djock/agent-workspaces/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/djock/agent-workspaces/releases/tag/v0.1.0
