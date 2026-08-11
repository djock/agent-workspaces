use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table};

pub const CODEX_STATUS_LINE: &[&str] =
    &["model-with-reasoning", "git-branch", "context-used", "five-hour-limit", "weekly-limit"];

#[derive(Debug, Default, Deserialize, Serialize)]
struct CodexStatuslineBackup {
    status_line: Option<Vec<String>>,
    status_line_use_colors: Option<bool>,
}

/// Which tool a hook is scoped to, resolved to a concrete matcher per agent.
///
/// Hook matchers are regexes over the payload's `tool_name`, and the two agents
/// do not name the same tools. Verified empirically against Codex CLI 0.145.0
/// (see `docs/2026-07-27-codex-hook-contract-verified.md`): Codex reports
/// `tool_name: "Bash"` for shell calls — same as Claude — but `"apply_patch"`
/// for file edits, where Claude reports `"Write"`/`"Edit"`.
///
/// Hardcoding Claude's names in a shared const is what silently disabled secret
/// redaction on Codex: `matcher: "Write|Edit"` can never match `apply_patch`, so
/// the one hook that keeps credentials out of files never fired for Codex users.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Shell,
    FileWrite,
}

/// A hook's scope: every tool call, or only one kind of tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Always,
    Tool(ToolKind),
}

pub struct HookSpec {
    pub event: &'static str,
    pub scope: Scope,
    pub handler: &'static str,
    pub script: &'static str,
}

/// Every hook event ws will register, for validating `hooks.toml`.
///
/// Not every agent fires every one of these — Codex has no `PostToolUseFailure` —
/// so support is asked per agent via `Agent::supports_event`. This list is the
/// vocabulary; the agent decides what it can honour.
pub const KNOWN_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "PermissionRequest",
    "PreCompact",
    "PostCompact",
];

pub fn is_known_event(e: &str) -> bool {
    KNOWN_EVENTS.contains(&e)
}

pub const HOOKS: &[HookSpec] = &[
    HookSpec {
        event: "SessionStart",
        scope: Scope::Always,
        handler: "session-start",
        script: "session-start.sh",
    },
    HookSpec {
        event: "UserPromptSubmit",
        scope: Scope::Always,
        handler: "user-prompt",
        script: "user-prompt.sh",
    },
    HookSpec { event: "Stop", scope: Scope::Always, handler: "stop", script: "stop.sh" },
    HookSpec {
        event: "SessionEnd",
        scope: Scope::Always,
        handler: "session-end",
        script: "session-end.sh",
    },
    HookSpec {
        event: "PostToolUse",
        scope: Scope::Tool(ToolKind::FileWrite),
        handler: "secret-redact",
        script: "secret-redact.sh",
    },
];

pub fn hooks_dir() -> PathBuf {
    crate::config::ws_config_dir().join("hooks")
}

pub fn claude_settings_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".claude").join("settings.json")
}

pub fn codex_hooks_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".codex").join("hooks.json")
}

pub fn codex_config_path() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".codex").join("config.toml")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_command(path: &Path) -> String {
    shell_quote(&path.to_string_lossy())
}

/// Shim filename for a user hook.
pub fn user_script_name(slug: &str) -> String {
    format!("user-{slug}.sh")
}

/// A user hook runs through a ws-owned shim rather than being registered
/// directly. Two reasons, both load-bearing: the shim lives under ws's hooks
/// directory, so `group_is_ws` recognises and *replaces* it on the next `setup`
/// (a bare user command would look foreign and be duplicated on every run); and
/// the user's command inherits ws's launch env (`WS_WORKSPACE`, `WS_DIR`,
/// `WS_ROOT`, `WS_AGENT`) and receives the hook payload on stdin unchanged.
fn render_user_shim(command: &Path) -> String {
    format!(
        "#!/bin/sh\n# ws hook \u{2014} user-defined (hooks.toml). Payload arrives on stdin.\nexec {} \"$@\"\n",
        shell_command(command)
    )
}

fn render_shim(ws_bin: &Path, handler: &str) -> String {
    format!(
        "#!/bin/sh\n# ws hook — thin shim (no jq/python); ws does the work.\nexec {} internal {}\n",
        shell_command(ws_bin),
        handler
    )
}

