# Codex hook contract — verified against the shipped binary

**Verified:** 2026-07-27 · **Codex CLI:** 0.145.0 (`aarch64-apple-darwin`) · **ws:** `audit-fixes` off `v0.2.0`

This is the empirical record behind the per-agent hook matchers in
`src/hooksetup.rs` and `src/agents/*.rs`. Every claim here was produced by
running the installed Codex binary and capturing what it actually sent, not by
reading documentation about it.

## Why this document exists

Two published descriptions of Codex's `hooks.json` contradicted each other on the
most basic question — whether event names sit at the file root or under a `hooks`
wrapper — and one named a feature flag (`[features].codex_hooks`) that does not
exist in this build. An audit pass that trusted them reached the **wrong**
conclusion twice: first that ws wrote a structurally invalid file, then that a
required opt-in flag was missing. Both were false.

**Rule going forward: for agent-harness contracts, probe the binary.** It is
faster than reading about it and it is the only source that cannot be stale.

```bash
codex features list | grep -w hooks
CB="$(readlink -f "$(which codex)")"
strings "$CB" | grep -E "HookSpecificOutputWire|additionalContext|permissionDecision"
strings "$CB" | grep -xE "SessionStart|SessionEnd|PreToolUse|PostToolUse|UserPromptSubmit|Stop|SubagentStart|SubagentStop|PreCompact|PostCompact|PermissionRequest"
```

## How the live capture was done

`~/.codex/hooks.json` did not exist on the test machine. The probe installed one
temporarily — with a trap removing it on every exit path — registered a handler
that dumped raw stdin per event, and never touched `~/.codex/config.toml`.

```sh
codex exec --dangerously-bypass-hook-trust --skip-git-repo-check \
           --sandbox workspace-write -C "$WORKSPACE" --color never \
           'run the shell command `echo probe-marker`; create probe.txt containing hello'
```

The registered `hooks.json` used **exactly the shape `ws setup` writes** — a
`hooks` wrapper, per-event arrays of matcher-groups, each holding a `hooks` list
of `{type, command, timeout}` — with the matchers omitted so every tool call was
observable.

## Findings

### 1. Hooks are on by default; there is no flag to write

```
$ codex features list | grep -w hooks
hooks                                stable             true
```

`hooks` is a **stable** feature, default **true**. The `codex_hooks` name from
one published source does not appear in this build. A separate `plugin_hooks`
feature exists and is unrelated. **ws needs to write no feature flag.**

### 2. The Claude-shaped `hooks.json` loads, and every registered event fires

All six events ws registers fired, in this order, for a single turn containing
one shell call and one file edit:

```
SessionStart → UserPromptSubmit → PreToolUse → PostToolUse → PreToolUse → PostToolUse → Stop → SessionEnd
```

So the outer schema, the matcher-group shape, and the `{type, command, timeout}`
entry shape are all accepted by Codex. `SessionEnd` **does** exist, contrary to
an earlier note in this repo's history. Codex has no `PostToolUseFailure`.

Events present in the binary: `SessionStart`, `SessionEnd`, `PreToolUse`,
`PostToolUse`, `UserPromptSubmit`, `Stop`, `SubagentStart`, `SubagentStop`,
`PreCompact`, `PostCompact`, `PermissionRequest`.

### 3. Codex mirrors Claude's wire format deliberately

The binary embeds JSON schemas named `SessionStartHookSpecificOutputWire`,
`UserPromptSubmitHookSpecificOutputWire`, `PreToolUseHookSpecificOutputWire`,
`PostToolUseHookSpecificOutputWire`, `PermissionRequestHookSpecificOutputWire`
and `SubagentStartHookSpecificOutputWire`, alongside the literal field names
`hookSpecificOutput`, `additionalContext`, `permissionDecision` and
`permissionDecisionReason`.

`src/hookio.rs` already emits `hookSpecificOutput.additionalContext`, so **one
handler set for both agents is the right design**, not a shortcut.

### 4. Payload field names are identical to Claude's

Captured `PreToolUse` payload, verbatim:

