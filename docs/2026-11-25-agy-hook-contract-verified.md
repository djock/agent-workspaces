# agy hook contract, as verified (agy 1.5.16, 2026-11-25)

Sources: `agy --help`, the hooks guide embedded in the binary (`strings` on
`~/.local/bin/agy`, the section starting `# Lifecycle Hooks (hooks.json)`), and
three live runs in a scratch directory with a hook that logged every payload.
Code that depends on this: `src/agents/agy.rs`, `hookio::from_agy` /
`hookio::render`, and `hooksetup::agy_entry`.

## Files

- Global hooks: `~/.gemini/config/hooks.json`. Workspace-local:
  `<workspace>/.agents/hooks.json`. Plugins: `~/.gemini/config/plugins/<name>/hooks.json`.
  The guide says the old `~/.gemini/antigravity-cli/hooks.json` path was a bug.
- Top-level keys are **hook names**. A `"hooks"` wrapper is rejected. ws owns
  the entry named `ws`. `"enabled": false` disables a named hook.
- Tool events (`PreToolUse`, `PostToolUse`) take `{matcher, hooks:[handler]}`
  groups, and the matcher is a regex (`*` or empty matches all). Every other
  event takes a flat list of handlers.
- A handler is `{type: "command", command, timeout}`. The command runs via
  `sh -c`, `~` expands, and the working directory is the hooks.json directory.
  The default timeout is 30 s.

## Events

SessionStart, PreToolUse, PostToolUse, PreInvocation, PostInvocation, Stop.
There is no UserPromptSubmit (ws uses PreInvocation, which runs before every
model call) and no SessionEnd.

Order seen live: SessionStart → PreInvocation → PreToolUse/PostToolUse… →
PostInvocation → PreInvocation → PostInvocation → Stop.

## Payload (stdin), camelCase

Common to every event: `conversationId` (equal to the `brain/<id>` directory,
present from SessionStart on), `workspacePaths`, `transcriptPath`,
`artifactDirectoryPath`, `modelName`, `lastUserInput`, `hookEventName`, `cwd`.

- Tool events: `toolCall{id, name, args}` and `stepIdx`. PostToolUse adds
  `error` (empty on success). Tool names seen live: `read_file` (`args.Path`),
  `run_command` (`args.CommandLine`), and `write_file`
  (`args{Path, Content, Overwrite}`, absolute Path). The edit tools
  (`replace_file_content`, `multi_replace_file_content`, `edit_file`,
  `create_file`) are known only from the binary's strings.
- PreInvocation / PostInvocation: `invocationNum`, `initialNumSteps`.
- Stop: `executionNum`, `terminationReason`, `error`, `fullyIdle`,
  `continuationActive` (the loop guard).

## Answers (stdout), JSON

- PreToolUse: `{decision: allow|deny|ask|force_ask, reason?, overwrite?}`.
  **The decision is required: `{}` denied the tool live.** ws registers no
  PreToolUse hook on agy.
- PostToolUse: `{}`.
- SessionStart / PreInvocation: `{injectSteps: [{systemMessage: {systemMessage,
  metadata?}}]}`. `metadata.user_markdown` shows text to the user only.
- Stop: `{decision: "continue", reason}` keeps the agent going. Any other
  decision lets it stop.

## Permissions

A hook's `allow` does not get past agy's own permission layer. In headless
`--mode accept-edits`, `run_command` was still auto-denied.
`--dangerously-skip-permissions` approves everything.

## Not verified yet

- Whether `invocationNum` resets per user turn. ws runs its prompt hook only
  when it is 0. If it never resets, the prompt notes appear only on the first
  prompt.
- Whether SessionStart fires on `--conversation <id>` resume.
- The argument names of the edit tools. ws reads `Path`, `TargetFile`,
  `AbsolutePath` and `FilePath`.