/// Materialize the shared hook shims once, then register them into `config_path`
/// (any JSON file with a top-level `hooks` object — settings.json or hooks.json).
///
/// `agent` supplies the per-agent `tool_name` matchers. It is a parameter rather
/// than a constant because Claude and Codex name their tools differently, and
/// baking one agent's names in is what disabled Codex secret redaction.
pub fn install_hooks_for(
    config_path: &Path,
    ws_bin: &Path,
    agent: &dyn crate::agents::Agent,
) -> Result<usize> {
    let dir = hooks_dir();
    std::fs::create_dir_all(&dir)?;

    // atomic_write, not fs::write: a hook firing during `setup` `exec`s this
    // very file, and a plain write can be observed truncated.
    for spec in HOOKS {
        crate::atomic::atomic_write_with_mode(
            &dir.join(spec.script),
            render_shim(ws_bin, spec.handler),
            Some(0o755),
        )?;
    }

    let user = crate::hooks_user::load()?;
    let (applies, skipped) = crate::hooks_user::for_agent(&user, agent);
    for h in &applies {
        crate::atomic::atomic_write_with_mode(
            &dir.join(user_script_name(&h.slug)),
            render_user_shim(&h.command),
            Some(0o755),
        )?;
    }
    for (h, agent_id) in skipped {
        eprintln!(
            "ws: note: user hook {} on {} is not registered for {agent_id} — that agent has no such event",
            h.command.display(),
            h.event
        );
    }

    register_settings(config_path, &dir, agent, &applies)?;
    Ok(HOOKS.len() + applies.len())
}

/// Merge ws's hook groups into `settings_path`.
///
/// Wrapped in `txn::transaction`: this file is `~/.claude/settings.json` or
/// `~/.codex/hooks.json` — the user's own agent configuration, shared with the
/// agents themselves. Two concurrent `ws setup`, or a `setup` racing
/// `-uninstall` (or `-update`, which runs setup), each read the same starting
/// document and renamed their own result into place, so one side's registration
/// vanished silently. An atomic rename makes a write all-or-nothing; it does not
/// make a read-modify-write a transaction.
fn register_settings(
    settings_path: &Path,
    hooks_dir: &Path,
    agent: &dyn crate::agents::Agent,
    user: &[&crate::hooks_user::UserHook],
) -> Result<()> {
    crate::txn::transaction(settings_path, || {
        register_settings_locked(settings_path, hooks_dir, agent, user)
    })
}

