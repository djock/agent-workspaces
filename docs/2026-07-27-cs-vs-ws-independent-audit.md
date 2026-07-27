# `cs` vs `ws` — independent audit and roadmap to parity

**Written:** 2026-07-27
**Auditor:** external pass (Claude), reading both codebases from disk rather than from their READMEs
**Baselines:** `ws` @ `1f89786` (`v0.2.0`) · `cs` installed `v2026.7.24`, upstream `main` @ `v2026.7.26`
**Relationship to existing docs:** complements `2026-07-27-cs-vs-ws-comparison.md` (the project's own self-audit) and `2026-07-26-ws-remaining-work.md` (the M1–M8 ledger). Where this document disagrees with either, it says so explicitly.

> ## Status update — 2026-07-27, branch `audit-fixes`
>
> The audit below is a **point-in-time record and is deliberately not rewritten**.
> Several findings have since been fixed on the `audit-fixes` branch. Test count
> went 366 → 383, clippy clean.
>
> **Fixed:** Stage 0 (the Codex hook contract is now empirically verified —
> `docs/2026-07-27-codex-hook-contract-verified.md`); Stage 1 (per-agent hook
> matchers, so Codex secret redaction actually runs, plus `apply_patch` envelope
> parsing and a parsed-JSON replacement for the string-contains test in
> `tests/setup.rs`); Stage 3 in full (`-spawn` passes argv to tmux instead of a
> shell string, name validation moved to an allowlist enforced in
> `contract::init` and `registry::register` so `-adopt` cannot bypass it, and M7's
> manifest clobber now refuses instead of resetting); Stage 2 in full (three inert
> config keys resolved — `statusline` implemented, `prompt_on_launch` and
> `nerd_fonts` removed; the `-resume` no-op removed; `--help` completed with a
> drift test; README rewritten with a specific known-limitations section; the
> stale design doc marked superseded).
>
> **Corrected in §1 relative to the first draft of this audit:** the claim that
> `ws` wrote a structurally invalid `hooks.json` and omitted a required feature
> flag was **wrong on both counts**. Codex's `hooks` feature is stable and
> defaults to true, and Codex deliberately mirrors Claude's wire format. §1 as it
> stands reflects the corrected finding; §1.3's "still unproven" list is now
> largely resolved and superseded by the verification doc.
>
> **Not started:** Stage 4 (interprocess transactions — the `lock::acquire` TOCTOU
> and the unused `fs2` dependency), Stage 5 (Linux CI, public repo, authenticated
> releases, shell completions, `cargo fmt` gate — repo-wide formatting still
> fails), Stage 6 (conversation lineage, checkpoints, `-limits` staleness check),
> Stage 7 (managed-block refresh path, injection instrumentation), Stage 8.
> M3 and M6 remain open.

---

## 0. Scope and method

This is an adversarial read of both tools, commissioned as "hard truth, pro and contra." It is not a product review and not a marketing comparison. Every claim below is anchored to a file and line, a command that was run, or is explicitly flagged as unverified.

### 0.1 A method lesson worth keeping

The single most important process finding: **for agent-harness contracts, probe the installed binary — do not read documentation about it.**

The first pass of this audit concluded that `ws`'s Codex hook integration was completely inert. That conclusion was drawn from web search results and was **wrong in two separate directions**. Two published sources contradicted each other on the basic question of whether `hooks.json` has a `hooks` wrapper. The authoritative answer came from the shipped binary in under five minutes:

```bash
codex features list | grep hooks
strings "$(readlink -f "$(which codex)")" | grep -E "HookSpecificOutputWire|additionalContext|permissionDecision"
strings "$(readlink -f "$(which codex)")" | grep -E "^(SessionStart|SessionEnd|PreToolUse|PostToolUse|UserPromptSubmit|Stop|SubagentStart|SubagentStop|PreCompact|PostCompact|PermissionRequest)$"
```

Both projects depend on undocumented or under-documented harness internals. Both should adopt binary probing as the verification method of record, and both should record the probe command alongside any claim about harness behaviour so the claim can be re-checked after an upgrade.

### 0.2 What could not be verified

- **`cargo test` was never run.** No Rust toolchain is present on the audit machine (`~/.cargo` absent, `cargo`/`rustc` not on `PATH`, no `target/`). Every `ws` test claim here is a static read of source, not an execution result.
- **The Codex per-event array element shape** was not confirmed byte-for-byte. Evidence strongly implies Claude compatibility (§1) but this needs the live test in Stage 0.
- **`cs`'s pinned `age` v1.3.1 SHA-256 table** was not checked against upstream artifacts. The real macOS Keychain and Windows Credential Manager paths were not exercised; `cs`'s own suite fakes both.
- The installed `cs` is two releases behind upstream, so a small number of `lib/`-relative line citations may drift against `/Users/ionut.mocanu/.local/bin/cs`.

---

## 1. The Codex hook contract — corrected findings

This section supersedes the first-pass conclusion. It is the highest-value technical content in the document because it determines whether `ws`'s differentiator is one afternoon of work or a rewrite.

### 1.1 What is true (verified against Codex CLI 0.145.0)

| Question | Answer | Evidence |
|---|---|---|
| Is there an opt-in feature flag? | **No.** `hooks` is `stable` and defaults to `true`. | `codex features list` → `hooks   stable   true` |
| Does `[features].codex_hooks = true` exist? | **No.** That name is stale/incorrect. A separate `plugin_hooks` feature exists. | `codex features list` |
| Does Codex mirror Claude's hook wire format? | **Yes, deliberately.** | Binary embeds `SessionStartHookSpecificOutputWire`, `UserPromptSubmitHookSpecificOutputWire`, `PreToolUseHookSpecificOutputWire`, `PostToolUseHookSpecificOutputWire`, `PermissionRequestHookSpecificOutputWire`, `SubagentStartHookSpecificOutputWire`, plus literals `hookSpecificOutput`, `additionalContext`, `permissionDecision`, `permissionDecisionReason` |
| Config pointer | `"hooks": "./hooks.json"` | binary strings |
| Supported events | `SessionStart`, `SessionEnd`, `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`, `PermissionRequest` | binary strings |
| Not supported | `PostToolUseFailure` | absent from binary |
| Tool names | `shell`, `apply_patch`. **No `Bash`, `Write`, or `Edit` tool exists in Codex.** | feature strings `shell_tool`, `unified_exec`, `apply_patch_freeform`, `apply_patch_streaming_events`; payload carries `tool_name` |

**Consequence: `ws`'s architectural bet was correct.** Reusing one handler set and one schema across both agents is sound. The `hookio.rs` contract — parsing a Claude-shaped payload, emitting `hookSpecificOutput.additionalContext` — is the right contract for Codex too. This is a genuine and non-obvious design win, and it was previously mis-reported as a defect.

### 1.2 The defect that survives

`HOOKS` is a single module-level const shared by both agents (`src/hooksetup.rs:28-35`), which hardcodes Claude's tool names:

```rust
HookSpec { event: "PreToolUse",  matcher: Some("Bash"),       handler: "bash-audit",    script: "bash-audit.sh" },
HookSpec { event: "PostToolUse", matcher: Some("Write|Edit"), handler: "secret-redact", script: "secret-redact.sh" },
```

Neither matcher can match a Codex tool name. On Codex this means **no bash audit and, more seriously, no secret redaction**. The four matcher-less hooks (`SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`) should fire normally.

The reason this shipped undetected is a weak test. `tests/setup.rs:14-16` asserts only that the written file *contains the string* `session-start.sh`, which passes for any schema, any event name, and any matcher.

### 1.3 Still unproven — the Stage 0 test list

1. Is the per-event array element shape byte-compatible, or does Codex reject `{matcher, hooks:[{type,command,timeout}]}`?
2. Does `additionalContext` returned from `SessionStart` actually reach the model?
3. What exact string arrives in `tool_name` for a shell call and for a patch application?
4. Does an untrusted hook silently no-op? Codex has a trust model surfaced through `/hooks` and a `--dangerously-bypass-hook-trust` flag. `ws`'s `hook_trust_note` already instructs users to run `/hooks`, which appears correct.
5. Are `commandWindows` / `statusMessage` fields required or optional?

---

## 2. Scale and provenance

| | `cs` | `ws` |
|---|---|---|
| Repo | `hex/claude-sessions`, public, MIT, 31 stars | `djock/agent-workspaces`, **private** |
| Author | Alexandru Geana / `hex` (944 of 980 commits) | single author, 19 commits |
| Age | created 2025-11-24; ~8 months | 2 days (2026-07-26 → 2026-07-27) |
| Releases | 97, roughly monthly majors with near-daily patches | 4 |
| Production code | ~12,200 lines shipping shell + 9,977 lines Rust TUI | ~7,246 lines Rust |
| Tests | 51 suites, 1,101 `run_test` cases, ~20,190 lines (project's own count: 1,356 tests / 25,428 lines) + 273 Rust | 366 tests, ~7,609 lines |
| Platforms | macOS, Linux, WSL2, MSYS/Git Bash (manage-only) | macOS Apple Silicon only |
| Install | `curl`, unauthenticated | `gh repo clone` against a private repo |
| Agents | Claude Code only (`grep -rni "codex\|openai\|gemini"` → zero hits) | Claude Code + Codex |

**`cs` is not a 6,000-line bash script**, and that critique should be retired. It is 27 focused `lib/*.sh` fragments concatenated by `build.sh` into one deliverable, `bash -n` validated, with CI running `git diff --exit-code bin/cs` after a rebuild so the shipped artifact can never drift from source (`.github/workflows/test.yml:35-44`). That is the correct architecture for a shell tool that must ship as a single file.

---

## 3. Where `cs` genuinely leads

**Test mass and designed-in testability.** A 1.6:1 test-to-production ratio *in shell*, with 40+ named override seams (`CS_CLAUDE_DIR`, `CS_ASSUME_TTY`, `CS_PLATFORM_OVERRIDE`, `CS_TMUX_BIN`, `CS_PS_BIN`, `CS_AGE_BIN`, `CS_STATUSLINE_NOW`, `CS_TRANSCRIPTS_DIR`). `cs_interactive()` exists purely so interactive gates are drivable from the harness — the source comment states that a bare `[ -t 0 ]` is untestable (`lib/10-help.sh:116-117`).

**Portability that is actually exercised.** A real bash 3.2 contract, verified by absence: zero uses of `declare -A`, `mapfile`/`readarray`, `${var,,}`, globstar, `coproc`, `;;&`, or namerefs. `sed -i` is deliberately avoided everywhere with the reason stated (`lib/35-claudemd.sh:72-74` — the BSD `sed -i ''` form errors on GNU sed and would abort session resume on Linux under `set -e`). `grep -P` has zero hits. CI runs macOS + Ubuntu for shell and a 4-shard MSYS lane for Windows.

**Concurrency correctness in `cs-secrets`.** `noclobber` O_EXCL locks carrying the holder PID, traps installed *before* acquire with the reason stated (a signal arriving between creation and trap install would leak the lock permanently), signal handlers that `exit` rather than merely release (bash does not auto-exit after a handler returns), and 10 deterministic concurrency tests using slow-`openssl` shims. The refusal to auto-reap stale locks is argued, not accidental. **This is the single most transferable artifact in the codebase and sets the bar for `ws` Stage 4.**

**Fail-loud posture where it matters.** A decrypt failure never masquerades as an empty store (`bin/cs-secrets:520-530`); `CS_NOT_FOUND=3` separates "absent" from "backend broke" across all three backends; `export` is buffered and emitted only if every secret reads, because `eval "$(…)"` applies whatever reached stdout regardless of exit status.

**The property most worth copying: `cs` measures its own prompt engineering and deletes what fails.** `RETIRED_HOOKS` (`lib/00-header.sh:31-43`) is twelve one-line postmortems backed by real data — an 8-day measurement of actual memory writes retiring a ~940-token guidance block; a 35-session / 3,918-command audit showing 95.1% one-shot reuse retiring command tracking; a hook caught injecting notices dated 27 days stale. Roughly half of everything `cs` ever built to feed the model context was measured and found worthless. Retirement is then enforced twice: a write-time guard that strips stale registrations, plus `_doctor_check_settings_hooks_resolve` as a discipline-free audit added specifically because discipline failed once.

**Feature depth `ws` lacks entirely:** labelled checkpoints, explicit conversation lineage and rotation, `-live` PID+heartbeat monitoring, per-session token usage across 5h/weekly/lifetime windows, a 16-check doctor, shell completions, and signed multi-platform releases.

---

## 4. Where `ws` genuinely leads

Not merely "newer." These are real engineering advantages.

**Panic discipline.** 12 `.unwrap()` and **zero** `panic!`/`unreachable!`/`unimplemented!`/`todo!` across ~7,246 lines of production Rust, with all 12 immediately guarded by a preceding check (`internal.rs:367,371,434`; `hooksetup.rs:124,129,136,209,431,490,494`; `tui/app.rs:130`; `agents/claude.rs:60`). One `#[allow(dead_code)]` in the entire tree.

**`atomic.rs` as an enforced chokepoint.** A single path for every shared write: per-process temp name, mode-at-creation for credential files, `fsync` on both file and parent directory, cleanup on failure. Its module doc notes the fixed-temp-name race "has been written and fixed five separate times in this codebase" — the correct reason to build a chokepoint rather than fix it a sixth time. A test squats the old fixed temp path to prove it is unused.

**The "absent ≠ unreadable" doctrine, applied uniformly.** A missing file is treated as empty; a *corrupt or unreadable* file is a hard error and is never written back. Applied across `config::set`, `registry::load`, `meta::update`, `contract::read_state_table`, `hooksetup::register_settings`/`register_statuslines`, `secrets::FileStore::load`, `KeyringStore::read_index`, `internal::note_manifest`, `mail::unread`, `queue::tasks`, `context::regenerate` — each with a chmod-based regression test asserting the original survives byte-for-byte. This is rare to see done consistently and is `ws`'s most distinctive property.

**The lenient/checked API split.** `lock::live_pid`/`live_pid_checked`, `meta::read`/`read_checked`, `registry::lookup`/`lookup_checked` — with doc comments stating precisely which callers may degrade and why. `remove_one`'s doc names three specific ways the old guard failed open, including `Path::starts_with("")` returning true for every path, which made `ws config set sessions_root ""` turn `-rm` into "delete the adopted project's whole source tree." Four tests pin the fix.

**Worktree merge safety, in one respect better than `cs`.** It refuses when the base is *already* mid-merge, so cleanup can only ever abort a merge `ws` itself started; unconditionally `merge --abort`s on failure; resolves `mid_merge` via `rev-parse --git-path` (correct for linked worktrees where `.git` is a file); reports git's *combined* stdout+stderr because git writes conflict reports to stdout; and preserves the worktree on failure so work is not stranded (`worktree.rs:112-206`).

**An unattended-execution safety model that reasons about unbounded success.** `drain.rs` has a consecutive-failure circuit breaker with a persisted marker requiring `--reset`, crash reaping that marks orphaned `Running` tasks `Failed` and never re-runs them, and a **50-iteration cap specifically because a drained agent inherits `WS_WORKSPACE` and can `ws -queue add` itself** — producing runaway *success* that no failure-counting breaker could ever catch. Tests assert escalation flags are never passed, with the comment that the assertion "is the thing standing between an unattended agent and the user's filesystem."

**Codex session-ownership modelling** (`agents/codex.rs:26-66`). Faced with `resume --last` being physically unable to address two lineages in one directory, it records an ownership token (`last_owner = interactive | drain`) and *refuses* ambiguous legacy state rather than guessing, with tests in both directions and downgrade compatibility for the legacy `launched` boolean. The best code in either repo.

**Correct handling of Codex's absent success signal.** `codex exec` exits 0 and prints a chatty banner even on an outright refusal, so `headless_succeeded` reads only the `-o` output file. The test feeds a realistic refusal banner with exit 0 and asserts failure.

**Foreign-config citizenship.** Installing hooks, status lines, and the Codex footer never destroys what was there: path-boundary matching preserves sibling-prefix foreign hooks; prior status-line commands are backed up with *merge* so an earlier original is never lost; `toml_edit` preserves Codex config comments and layout. It explicitly backs up and restores a competing `cs-statusline`.

**Mutation-test-informed assertions.** Several tests document their own provenance — `tests/orchestration.rs:36-43` explains that the previous assertion still passed with the entire check deleted, and asserts on wording only the intended branch produces.

**Measured performance.** Warm benchmarks from the project's own comparison: version 2.16 ms vs 11.80 ms, help 2.06 vs 16.72, empty list 2.11 vs 11.79, synthetic 100-workspace list **5.22 ms vs 291.60 ms**. The central registry beats a filesystem scan at scale.

---

## 5. `cs` weaknesses — with a disclosure note

These are findings about a **third-party tool by a colleague** (Alexandru Geana, `hex`). They are recorded here because they define the competitive opening and because three of them are security-relevant. **The security items should be reported upstream to `hex/claude-sessions` rather than only living in a competitor's repo.** Recommended disclosure order: the stale instruction block first (it actively degrades agent behaviour), then the argv password exposure, then the unverified install path.

### 5.1 The stale instruction block (most severe)

Every `cs` session's `CLAUDE.local.md` still instructs the agent that Write is auto-redirected to `.cs/artifacts/`, that sensitive data is "automatically detected and stored securely," that "the artifact file contains redacted placeholders," and to read `.cs/artifacts/MANIFEST.json` on resume.

All of it is false. `artifact-tracker.sh` was retired in v2026.7.5, and its own graveyard note states the `PreToolUse:Write` redirect "was inert (updatedInput path rewrite is not honored by the harness)" — **it never worked at all**. Automatic secret capture died with it.

It is permanent, not transitional: `migrate_claude_md_to_local` returns early whenever the `cs:session-protocol` sentinel is present (`lib/35-claudemd.sh:286-288`), so no existing session's block is ever refreshed, and `tests/test_prune_commands.sh:100` explicitly asserts the stale `## Artifact Auto-Tracking` section is *retained*. Upstream prose docs are honest about the retirement (`docs/secrets.md:200`); the deployed prompt is not.

An agent that believes secrets are auto-redacted on Write is measurably less careful than one that knows they are not. Combined with §5.3, this is the worst finding in either codebase.

### 5.2 Undeclared hard dependency on `jq`

All 10 hooks use `jq` — `narrative-reminder.sh` 13 times, `session-start.sh` and `scope-prompt.sh` 8 each — and **not one has a `command -v jq` guard**. `check_dependencies` (`lib/25-deps.sh:4-15`) checks only the claude binary, not `jq` and not `git`. Without `jq`, 8 of 10 hooks die under `set -e` on every session event. The degradation path is untested.

### 5.3 Secrets handling gaps

- **The master password is on argv**: `openssl -pass "pass:$password"` at 4 sites. On Linux `/proc/<pid>/cmdline` is world-readable, and Linux/WSL is precisely where the file backend is the *only* option. `-pass env:`/`fd:`/`stdin` exist and are unused. Ironic given the code warns about *the user's* value on argv (`bin/cs-secrets:2454`).
- **The interactive prompt echoes the secret** — `read -r` without `-s` (`bin/cs-secrets:2461`), while `read -r -p` is used elsewhere.
- **`hooks/bash-logger.sh:26-36` logs commands verbatim with zero redaction** (truncated at 200 chars).
- **Typo'd subcommands exit 0.** The `*)` catch-all assigns any unknown token to `SECRET_NAME`, making the `*) error "Unknown command"` arm at `:2507` unreachable dead code; `cs -secrets lst` silently succeeds.
- The encrypted file backend is **AES-256-CBC with no AEAD and no MAC**, keyed by `SHA-256("${USER}@$(hostname)" || salt)` with the salt in the same directory as the ciphertext. `ws`'s AES-256-GCM + Argon2 is strictly better.
- `lib/80-secrets.sh`'s dispatcher has **zero test coverage** — all 119 tests drive `bin/cs-secrets` directly.

### 5.4 Inverted supply-chain verification

`install.sh`'s web path downloads `bin/cs`, `cs-secrets`, both status lines, all 10 hooks, commands, skills, and completions straight from `raw.githubusercontent.com/.../${CS_INSTALL_REF:-main}` with **no checksum and no signature**. The *only* verified artifact is `cs-tui` — the read-only session picker, i.e. the least security-sensitive component. Even that gate is soft: with no `sha256sum`/`shasum` present the comparison is skipped silently, minisign is best-effort, and on failure the binary is removed with a warning while install still exits 0 despite a comment calling it a "hard gate." Meanwhile `release.yml` *does* minisign-sign `install.sh` and every binary — but the shell payload is fetched from a git ref, not from signed release assets, so those signatures protect nothing that gets executed.

`cs -update` is meaningfully stronger (SHA-256 hard gate on `install.sh`, then payload pinned to the immutable tag via `CS_INSTALL_REF`). **The upgrade path is strong; the first-install path is not.** This is the clearest axis on which `ws` can be outright better.

### 5.5 Structural

- **Behaviour enforced by prose, not code.** The queue drain, circuit breakers, rotation protocol, `/wrap` cues, prose lint, and secret-handling discipline all work by asking the model nicely through injected instructions and `block` messages. Non-deterministic and untestable end-to-end: the tests verify the right JSON is emitted, never that the model complied. **This is the strategic opening for a typed implementation.**
- **Deep coupling to undocumented Claude Code internals** across ~15 surfaces — `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE` (hedged with a second guessed name at `lib/75-launch.sh:120-125`), `autoMemoryDirectory`/`plansDirectory`, `--session-id`/`--name`/`--resume` semantics, a reimplemented `~/.claude/projects/<encoded-path>` path encoder (`lib/00-header.sh:115-120`), `/color`'s exact vocabulary, the context-limit UUID fork, statusline stdin shape, 9 hook event names — with **exactly one verifiable upstream citation in shipping code** (`anthropics/claude-code#35148`). The graveyard proves this is not hypothetical: 3 of 12 retirements are "the harness didn't do what we assumed."
- **A skip is counted as a pass** in the test harness (`tests/test_lib.sh:56-62` returns 0 and `run_test` increments `TESTS_PASSED`, with no skip counter), which materially misleads on the 4 Windows shards. 223 `"$CS_BIN" … || true` sites swallow exit codes.
- **CI gaps:** no shellcheck job despite 5 local `# shellcheck disable` directives, no Linux Rust lane despite shipping linux-musl, and **no bash-3.2 matrix** — `macos-latest` resolves to Homebrew bash 5, so the 3.2 path is never exercised. The contract is honoured but unenforced; there is no `BASH_VERSINFO` guard anywhere.
- `stat -c`/`-f` is branched at 5 sites, all on `$OSTYPE == darwin*` rather than `cs_platform()`, contradicting `lib/02-platform.sh:2` and making `CS_PLATFORM_OVERRIDE` unable to exercise the other branch. The *test* harness solves this properly with a `stat --version` capability probe that production never adopted.
- `readlink -f` survives at `lib/80-secrets.sh:17` and `lib/20-update.sh:346` — the exact pattern the project rejected elsewhere for BSD compatibility, in the one fragment with zero coverage.
- **Bus factor 1 at ~1 release/day**, for a tool that rewrites `~/.claude/settings.json`, installs 10 hooks on every session event, and holds secrets. No second reviewer.
- **Feature sprawl.** A session manager that also ships a tmux spawner, cross-session mailbox, task queue with three breakers, rate-limit analyzer, prose linter with a banned-phrase list, voice-cloning corpus builder, OSC-11 background detection through tmux DCS passthrough, and ~1,500 words on statusline divider rendering. Individually well built; collectively one person's workflow shipped as a product, and the parts a second user needs are hard to separate from personal preference.

---

## 6. `ws` weaknesses

### 6.1 Security

**Command injection via `-spawn`.** `spawn.rs:24-28` builds `format!("{ws_bin} {ws_name}")` and `commands_for` (`spawn.rs:44-51`) hands that string to `tmux new-window`, which executes it via `sh -c`. `validate_name` rejects only empty, `/`, `..`, and a leading `-` (`workspace.rs:102-111`) — `;`, `$`, backticks, spaces, and newlines all pass. Worse, **`adopt` never calls `validate_name` at all** (`commands.rs:190-211`), so `ws -adopt 'x;rm -rf ~'` registers that name and `ws -spawn` will execute it. An install path containing a space also breaks. Logged as M2 and deferred; `-spawn` is a v0.2.0 headline feature, so it should not stay deferred.

**M7 violates the project's own doctrine, in the credential path.** `internal.rs:427` maps a JSON *parse* error to `json!({})` and `atomic_write`s it back, discarding every recorded `redacted_secrets` entry — in a file whose comment three lines above says it refuses to do exactly that.

### 6.2 Correctness

- **`lock::acquire` TOCTOU** — `exists()` then `fs::write`, not `O_EXCL` (`lock.rs:90-125`). Two simultaneous `ws proj` invocations can both win. Liveness is checked by shelling out to `kill -0`.
- **`now_iso()` forks `date -u`** on every timestamp (`main.rs:96-104`) — one fork/exec per timeline event, journal line, mail message, and lock acquisition — and `unwrap_or_default()` silently yields an **empty timestamp string** on failure. Same family: `mail::new_id`'s `unwrap_or(0)` (M5).
- **`-limits` can print stale data as current.** It is parasitic on the JSON Claude pipes to the statusline command's stdin (`statusline.rs:27-40`), so there is no data until a Claude session with `ws`'s statusline has run; `stamped_at` is recorded but never checked for age; and Codex usage is never captured because `to_snapshot` hardcodes `agent: "claude"` (`statusline.rs:59`).
- **Names containing `@` are unreachable from the CLI** — the parser claims every such name for the worktree arm (documented at `cli.rs:227-233`). Fails safe but the workspace is inaccessible.
- `search_all` uses the lenient `registry::all()`/`meta::read()`, so a corrupt `workspace.toml` silently makes a workspace searchable-as-unarchived (`search.rs:105-117`) — inconsistent with `rows.rs`'s deliberately strict path.
- **M6 (open):** `merge` never verifies the base still has the worktree's parent branch checked out. Switch `api` to another branch after `ws api@retry`, and `--merge` silently commits into that other branch.
- **M3 (open):** `worktree::create` leaves an orphan branch + worktree on partial failure; every retry bails "already exists" and manual `git worktree remove` is required.

### 6.3 Honesty gaps

These matter more than the bugs at this stage, because they are what a careful evaluator finds first.

- **3 of 10 config keys are inert** — `prompt_on_launch`, `nerd_fonts`, `statusline` are settable and listed but read nowhere. `ws config set statusline false` does nothing, and `ws setup` registers the status line regardless (`commands.rs:91-98`).
- **`ws config set --workspace` is a hard stub** — `bail!("per-workspace config is added in a later task")` (`commands.rs:829`).
- **`-resume` is a parsed no-op** (`cli.rs:257,263`).
- **`ws --help` omits** `-limits`, `-doctor`, `-secrets`, `setup`, `-queue drain --reset`, and **every launch flag** (`-claude`, `-codex`, `--agent`, `--fresh`, `--handoff`, `--force`) — while `README.md:100` says "Run `ws --help` for the complete command summary."
- **README is two releases stale** — still cites the prebuilt `v0.1.0` (`README.md:25`), documents none of the 0.2.0 surface (`-msg`, `-queue`, `-spawn`, `-whoami`, `-who`, `base@feature`), and reduces eight known trust findings to "back up important work." The project's own comparison doc sets an explicit release-language rule at line 144 that the README does not honour.
- **The design doc is unmarked fiction** — `docs/specs/2026-07-24-agent-workspaces-design.md` is still `Status: draft for user review` while documenting Gemini CLI as a first-class agent (removed in `85f355f`), clap-based parsing, `src/doctor.rs`, and four commands that do not exist (`-live`, `-usage`, `-conversations`, `completions`). Two referenced ledger files are absent from the repo.
- **466 unchecked plan checkboxes, 0 checked**, across 11 docs for phases that all shipped — the checkbox state carries no information.

### 6.4 Process and platform

- **85% of the code landed in one squashed initial commit** — 77 files, 27,701 insertions. Most of the codebase has no reviewable incremental history.
- **A 640-line fix wave shipped into v0.2.0 self-described as unreviewed**, touching the Critical path that writes into users' git repositories (`2026-07-26-ws-remaining-work.md:69-72`).
- **`rustfmt` is installed in CI but never run.** There is no `cargo fmt --check` gate, and repo-wide `rustfmt` currently fails.
- **macOS/Apple-Silicon only.** `install.sh:84-89` hard-refuses anything but Darwin/arm64; CI and release are `macos-14` only, so Linux is never built or tested despite `#[cfg(unix)]` throughout. macOS-only shell-outs: `osascript` (`internal.rs:178`), `defaults read -g AppleInterfaceStyle` (`tui/theme.rs:29`), iTerm2 OSC 6 tab colors (`term.rs:26-35`).
- **Private repo.** `-update` requires `gh` and a GitHub login; nobody without an explicit grant can install it.
- **`-uninstall` deletes the running binary** after a filename sanity check (`commands.rs:152-174`).
- **No shell completions, no man page**, despite the design doc promising `ws completions zsh|bash|fish`.
- **Zero user-facing documentation.** All 14 docs (~16,400 lines) are implementation plans or audits; the entire user surface is a 135-line README plus an incomplete `--help`. Nothing documents the `.ws/` contract, and there is no troubleshooting or recovery guide — which M3's "manual `git worktree remove` required" needs.
- **Test gaps:** `orchestration.rs` has 3 tests and all three are refusals — nothing integration-tests a drain that succeeds through a fake agent binary. No CLI-level tests for `-msg`, `-whoami`/`-who`, `-spawn`, `base@feature`, `-update`, or `-uninstall`. `tmux attach` is admitted as never verified live.

---

## 7. Roadmap to parity

"cs level" decomposes into three gaps, and conflating them is how roadmaps become infinite:

1. **Trust gap** — the 8 standing findings plus M1–M8. Pure engineering, a few weeks.
2. **Feature gap** — checkpoints, conversation lineage, usage attribution, completions, doctor depth. Pure engineering, a few weeks.
3. **Maturity gap** — 97 releases, 1,100+ tests, four platforms, and the retirement graveyard. Mostly *not* code. It is the output of eight months of real use. The platform coverage and test mass are buyable; the knowledge of which features are worthless is not, because it came from measuring users.

Ordering below is deliberate: cheapest credibility first, then safety, then the items that take real time.

### Stage 0 — Prove the Codex hook contract · ½ day · **do this first**

Gates every downstream Codex decision. Write a debug handler that dumps raw stdin to a file, register it for all six events, launch Codex, and drive one turn that runs a shell command and edits a file. Answer the five questions in §1.3.

**Acceptance:** a committed test-log doc recording observed `tool_name` values and which events fired. That artifact is worth more than any amount of schema reading.

### Stage 1 — Per-agent matchers · 1 day

Move `matcher` behind the `Agent` trait: `fn tool_matcher(&self, kind: ToolKind) -> Option<&'static str>` with `ToolKind::{Shell, FileWrite}`, returning Claude's names for Claude and Stage 0's observed names for Codex. `install_hooks_for` already takes the agent's config path (`commands.rs:78`), so the change is small.

Then replace `tests/setup.rs:14-16` with per-agent assertions on the *parsed* JSON — event names, matcher values, command paths — so a Claude-shaped matcher in the Codex file fails the build.

**Acceptance:** `ws setup` writes different matchers per agent, proven by test.

### Stage 2 — Truth in advertising · 1 day · highest credibility-per-hour

Delete or implement the 3 inert config keys. Delete `-resume`. Implement or remove `config set --workspace`. Rewrite `print_help` (`main.rs:106-136`) to cover everything, plus a test asserting every command and launch flag appears in help output so it cannot drift again. Refresh the README off `v0.1.0`, document the 0.2.0 surface, and honour the release-language rule at `2026-07-27-cs-vs-ws-comparison.md:144`. Mark the design doc superseded. Add a `docs/README.md` index so a reader can tell which of 14 files is current.

**Acceptance:** every flag in `--help` does something; every README claim is true of the tag it ships on.

### Stage 3 — Security-critical fixes · 2–3 days

1. **Pass argv to tmux instead of a command string.** Removes the shell rather than escaping for it — kills both the injection and the install-path-with-a-space bug. Strictly better than M2's suggested `shell-escape`.
2. **Validate names everywhere.** Move validation into `contract::init` and `registry::register` so no path can register an unvalidated name, and switch `validate_name` to an allowlist. One test per rejected character class.
3. **Close M7** — refuse a corrupt manifest rather than clobbering it.

**Acceptance:** `ws -adopt 'x;touch /tmp/pwned' && ws -spawn …` creates no file; a corrupt `MANIFEST.json` is refused.

### Stage 4 — Interprocess transactions · ~1 week

Hardening Changeset 1, correctly identified as foundational. Its governing rule is right: *an atomic rename is not a transaction; every shared read-modify-write must hold an interprocess lock from the first read through the durable rename.* `atomic.rs` alone does not satisfy it.

Add the `transaction(path, f)` helper over `fs2` advisory locks (already vendored, currently unused). Wrap registry, config, `workspace.toml`, `state.toml`, mail, and queue/timeline appends. Replace `lock::acquire`'s `exists()`-then-write with `OpenOptions::create_new` so the PID record is race-free — this closes the long-deferred TOCTOU. Keep the `mem::forget`-then-`exec` trick; it is correct and load-bearing for lock liveness. Replace the `date -u` fork in `now_iso()` with `chrono` or `jiff`.

**Acceptance:** a concurrency test running N simultaneous mutators against one registry, asserting no lost updates. `cs` has 10 such tests for its secret store using slow-`openssl` shims — that is the bar.

### Stage 5 — Platform reach and distribution · ~1 week

Widest gap, least glamorous.

- **Linux build and CI.** Add Ubuntu jobs to `ci.yml` and `release.yml` for `x86_64`/`aarch64-unknown-linux-gnu`. Gate the macOS-only shell-outs behind `cfg` or capability probes.
- **Make the repo public**, or accept that `ws` cannot be adopted. This is the largest single driver of the visibility gap.
- **Authenticated releases** (Changeset 7). Exploit the asymmetry in §5.4: if `ws` ships checksummed-and-signed assets and an updater that verifies *before* exec, it is ahead of `cs` on the axis that matters most for a tool that rewrites `~/.claude/settings.json`.
- **`ws completions bash|zsh|fish`** — promised by the design doc, present in `cs`, ~1 day hand-rolled.
- **Turn on the gate already installed:** fix repo-wide formatting, add `cargo fmt --check`.

### Stage 6 — Selective feature parity

The self-audit concedes 11 of 20 rows. Not all are worth closing.

**Do:**
- **Conversation lineage** — the biggest genuine hole. `ws` has "latest handoff by mtime" (`handoff.rs:6-21`); `cs` reconstructs an explicit chain from `timeline.jsonl` `started`/`rotated` events with `from`/`to`/`reason`. `ws` already writes a timeline, so adding `rotated` events plus a `ws -conversations` reader is small — and it is what makes agent-switching legible after the fact rather than a guess.
- **Checkpoints** — labelled snapshot of notebook state + git HEAD + dirty files + timeline event. `cs` does it in 280 lines. The feature people ask for after their first bad `--merge`.
- **`-limits` staleness check** — one hour, removes a silent-wrong-answer path.

**Skip:** `cs`'s broader secret backends (`ws`'s AES-GCM + Argon2 is better than unauthenticated AES-CBC) and its prose linter (encodes one author's taste as a turn-blocking gate).

### Stage 7 — Discipline to copy, and one bug not to inherit

**Copy the retirement graveyard.** `ws` currently injects context at `SessionStart` and `UserPromptSubmit` on faith. Instrument it: log what is injected and whether it was used, then delete what does not earn its tokens. Pair it with `cs`'s two-sided enforcement — a write-time guard that strips retired registrations plus a doctor check that catches drift without requiring discipline.

**Do not inherit `cs`'s §5.1 bug.** `ws` has the same architecture — a `<!-- ws:begin -->…<!-- ws:end -->` managed block spliced into `CLAUDE.local.md`/`AGENTS.md` (`context.rs:4-72`). Build the refresh path *now*, while there is only one template version in the wild. `Meta` already has a `contract_version` field; use it to force a re-render when the template changes.

Related: `ws` splices its block into `AGENTS.md` for Codex, which is typically a *committed* project file unlike gitignored `CLAUDE.local.md`, so it will appear in users' diffs on every launch. Either gitignore it or scope the block to a local override.

### Stage 8 — Re-run the comparison

Changeset 9. Same source counts, same test baseline, fresh warm benchmarks, same scoring method. Do not copy the historical benchmark table into a new verdict.

---

## 8. Bottom line

**Stages 0–3 (~2 weeks)** take `ws` from "prototype with one broken headline feature and three untrue claims" to "honest, safe, and does what it says." That is the threshold at which it can be described as a trustworthy cross-agent beta — which the project's own release rule currently forbids.

**Stages 4–6 (~4–6 weeks)** close the trust and feature gaps. At that point `ws` is a legitimate `cs` alternative for Claude and strictly better for anyone using Codex, since `cs` has zero Codex support and never will by construction.

**Stage 7 and the maturity gap are open-ended** and are not closed by writing code faster. They are closed by getting users, which requires Stage 5's public repo.

The strategic read: `ws` does not need to reach `cs`'s level on `cs`'s axes. `cs` is one person's entire workflow shipped as a product, and a meaningful fraction of its surface is personal preference that would be a mistake to chase. What `ws` has that `cs` structurally cannot acquire is **typed enforcement instead of injected prose, and two agents instead of one**. Both are worth finishing.