```json
{
  "session_id": "019fa430-273a-7fa2-a329-89fab081f383",
  "turn_id": "019fa430-27bb-79e2-bc60-3b3ab17b96b4",
  "transcript_path": "/Users/…/.codex/sessions/2026/07/27/rollout-….jsonl",
  "cwd": "/…/workspace",
  "hook_event_name": "PreToolUse",
  "model": "gpt-5.6-sol",
  "permission_mode": "bypassPermissions",
  "tool_name": "Bash",
  "tool_input": { "command": "echo probe-marker" },
  "tool_use_id": "exec-e873c90b-8887-4f4e-9bee-615718cb6684"
}
```

Per-event extras: `SessionStart` adds `source` (observed: `"startup"`), which is
what `internal::session_start` filters on. `Stop` adds `stop_hook_active` and
`last_assistant_message`. `SessionEnd` adds `reason`. `PostToolUse` adds
`tool_response`. Every field `hookio::HookInput` deserializes is present under
the same name, so no parsing change was needed.

### 5. The one real gap: tool names differ for file writes

| Tool | Claude `tool_name` | Codex `tool_name` |
|---|---|---|
| shell | `Bash` | **`Bash`** — identical |
| file write | `Write` / `Edit` | **`apply_patch`** |

Codex has no `Bash` *tool* internally (its feature strings are `shell_tool`,
`unified_exec`) yet it reports `tool_name: "Bash"` to hooks — so ws's
`matcher: "Bash"` for the bash-audit hook was already correct.

The `PostToolUse` matcher was not. `"Write|Edit"` cannot match `apply_patch`, so
**secret redaction never fired on Codex** — the one hook whose entire purpose is
keeping credentials out of files.

### 6. `apply_patch` carries no `file_path`

```json
{
  "tool_name": "apply_patch",
  "tool_input": {
    "command": "*** Begin Patch\n*** Add File: /abs/probe.txt\n+hello\n*** End Patch"
  }
}
```

`tool_input.file_path` is **absent**. The target is inside a patch envelope in
`tool_input.command`, and one patch may name several files. So fixing the matcher
alone was not enough: the handler had to learn to read the envelope.

Envelope headers that matter: `*** Add File:`, `*** Update File:`,
`*** Move to:` (all produce content to scan) and `*** Delete File:` (skipped —
nothing left to read). Paths may be relative, resolved against the payload's
`cwd`.

For reference, `apply_patch`'s `tool_response` also lists the written files
(`Success. Updated the following files:\nA /abs/path`), but parsing `tool_input`
is preferred: it is available at `PreToolUse` time too and does not depend on a
human-readable success message.

## What changed in ws as a result

| Change | Location |
|---|---|
| `HookSpec.matcher` → `HookSpec.scope` (`Always` / `Tool(ToolKind)`) | `src/hooksetup.rs` |
| `Agent::tool_matcher(ToolKind)` added, **no default impl** | `src/agents/mod.rs` |
| Claude: `Shell → "Bash"`, `FileWrite → "Write|Edit"` | `src/agents/claude.rs` |
| Codex: `Shell → "Bash"`, `FileWrite → "Write|Edit\|apply_patch"` | `src/agents/codex.rs` |
| `written_paths()` parses the `apply_patch` envelope; `secret_redact` loops over every target | `src/internal.rs` |
| `tests/setup.rs` asserts the **parsed** document and the per-agent matcher | `tests/setup.rs` |
| End-to-end redaction tests driven by real Codex payloads | `tests/redact.rs` |

`Write|Edit` is kept in Codex's alternation rather than replaced: it costs
nothing and means a future Codex file-write tool does not silently kill the hook
again.

## Still not verified

- Whether an **untrusted** hook silently no-ops. The capture used
  `--dangerously-bypass-hook-trust`. `CodexAgent::hook_trust_note` tells users to
  run `/hooks`, which appears correct, but the untrusted failure mode is
  uncharacterised — a user who skips `/hooks` may get silence rather than a
  warning.
- Whether `additionalContext` returned from `SessionStart` actually reaches the
  model on Codex. The event fires and the schema exists; the probe handler
  returned no output, so injection was never exercised end to end.
- `commandWindows` and `statusMessage` per-entry fields appear in published
  descriptions and were not exercised.
- Behaviour of `PermissionRequest`, `SubagentStart`/`SubagentStop`,
  `PreCompact`/`PostCompact` — ws registers none of them.
- Only one Codex version was tested. This contract is not covered by ws's CI, so
  a Codex upgrade can break it silently. A periodic re-run of the probe above is
  the cheapest guard.