fn register_settings_locked(
    settings_path: &Path,
    hooks_dir: &Path,
    agent: &dyn crate::agents::Agent,
    user: &[&crate::hooks_user::UserHook],
) -> Result<()> {
    // Absent → start fresh; unreadable (permission error, I/O error) → refuse.
    // Defaulting on an unreadable file would write the new hooks back over an
    // empty object, clobbering every other key settings.json already had.
    let mut root: Value = match std::fs::read_to_string(settings_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); refusing to overwrite it. \
                 Fix it or move it aside, then re-run `ws setup`.",
                settings_path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).context(format!("failed to read {}", settings_path.display())),
    };
    if !root.is_object() {
        anyhow::bail!(
            "{} is not a JSON object; refusing to overwrite it.",
            settings_path.display()
        );
    }

    let obj = root.as_object_mut().unwrap();
    let hooks_entry = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks_entry.is_object() {
        *hooks_entry = json!({});
    }
    let hooks_obj = hooks_entry.as_object_mut().unwrap();

    // Build every group ws wants first, keyed by event, then apply per event.
    //
    // Applying built-ins and user hooks in two passes was wrong in both
    // directions: the second pass's "drop stale ws entries" retain deleted the
    // group the first pass had *just added* for the same event, so a built-in and
    // a user hook on one event could not coexist — the built-in silently vanished.
    // One retain per event, then push everything for it.
    type Group = serde_json::Map<String, Value>;
    let mut wanted: Vec<(String, Group)> = Vec::new();

    for spec in HOOKS {
        let mut group = Group::new();
        if let Scope::Tool(kind) = spec.scope {
            group.insert("matcher".into(), json!(agent.tool_matcher(kind)));
        }
        group.insert(
            "hooks".into(),
            json!([{
                "type": "command",
                "command": shell_command(&hooks_dir.join(spec.script)),
                "timeout": 10,
            }]),
        );
        wanted.push((spec.event.to_string(), group));
    }

    // User hooks, resolved through the same per-agent matcher as the built-ins:
    // one `tool = "file-write"` declaration becomes Claude's
    // `Write|Edit|MultiEdit|NotebookEdit` and Codex's `Write|Edit|apply_patch`
    // without the user knowing either vocabulary.
    for h in user {
        let mut group = Group::new();
        if let Scope::Tool(kind) = h.scope {
            group.insert("matcher".into(), json!(agent.tool_matcher(kind)));
        }
        group.insert(
            "hooks".into(),
            json!([{
                "type": "command",
                "command": shell_command(&hooks_dir.join(user_script_name(&h.slug))),
                "timeout": h.timeout,
            }]),
        );
        wanted.push((h.event.clone(), group));
    }

    let events: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for (e, _) in &wanted {
            if !seen.contains(e) {
                seen.push(e.clone());
            }
        }
        seen
    };

    for event in events {
        let arr_entry = hooks_obj.entry(event.clone()).or_insert_with(|| json!([]));
        if !arr_entry.is_array() {
            *arr_entry = json!([]);
        }
        let arr = arr_entry.as_array_mut().unwrap();
        // Drop ws's own entries (command under our hooks dir); keep everything
        // else, so a hook the user wired in by hand survives.
        arr.retain(|group| !group_is_ws(group, hooks_dir));
        for (e, group) in &wanted {
            if *e == event {
                arr.push(Value::Object(group.clone()));
            }
        }
    }

    crate::atomic::atomic_write(settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

/// Register `ws statusline` in settings.json, recording
/// any pre-existing command into <ws_config_dir>/statusline-backup.json first.
/// Preserves all other settings.json keys; refuses to overwrite an unparseable file.
pub fn register_statuslines(ws_bin: &Path) -> Result<()> {
    let settings_path = claude_settings_path();
    // Absent → start fresh; unreadable → refuse (see register_settings above).
    let mut root: Value = match std::fs::read_to_string(&settings_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); refusing to overwrite it. \
                 Fix it or move it aside, then re-run `ws setup`.",
                settings_path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).context(format!("failed to read {}", settings_path.display())),
    };
    if !root.is_object() {
        anyhow::bail!(
            "{} is not a JSON object; refusing to overwrite it.",
            settings_path.display()
        );
    }

    // back up any prior commands (so cs-statusline is recoverable)
    let mut backup = serde_json::Map::new();
    let ws_prefix = format!("{} ", ws_bin.display());
    let quoted_ws_prefix = format!("{} ", shell_command(ws_bin));
    for key in ["statusLine"] {
        if let Some(cmd) = root.get(key).and_then(|v| v.get("command")).and_then(|c| c.as_str()) {
            if !cmd.starts_with(&ws_prefix) && !cmd.starts_with(&quoted_ws_prefix) {
                backup.insert(key.to_string(), json!(cmd));
            }
        }
    }
    // merge into any existing backup so a prior original is never lost
    let bpath = crate::config::ws_config_dir().join("statusline-backup.json");
    // Absent → nothing backed up yet; unreadable → refuse. Defaulting to an
    // empty map on a read error would overwrite the backup with one missing
    // whatever original command it had already preserved — the one thing
    // this file exists to protect.
    let mut existing: serde_json::Map<String, Value> = match std::fs::read_to_string(&bpath) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("{} is corrupt (refusing to overwrite)", bpath.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", bpath.display())),
    };
    for (k, v) in backup {
        existing.entry(k).or_insert(v);
    }
    if !existing.is_empty() {
        crate::atomic::atomic_write(
            &bpath,
            serde_json::to_string_pretty(&Value::Object(existing))?,
        )?;
    }

    let obj = root.as_object_mut().unwrap();
    obj.insert(
        "statusLine".into(),
        json!({ "type": "command", "command": format!("{} statusline", shell_command(ws_bin)), "refreshInterval": 1 }),
    );

    crate::atomic::atomic_write(&settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn codex_statusline_backup_path() -> PathBuf {
    crate::config::ws_config_dir().join("codex-statusline-backup.toml")
}

fn codex_tui(doc: &DocumentMut) -> Result<Option<&Table>> {
    match doc.get("tui") {
        None => Ok(None),
        Some(item) => item
            .as_table()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("Codex config key `tui` is not a table")),
    }
}

fn codex_status_line_of(tui: Option<&Table>) -> Result<Option<Vec<String>>> {
    let Some(item) = tui.and_then(|table| table.get("status_line")) else {
        return Ok(None);
    };
    let array = item
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Codex config key `tui.status_line` is not an array"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("Codex `tui.status_line` entries must be strings"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn codex_status_line_colors_of(tui: Option<&Table>) -> Result<Option<bool>> {
    let Some(item) = tui.and_then(|table| table.get("status_line_use_colors")) else {
        return Ok(None);
    };
    item.as_bool().map(Some).ok_or_else(|| {
        anyhow::anyhow!("Codex config key `tui.status_line_use_colors` is not a boolean")
    })
}

fn codex_tui_mut(doc: &mut DocumentMut) -> Result<&mut Table> {
    if !doc.contains_key("tui") {
        doc.insert("tui", Item::Table(Table::new()));
    }
    doc.get_mut("tui")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("Codex config key `tui` is not a table"))
}

fn codex_status_line_array(values: &[&str]) -> Array {
    let mut array = Array::new();
    for value in values {
        array.push(*value);
    }
    array
}

/// Configure Codex's native footer with the same information rendered by the
/// Claude status-line command. The original Codex footer is backed up once and
/// restored by uninstall. toml_edit preserves unrelated comments and layout.
pub fn register_codex_statusline() -> Result<()> {
    let path = codex_config_path();
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
    };
    let mut doc = source.parse::<DocumentMut>().map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid TOML ({e}); refusing to overwrite it. \
             Fix it or move it aside, then re-run `ws setup`.",
            path.display()
        )
    })?;

    let backup_path = codex_statusline_backup_path();
    match std::fs::read_to_string(&backup_path) {
        Ok(source) => {
            toml::from_str::<CodexStatuslineBackup>(&source).with_context(|| {
                format!("{} is corrupt (refusing to overwrite)", backup_path.display())
            })?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let tui = codex_tui(&doc)?;
            let backup = CodexStatuslineBackup {
                status_line: codex_status_line_of(tui)?,
                status_line_use_colors: codex_status_line_colors_of(tui)?,
            };
            crate::atomic::atomic_write(&backup_path, toml::to_string(&backup)?)?;
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", backup_path.display()))
        }
    }

    let tui = codex_tui_mut(&mut doc)?;
    tui.insert("status_line", toml_edit::value(codex_status_line_array(CODEX_STATUS_LINE)));
    tui.insert("status_line_use_colors", toml_edit::value(true));
    crate::atomic::atomic_write(&path, doc.to_string())?;
    Ok(())
}

/// Restore the Codex footer that existed before ws setup. If the user changed
/// the footer after setup, leave their newer value and the backup untouched.
pub fn unregister_codex_statusline() -> Result<usize> {
    let path = codex_config_path();
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
    };
    let mut doc = source.parse::<DocumentMut>().map_err(|e| {
        anyhow::anyhow!("{} is not valid TOML ({e}); refusing to modify it.", path.display())
    })?;
    let current = codex_status_line_of(codex_tui(&doc)?)?;
    let owned = CODEX_STATUS_LINE.iter().map(|value| value.to_string()).collect::<Vec<_>>();
    if current.as_ref() != Some(&owned) {
        return Ok(0);
    }

    let backup_path = codex_statusline_backup_path();
    let backup: CodexStatuslineBackup = match std::fs::read_to_string(&backup_path) {
        Ok(source) => toml::from_str(&source).with_context(|| {
            format!("{} is corrupt (refusing to modify)", backup_path.display())
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", backup_path.display()))
        }
    };

    let tui = codex_tui_mut(&mut doc)?;
    match backup.status_line {
        Some(values) => {
            let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
            tui.insert("status_line", toml_edit::value(codex_status_line_array(&refs)));
        }
        None => {
            tui.remove("status_line");
        }
    }
    match backup.status_line_use_colors {
        Some(value) => {
            tui.insert("status_line_use_colors", toml_edit::value(value));
        }
        None => {
            tui.remove("status_line_use_colors");
        }
    }
    crate::atomic::atomic_write(&path, doc.to_string())?;
    std::fs::remove_file(&backup_path)?;
    Ok(1)
}

/// Is this hook group one ws installed?
///
/// Anything whose command lives in ws's hooks directory, rather than only the
/// scripts currently in `HOOKS`. Matching against `HOOKS` alone had two bugs: a
/// user-hook shim (`user-<slug>.sh`) was not in that list, so it read as *foreign*
/// and every `ws setup` appended another copy of it; and a built-in that was
/// renamed or removed left its stale registration behind forever, pointing at a
/// script that no longer exists.
fn group_is_ws(group: &Value, hooks_dir: &Path) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|command| command_is_in(command, hooks_dir))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Does this registered command point at a file inside `dir`?
///
/// Compared on **path components**, not as a string prefix. A foreign hook at
/// `<hooks_dir>-legacy/foo.sh` shares the textual prefix of the hooks directory
/// while living somewhere else entirely, and a `starts_with`/`contains` test on
/// the raw strings deletes it.
fn command_is_in(command: &str, dir: &Path) -> bool {
    // Registered commands are shell-quoted by `shell_command`.
    let unquoted = command.trim().trim_matches('\'');
    Path::new(unquoted).starts_with(dir)
}

/// Remove ws-owned hook groups while preserving every unrelated setting and
/// hook group in the same JSON file.
pub fn unregister_hooks_for(config_path: &Path) -> Result<usize> {
    let mut root: Value = match std::fs::read_to_string(config_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); refusing to modify it.",
                config_path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).context(format!("failed to read {}", config_path.display())),
    };
    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object; refusing to modify it.", config_path.display());
    }

    let dir = hooks_dir();
    let mut removed = 0;
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        for groups in hooks.values_mut() {
            if let Some(groups) = groups.as_array_mut() {
                let before = groups.len();
                groups.retain(|group| !group_is_ws(group, &dir));
                removed += before - groups.len();
            }
        }
        hooks.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));
    }
    if root.get("hooks").and_then(Value::as_object).is_some_and(|hooks| hooks.is_empty()) {
        root.as_object_mut().unwrap().remove("hooks");
    }

    if removed > 0 {
        crate::atomic::atomic_write(config_path, serde_json::to_string_pretty(&root)?)?;
    }
    Ok(removed)
}

/// Restore status-line commands that ws replaced, or remove the ws entries
/// when there was no prior command.
pub fn unregister_statuslines(ws_bin: &Path) -> Result<usize> {
    let settings_path = claude_settings_path();
    let mut root: Value = match std::fs::read_to_string(&settings_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid JSON ({e}); refusing to modify it.",
                settings_path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).context(format!("failed to read {}", settings_path.display())),
    };
    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object; refusing to modify it.", settings_path.display());
    }

    let owned: Vec<&str> = ["statusLine", "subagentStatusLine"]
        .into_iter()
        .filter(|key| {
            let suffix = if *key == "statusLine" { "statusline" } else { "subagent-statusline" };
            let expected = format!("{} {suffix}", ws_bin.display());
            let quoted_expected = format!("{} {suffix}", shell_command(ws_bin));
            root.get(*key)
                .and_then(|v| v.get("command"))
                .and_then(Value::as_str)
                .is_some_and(|command| command == expected || command == quoted_expected)
        })
        .collect();
    if owned.is_empty() {
        return Ok(0);
    }

    let backup_path = crate::config::ws_config_dir().join("statusline-backup.json");
    let mut backup: serde_json::Map<String, Value> = match std::fs::read_to_string(&backup_path) {
        Ok(s) => serde_json::from_str(&s).with_context(|| {
            format!("{} is corrupt (refusing to overwrite)", backup_path.display())
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", backup_path.display()))
        }
    };

    for key in &owned {
        match backup.remove(*key).and_then(|v| v.as_str().map(str::to_string)) {
            Some(command) => {
                root.as_object_mut()
                    .unwrap()
                    .insert((*key).to_string(), json!({ "type": "command", "command": command }));
            }
            None => {
                root.as_object_mut().unwrap().remove(*key);
            }
        }
    }
    crate::atomic::atomic_write(&settings_path, serde_json::to_string_pretty(&root)?)?;

    if backup.is_empty() {
        match std::fs::remove_file(&backup_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    } else {
        crate::atomic::atomic_write(
            &backup_path,
            serde_json::to_string_pretty(&Value::Object(backup))?,
        )?;
    }
    Ok(owned.len())
}

/// Remove only the hook scripts whose filenames are owned by ws.
pub fn remove_hook_scripts() -> Result<usize> {
    let dir = hooks_dir();
    let mut removed = 0;
    for spec in HOOKS {
        match std::fs::remove_file(dir.join(spec.script)) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    // User-hook shims too, or `-uninstall` leaves executables behind in a
    // directory it just told the user it had cleaned out. Derived from the same
    // `hooks.toml` that created them; an unreadable one is not fatal here — there
    // is nothing left to register — it only means those shims stay.
    if let Ok(user) = crate::hooks_user::load() {
        for h in &user {
            match std::fs::remove_file(dir.join(user_script_name(&h.slug))) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    // Try the removal rather than testing emptiness first: `read_dir().next()`
    // then `remove_dir` is a race, and a directory holding something ws did not
    // put there is not ws's to delete.
    if dir.is_dir() {
        match std::fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // "Directory not empty" is reported with different errnos across
            // platforms, so confirm by looking rather than by matching a kind.
            Err(e) => {
                let non_empty =
                    std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false);
                if !non_empty {
                    return Err(e.into());
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude::ClaudeAgent;
    use crate::agents::codex::CodexAgent;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // The permission-mutating tests below chmod a real file on disk and touch
    // process-global HOME/XDG_CONFIG_HOME; `.cargo/config.toml` pins
    // RUST_TEST_THREADS=1 today, but serialize explicitly rather than lean on
    // that project-wide default (see registry.rs's TEST_LOCK for the same call).
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn iso() -> TempDir {
        let d = TempDir::new().unwrap();
        std::env::set_var("HOME", d.path());
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        d
    }

    /// The whole reason `tool_matcher` is on the `Agent` trait instead of in the
    /// shared `HOOKS` const. Verified against Codex CLI 0.145.0: both agents
    /// report a shell call as `Bash`, but a file edit is `Write`/`Edit` on Claude
    /// and `apply_patch` on Codex. When one const carried Claude's names, the
    /// Codex secret-redaction hook could never match and never ran.
    #[test]
    fn the_two_agents_resolve_different_file_write_matchers() {
        use crate::agents::Agent;
        assert_eq!(ClaudeAgent.tool_matcher(ToolKind::Shell), "Bash");
        assert_eq!(CodexAgent.tool_matcher(ToolKind::Shell), "Bash", "shell agrees");

        let claude = ClaudeAgent.tool_matcher(ToolKind::FileWrite);
        let codex = CodexAgent.tool_matcher(ToolKind::FileWrite);
        assert_ne!(claude, codex, "file-write matchers must differ or Codex is broken again");
        assert!(!claude.contains("apply_patch"), "Claude has no apply_patch tool: {claude}");
        assert!(codex.contains("apply_patch"), "Codex edits arrive as apply_patch: {codex}");
    }

    #[test]
    fn install_writes_scripts_and_registers_hooks() {
        let _d = iso();
        let ws_bin = std::path::Path::new("/opt/ws/ws");
        let n = install_hooks_for(&claude_settings_path(), ws_bin, &ClaudeAgent).unwrap();
        assert_eq!(n, HOOKS.len());

        // scripts exist and are executable, referencing the bin
        let s = std::fs::read_to_string(hooks_dir().join("session-start.sh")).unwrap();
        assert!(s.contains("/opt/ws/ws"));
        assert!(s.contains("internal session-start"));

        // settings.json has our hooks under the right events
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_settings_path()).unwrap())
                .unwrap();
        let ss = &settings["hooks"]["SessionStart"];
        assert!(ss.is_array());
        let cmd = ss[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("session-start.sh"));
        assert!(cmd.starts_with('\'') && cmd.ends_with('\''));
        // PostToolUse is the one built-in with a tool matcher (secret redaction);
        // PreToolUse has none since the write-only bash audit hook was removed.
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["matcher"],
            "Write|Edit|MultiEdit|NotebookEdit"
        );
        assert!(settings["hooks"]["PreToolUse"].is_null(), "no built-in PreToolUse hook");
    }

    #[test]
    fn install_is_idempotent_and_preserves_foreign_hooks() {
        let _d = iso();
        // pre-existing foreign (cs-style) hook must survive
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        std::fs::write(&sp, r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"~/.claude/hooks/cs/session-start.sh"}]}]}}"#).unwrap();

        install_hooks_for(
            &claude_settings_path(),
            std::path::Path::new("/opt/ws/ws"),
            &ClaudeAgent,
        )
        .unwrap();
        install_hooks_for(
            &claude_settings_path(),
            std::path::Path::new("/opt/ws/ws"),
            &ClaudeAgent,
        )
        .unwrap(); // twice

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
        let arr = settings["hooks"]["SessionStart"].as_array().unwrap();
        // exactly one foreign + exactly one ws entry (idempotent)
        let foreign = arr
            .iter()
            .filter(|g| g["hooks"][0]["command"].as_str().unwrap().contains("/cs/"))
            .count();
        let ours = arr
            .iter()
            .filter(|g| {
                g["hooks"][0]["command"].as_str().unwrap().contains("session-start.sh")
                    && !g["hooks"][0]["command"].as_str().unwrap().contains("/cs/")
            })
            .count();
        assert_eq!(foreign, 1);
        assert_eq!(ours, 1);
    }

    #[test]
    #[cfg(unix)]
    fn commands_with_spaces_are_quoted_executable_and_idempotent() {
        use std::os::unix::fs::PermissionsExt;

        let d = TempDir::new().unwrap();
        std::env::set_var("HOME", d.path());
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("Application Support"));
        let ws_bin = d.path().join("bin with spaces/ws");
        std::fs::create_dir_all(ws_bin.parent().unwrap()).unwrap();
        std::fs::write(&ws_bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&ws_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        install_hooks_for(&claude_settings_path(), &ws_bin, &ClaudeAgent).unwrap();
        install_hooks_for(&claude_settings_path(), &ws_bin, &ClaudeAgent).unwrap();

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(claude_settings_path()).unwrap())
                .unwrap();
        let groups = settings["hooks"]["SessionEnd"].as_array().unwrap();
        assert_eq!(
            groups.len(),
            1,
            "re-running setup must replace, not duplicate, the quoted hook"
        );
        let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(command.starts_with('\'') && command.ends_with('\''));
        assert!(std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn unregister_removes_only_ws_hooks() {
        let _d = iso();
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        std::fs::write(&sp, r#"{"other":"keep","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/opt/other/start.sh"}]}]}}"#).unwrap();
        install_hooks_for(&sp, std::path::Path::new("/opt/ws/ws"), &ClaudeAgent).unwrap();

        let removed = unregister_hooks_for(&sp).unwrap();

        assert_eq!(removed, HOOKS.len());
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
        assert_eq!(settings["other"], "keep");
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["command"], "/opt/other/start.sh");
    }

    #[test]
    fn unregister_statuslines_restores_the_previous_command() {
        let _d = iso();
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        std::fs::write(
            &sp,
            r#"{"statusLine":{"type":"command","command":"/opt/my-status"},"other":"keep"}"#,
        )
        .unwrap();
        let ws_bin = std::path::Path::new("/opt/ws/ws");
        register_statuslines(ws_bin).unwrap();

        let removed = unregister_statuslines(ws_bin).unwrap();

        // One key now, not two: the subagent status line was removed.
        assert_eq!(removed, 1);
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
        assert_eq!(settings["statusLine"]["command"], "/opt/my-status");
        assert!(settings.get("subagentStatusLine").is_none());
        assert_eq!(settings["other"], "keep");
        assert!(!crate::config::ws_config_dir().join("statusline-backup.json").exists());
    }

    #[test]
    fn codex_statusline_preserves_config_comments_and_backs_up_the_original() {
        let _d = iso();
        let path = codex_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "# keep this comment\nmodel = \"gpt-test\"\n\n[tui]\nstatus_line = [\"model\", \"current-dir\"]\nstatus_line_use_colors = false\n";
        std::fs::write(&path, original).unwrap();

        register_codex_statusline().unwrap();

        let configured = std::fs::read_to_string(&path).unwrap();
        assert!(configured.contains("# keep this comment"));
        assert!(configured.contains("model = \"gpt-test\""));
        let doc = configured.parse::<DocumentMut>().unwrap();
        assert_eq!(
            codex_status_line_of(codex_tui(&doc).unwrap()).unwrap(),
            Some(CODEX_STATUS_LINE.iter().map(|value| value.to_string()).collect())
        );
        assert_eq!(codex_status_line_colors_of(codex_tui(&doc).unwrap()).unwrap(), Some(true));

        let backup = std::fs::read_to_string(codex_statusline_backup_path()).unwrap();
        let backup: CodexStatuslineBackup = toml::from_str(&backup).unwrap();
        assert_eq!(backup.status_line, Some(vec!["model".into(), "current-dir".into()]));
        assert_eq!(backup.status_line_use_colors, Some(false));
    }

    #[test]
    fn codex_statusline_setup_is_idempotent_and_uninstall_restores_the_original() {
        let _d = iso();
        let path = codex_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "[tui]\nstatus_line = [\"model\", \"weekly-limit\"]\n";
        std::fs::write(&path, original).unwrap();

        register_codex_statusline().unwrap();
        register_codex_statusline().unwrap();
        assert_eq!(unregister_codex_statusline().unwrap(), 1);

        let restored = std::fs::read_to_string(&path).unwrap();
        let doc = restored.parse::<DocumentMut>().unwrap();
        assert_eq!(
            codex_status_line_of(codex_tui(&doc).unwrap()).unwrap(),
            Some(vec!["model".into(), "weekly-limit".into()])
        );
        assert_eq!(codex_status_line_colors_of(codex_tui(&doc).unwrap()).unwrap(), None);
        assert!(!codex_statusline_backup_path().exists());
    }

    #[test]
    fn codex_statusline_refuses_to_replace_invalid_toml() {
        let _d = iso();
        let path = codex_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "not valid toml ][";
        std::fs::write(&path, original).unwrap();

        assert!(register_codex_statusline().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!codex_statusline_backup_path().exists());
    }

    #[test]
    fn install_refuses_to_clobber_unparseable_settings() {
        let _d = iso();
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        let garbage = "{ this is not json ,,, ";
        std::fs::write(&sp, garbage).unwrap();

        let result = install_hooks_for(
            &claude_settings_path(),
            std::path::Path::new("/opt/ws/ws"),
            &ClaudeAgent,
        );
        assert!(result.is_err(), "install must error on unparseable settings.json");
        // the original file must be untouched
        assert_eq!(std::fs::read_to_string(&sp).unwrap(), garbage);
    }

    #[test]
    fn install_preserves_sibling_prefix_foreign_hook() {
        let _d = iso();
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        // foreign command whose path shares the textual prefix of hooks_dir but is a different dir
        let sibling = format!("{}-legacy/foo.sh", hooks_dir().display());
        let settings = serde_json::json!({
            "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": sibling } ] } ] }
        });
        std::fs::write(&sp, serde_json::to_string(&settings).unwrap()).unwrap();

        install_hooks_for(
            &claude_settings_path(),
            std::path::Path::new("/opt/ws/ws"),
            &ClaudeAgent,
        )
        .unwrap();

        let out: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
        let arr = out["hooks"]["SessionStart"].as_array().unwrap();
        // the sibling foreign hook must survive (path-boundary match, not string prefix)
        assert!(
            arr.iter()
                .any(|g| g["hooks"][0]["command"].as_str().unwrap().contains("-legacy/foo.sh")),
            "sibling-prefix foreign hook was wrongly dropped"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_unreadable_settings_file_is_never_replaced_by_an_empty_one() {
        use std::os::unix::fs::PermissionsExt;
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let _guard = lock();
        let _d = iso();
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        let original = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"~/.claude/hooks/cs/session-start.sh"}]}]},"other":"keep-me"}"#;
        std::fs::write(&sp, original).unwrap();

        let mut perms = std::fs::metadata(&sp).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&sp, perms).unwrap();

        let result = install_hooks_for(&sp, std::path::Path::new("/opt/ws/ws"), &ClaudeAgent);

        let mut perms = std::fs::metadata(&sp).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&sp, perms).unwrap();

        assert!(result.is_err(), "install must refuse to overwrite an unreadable settings.json");
        assert_eq!(
            std::fs::read_to_string(&sp).unwrap(),
            original,
            "the original settings.json must survive untouched"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_unreadable_settings_file_is_never_replaced_when_registering_statuslines() {
        use std::os::unix::fs::PermissionsExt;
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let _guard = lock();
        let _d = iso();
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        let original = r#"{"statusLine":{"type":"command","command":"~/my-custom-statusline"},"other":"keep-me"}"#;
        std::fs::write(&sp, original).unwrap();

        let mut perms = std::fs::metadata(&sp).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&sp, perms).unwrap();

        let result = register_statuslines(std::path::Path::new("/opt/ws/ws"));

        let mut perms = std::fs::metadata(&sp).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&sp, perms).unwrap();

        assert!(
            result.is_err(),
            "register_statuslines must refuse to overwrite an unreadable settings.json"
        );
        assert_eq!(
            std::fs::read_to_string(&sp).unwrap(),
            original,
            "the original settings.json must survive untouched"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_unreadable_backup_is_never_replaced_by_one_missing_the_original_command() {
        use std::os::unix::fs::PermissionsExt;
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let _guard = lock();
        let _d = iso();

        // register once with a foreign statusLine so a backup gets created
        let sp = claude_settings_path();
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        std::fs::write(
            &sp,
            r#"{"statusLine":{"type":"command","command":"~/my-original-statusline --flag"}}"#,
        )
        .unwrap();
        register_statuslines(std::path::Path::new("/opt/ws/ws")).unwrap();

        let bpath = crate::config::ws_config_dir().join("statusline-backup.json");
        let before = std::fs::read_to_string(&bpath).unwrap();
        assert!(
            before.contains("my-original-statusline"),
            "sanity: backup captured the original command"
        );

        // Write-only, no read: this isolates the *read* failure from a write
        // failure. A plain `fs::write` (the pre-fix code) would still succeed
        // here and silently clobber the backup with one missing the original
        // command — proving the bug is in treating an unreadable file as
        // empty, not merely in an inability to write.
        let mut perms = std::fs::metadata(&bpath).unwrap().permissions();
        perms.set_mode(0o200);
        std::fs::set_permissions(&bpath, perms).unwrap();

        // A second, different foreign statusLine (simulating another tool having
        // taken over statusLine in between) triggers another backup merge.
        std::fs::write(
            &sp,
            r#"{"statusLine":{"type":"command","command":"~/another-tool-statusline"}}"#,
        )
        .unwrap();
        let result = register_statuslines(std::path::Path::new("/opt/ws/ws"));

        let mut perms = std::fs::metadata(&bpath).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&bpath, perms).unwrap();

        assert!(
            result.is_err(),
            "register_statuslines must refuse to overwrite an unreadable backup file"
        );
        assert_eq!(
            std::fs::read_to_string(&bpath).unwrap(),
            before,
            "the original backup must survive untouched, not be replaced by one missing the first command"
        );
    }
}
