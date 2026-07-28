use anyhow::Result;
use std::path::PathBuf;

use crate::actors;
use crate::contract;
use crate::hookio;
use crate::limits;
use crate::readme;
use crate::timeline;
use crate::workspace::Workspace;

/// The workspace a hook is running inside, or None when not in a ws launch.
pub fn current_ws() -> Option<Workspace> {
    let name = std::env::var("WS_WORKSPACE").ok().filter(|s| !s.is_empty())?;
    let root = std::env::var("WS_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    Some(Workspace { name, root })
}

pub fn run(args: Vec<String>) -> Result<()> {
    let handler = args.first().map(String::as_str).unwrap_or("");
    match handler {
        "hook-payload" => hook_payload(args.get(1).map(String::as_str).unwrap_or("")),
        "session-start" => session_start(),
        "user-prompt" => user_prompt(),
        "stop" => stop(),
        "bash-audit" => bash_audit(),
        "session-end" => session_end(),
        "secret-redact" => secret_redact(),
        // real handlers are added in later tasks:
        _ => {} // unknown → silent no-op (never break the agent)
    }
    Ok(())
}

fn session_start() {
    let h = hookio::read_stdin();
    // subagents don't drive session lifecycle
    if h.agent_id.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
        return;
    }
    let ws = match current_ws() {
        Some(w) => w,
        None => return, // not a ws launch → no context, exit 0
    };

    // audit log
    let _ = append_log(&ws, &format!("session started (source: {})", h.source));

    // timeline: opened (only on a real start/resume, not clear/compact)
    if h.source == "startup" || h.source == "resume" {
        let _ = timeline::record(&ws.timeline(), "opened", &actors::actor_slug(), serde_json::json!({}));
    }

    // Record which agent session this is, and the lineage if it replaced one.
    // This is the only place either agent's real session id is observable:
    // Claude mints its own at launch, but Codex assigns one itself, and the
    // hook payload is where it surfaces. See `record_session_identity`.
    record_session_identity(&ws, &h);

    println!("{}", hookio::additional_context("SessionStart", &build_context(&ws)));
}

/// Record which agent session this workspace is now on, and the lineage if it
/// replaced a previous one.
///
/// This is the only place either agent's real session id is observable from one
/// code path. Claude mints its own id at launch (`--session-id`), but Codex
/// assigns one itself and the hook payload is the only place it surfaces —
/// verified against Codex CLI 0.145.0, whose SessionStart payload carries
/// `session_id` (see `docs/2026-07-27-codex-hook-contract-verified.md`). With the
/// id recorded here, `codex resume <uuid>` addresses a session exactly, which
/// replaced an ownership-marker heuristic and a 32 MiB rollout-directory scan.
///
/// Recording lineage here rather than at launch is what finally gives
/// `conversations::record_rotation` a caller: it had **zero**, so every `rotated`
/// row in `ws -conversations` described a shape production never wrote. One hook
/// now serves both agents, so neither can drift.
///
/// `$WS_AGENT` is exported by both agents' launch. Without it there is nothing to
/// file the id under, so the function does nothing rather than guessing.
fn record_session_identity(ws: &Workspace, h: &hookio::HookInput) {
    let agent = match std::env::var("WS_AGENT") {
        Ok(a) if !a.is_empty() => a,
        _ => return,
    };
    if h.session_id.trim().is_empty() {
        return;
    }
    let prior = contract::read_session_id(&ws.state_toml(), &agent);
    if prior.as_deref() == Some(h.session_id.as_str()) {
        return; // same session resuming; nothing changed
    }
    if let Err(e) = contract::write_session_id(&ws.state_toml(), &agent, &h.session_id) {
        // Losing the id means a later launch cannot resume this session. That is
        // worth a line the user can act on, not silence.
        eprintln!(
            "ws: warning: could not record the {agent} session id in {}: {e:#}",
            ws.state_toml().display()
        );
        return;
    }
    let _ = append_log(
        ws,
        &format!("{agent} session {} recorded (source: {})", h.session_id, h.source),
    );
    crate::conversations::record_rotation(
        ws,
        &agent,
        prior.as_deref(),
        &h.session_id,
        &h.source,
    );
}

fn user_prompt() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if h.prompt.trim().is_empty() {
        return;
    }
    let _ = readme::capture_objective(&ws.readme(), &h.prompt);
    // Phase 2: capture only, no context injection → no stdout.

    if ws.limit_guard().exists() {
        let notice = "Note: the ws rate-limit guard is active (a handoff was already saved). \
            Continuing spends more of the current budget — that's fine, but it's your call.";
        println!("{}", hookio::additional_context("UserPromptSubmit", notice));
    }
}

const COOLDOWN_SECS: u64 = 300;

fn stop() {
    let ws = match current_ws() {
        Some(w) => w,
        // Stop hooks allow the turn to end by exiting successfully without a
        // decision. Claude only accepts `decision: "block"` for this event;
        // emitting the old `decision: "approve"` shape is invalid.
        None => return,
    };
    let _ = hookio::read_stdin(); // drain stdin; Stop payload is unused

    // Limit-aware handoff: check before the notebook reminder.
    if let Some(directive) = limit_check(&ws) {
        println!("{}", hookio::decision_block(&directive));
        return;
    }

    let newest_nb = newest_mtime_secs(&ws.notebook_dir());
    let stamp = ws.local_dir().join("notebook-reminder.stamp");
    let stamp_age = age_secs(&stamp);

    let approve = match newest_nb {
        None => true, // nothing to nag about yet
        Some(nb_age) if nb_age < COOLDOWN_SECS => true, // just updated
        _ => stamp_age.map(|a| a < COOLDOWN_SECS).unwrap_or(false), // cooled down recently
    };

    if approve {
        return;
    }

    // touch the stamp and remind
    let _ = std::fs::create_dir_all(ws.local_dir());
    let _ = std::fs::write(&stamp, crate::now_iso());
    let reason = "Notebook check. Append any new findings to your own notebook \
        (.ws/notebook/notebook.<actor>.md — run `ws -whoami` if unsure which actor \
        you are; never edit a teammate's). If a prior note was disproven by your recent \
        work, correct it. If nothing needs changing, say so in one line and stop.";
    println!("{}", hookio::decision_block(reason));
}

/// Returns Some(directive) when the Stop hook should block for a limit handoff.
/// Also manages the guard marker (write on first cross; clear on reset) and a
/// best-effort desktop notification. Returns None to fall through to the
/// notebook reminder (including in "warn" mode).
fn limit_check(ws: &Workspace) -> Option<String> {
    let cfg = crate::config::load();
    let snap = limits::read(&ws.local_dir().join("limits.json"))?;
    let guard = ws.limit_guard();

    match limits::over_threshold(&snap, cfg.limit_warn_5h, cfg.limit_warn_week) {
        None => {
            // reset (or never crossed) → clear a stale guard
            let _ = std::fs::remove_file(&guard);
            None
        }
        Some(window) => {
            let first_time = !guard.exists();
            if first_time {
                let _ = std::fs::create_dir_all(ws.local_dir());
                let _ = std::fs::write(&guard, crate::now_iso());
                notify(&format!(
                    "ws: Claude {window} limit high in {} — work is being saved.",
                    ws.name
                ));
            }
            if cfg.limit_action == "warn" {
                return None; // warn-only: don't block
            }
            if !first_time {
                // already handed off once; let turns end normally afterwards
                return None;
            }
            Some(format!(
                "Rate-limit guard: the Claude {window} window is high. Finish only the \
                 current step — start no new work — update your notebook \
                 (.ws/notebook/notebook.<actor>.md), write a handoff to .ws/handoffs/, then \
                 stop and tell the user: work is saved, continue in a fresh window later or \
                 with another agent."
            ))
        }
    }
}

fn notify(msg: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification {:?} with title \"ws\"", msg);
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = msg;
    }
}

/// Age in seconds of the newest `notebook.*.md` in `dir`, or None if none exist.
fn newest_mtime_secs(dir: &std::path::Path) -> Option<u64> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut newest: Option<std::time::SystemTime> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("notebook.") && name.ends_with(".md")) {
            continue;
        }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            newest = Some(newest.map_or(m, |n| n.max(m)));
        }
    }
    newest.map(system_time_age_secs)
}

fn age_secs(path: &std::path::Path) -> Option<u64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(system_time_age_secs(m))
}

fn system_time_age_secs(t: std::time::SystemTime) -> u64 {
    std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build_context(ws: &Workspace) -> String {
    let mut s = format!("# ws workspace: {}\n\n", ws.name);

    if let Ok(readme) = std::fs::read_to_string(ws.readme()) {
        if let Some(obj) = crate::readme::objective_of(&readme) {
            s.push_str(&format!("Objective: {obj}\n\n"));
        }
    }

    // list notebook files so the agent knows where to read/append
    if let Ok(rd) = std::fs::read_dir(ws.notebook_dir()) {
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("notebook.") && n.ends_with(".md"))
            .collect();
        names.sort();
        if !names.is_empty() {
            s.push_str("Notebook files (append findings to your own): ");
            s.push_str(&names.join(", "));
            s.push_str("\n\n");
        }
    }

    s.push_str(
        "Protocol: read .ws/README.md and .ws/notebook/ on start; append findings to \
         your own notebook (ws -whoami for your actor); write a handoff to .ws/handoffs/ \
         on rotate or agent switch; store secrets via ws -secrets, never in files.",
    );
    s
}

fn append_log(ws: &Workspace, msg: &str) -> std::io::Result<()> {
    let path = ws.session_log();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "[{}] {}", crate::now_iso(), msg)
}

fn bash_audit() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if h.tool_name != "Bash" {
        return;
    }
    let mut cmd = h.tool_input.command;
    if cmd.is_empty() {
        return;
    }
    if cmd.chars().count() > 200 {
        cmd = format!("{}...", cmd.chars().take(200).collect::<String>());
    }
    let _ = append_log(&ws, &format!("BASH: {cmd}"));
}

fn session_end() {
    let _ = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    let _ = timeline::record(&ws.timeline(), "closed", &actors::actor_slug(), serde_json::json!({}));
}

/// The files a `PostToolUse` payload says were just written.
///
/// Claude's `Write`/`Edit` name one file in `tool_input.file_path`. Codex's
/// `apply_patch` names **zero or more** in an envelope inside
/// `tool_input.command`, and leaves `file_path` empty — verified against Codex
/// CLI 0.145.0, where the payload is
/// `{"tool_name":"apply_patch","tool_input":{"command":"*** Begin Patch\n*** Add File: /abs/p\n+x\n*** End Patch"}}`.
/// Reading only `file_path` therefore found nothing to redact on Codex even once
/// the matcher was fixed, so both halves are needed to close the gap.
///
/// `Delete File` is skipped: there is nothing left to scan. `Move to` is
/// followed, because the content lands at the new path. Relative paths are
/// resolved against the payload's `cwd`, which is where Codex ran the patch.
fn written_paths(h: &hookio::HookInput) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    if !h.tool_input.file_path.is_empty() {
        return vec![PathBuf::from(&h.tool_input.file_path)];
    }
    // NotebookEdit carries its target as `notebook_path`; matching the tool
    // without reading this field would make notebook redaction a silent no-op.
    if !h.tool_input.notebook_path.is_empty() {
        return vec![PathBuf::from(&h.tool_input.notebook_path)];
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for line in h.tool_input.command.lines() {
        let t = line.trim();
        let rest = t
            .strip_prefix("*** Add File:")
            .or_else(|| t.strip_prefix("*** Update File:"))
            .or_else(|| t.strip_prefix("*** Move to:"));
        if let Some(raw) = rest {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let p = PathBuf::from(raw);
            let p = if p.is_absolute() { p } else { PathBuf::from(&h.cwd).join(p) };
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Tool names that mean "a file on disk just changed".
///
/// Claude's write side is `Write`, `Edit`, `MultiEdit` and `NotebookEdit`;
/// Codex's is the single `apply_patch`. The registered matcher should already
/// have filtered, but a handler that trusts the matcher blindly is how the
/// Codex gap stayed invisible for two releases — so it is re-checked here, and
/// this list has to stay in step with the matchers in `agents/claude.rs` and
/// `agents/codex.rs`.
const WRITE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit", "apply_patch"];

fn secret_redact() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if !WRITE_TOOLS.contains(&h.tool_name.as_str()) {
        return;
    }
    for path in written_paths(&h) {
        if !path.is_file() {
            continue; // deleted, a directory, or never created — nothing to scan
        }
        match contained(&ws.root, &path) {
            Ok(real) => redact_file(&ws, &real),
            // Logged, not printed. An agent writing outside the workspace is
            // routine — a scratch file in /tmp, a dotfile in $HOME — and a
            // stderr line on every such tool call would teach the user to
            // ignore this hook's stderr, which is where the failures that
            // matter (below) are reported.
            Err(reason) => {
                let _ = append_log(&ws, &format!("redaction skipped: {reason}"));
            }
        }
    }
}

/// `path` resolved to a real location inside `root`, or the reason it is not.
///
/// Both sides are canonicalized before comparing, and that is not
/// belt-and-braces: on macOS `/tmp` is a symlink to `/private/tmp`, so a
/// workspace rooted at `/tmp/w` and a payload naming `/tmp/w/.env` compare
/// unequal until both are resolved — a textual check would reject every write
/// in a temp-rooted workspace, which is also every write in this crate's own
/// tests. Resolving closes the direction that matters too:
/// `<root>/../../.aws/credentials`, and a symlink pointing out of the tree,
/// both fail here instead of passing a prefix test on the unresolved string.
///
/// Fail-closed on purpose: a path that cannot be resolved is refused rather
/// than compared literally.
pub fn contained(root: &std::path::Path, path: &std::path::Path) -> Result<PathBuf, String> {
    let real_root = std::fs::canonicalize(root)
        .map_err(|e| format!("workspace root {} could not be resolved ({e})", root.display()))?;
    let real = std::fs::canonicalize(path)
        .map_err(|e| format!("{} could not be resolved ({e})", path.display()))?;
    if real.starts_with(&real_root) {
        Ok(real)
    } else {
        Err(format!(
            "{} is outside the workspace root {}",
            path.display(),
            real_root.display()
        ))
    }
}

/// Report that a credential was found and *not* captured.
///
/// Both channels, always. Stderr is the only one that reaches the operator in
/// the moment (a PostToolUse hook's stderr is surfaced without blocking the
/// tool call) and it is gone when the pane scrolls; the session log is the only
/// one that survives. The old code returned silently here, which left the
/// plaintext on disk with no record anywhere that redaction had been asked for
/// and declined — strictly worse than not having the feature, because the
/// README promised it. Never includes the value.
fn report_skipped(ws: &Workspace, path: &std::path::Path, reason: &str) {
    let msg = format!("redaction skipped: secret store unavailable ({reason})");
    eprintln!(
        "ws: {msg} — {} still contains what the agent wrote.",
        path.display()
    );
    let _ = append_log(ws, &msg);
}

/// Redact one file in place. Split out of `secret_redact` because a single
/// `apply_patch` can write several files and each must be handled independently
/// — one unreadable file must not abandon the rest.
fn redact_file(ws: &Workspace, path: &std::path::Path) {
    let text = match std::fs::read_to_string(path) {
        // Unreadable, or not UTF-8. Neither is evidence of a leak (binary
        // output is the common case) and neither is actionable here.
        Err(_) => return,
        Ok(t) => t,
    };
    // Cheap pre-scan (no store, no keyring, no password): is there any
    // secret-looking line at all? This runs on *every* file write the agent
    // makes, so nothing expensive or interactive may happen above this line.
    // It uses the same predicate as the rewrite below — one predicate, so the
    // two cannot drift into disagreeing about what a secret is.
    if !text.lines().any(|line| {
        parse_assignment(line).is_some_and(|(name, value)| is_secret_assignment(name, value))
    }) {
        return;
    }
    // A hook has no human attached: its stdin is the payload JSON. On the file
    // backend `secrets::open` calls rpassword, which opens /dev/tty — there is
    // nothing to read from here, and blocking the agent mid-turn on a password
    // nobody will type is the worst outcome available. "Would have to prompt"
    // is therefore the same case as "unavailable", decided before opening
    // rather than discovered by hanging.
    if crate::secrets::would_prompt_for_password() {
        report_skipped(
            ws,
            path,
            "no master password: $WS_SECRETS_PASSWORD is unset and a hook cannot prompt",
        );
        return;
    }
    let store = match crate::secrets::open(&ws.name) {
        Ok(s) => s,
        Err(e) => {
            report_skipped(ws, path, &format!("{e:#}"));
            return;
        }
    };

    // Two passes, and one store write.
    //
    // This used to call `store.get` + `store.set` **per secret-shaped line**, and
    // on the file backend each of those is a full Argon2id derivation plus a
    // re-encryption of the entire store — O(n) whole-store rewrites for one file.
    // The installer bounds every hook at 10 seconds (`hooksetup`), so a `.env`
    // with a few dozen credentials simply ran out of time, and the kill landed
    // *after* some values had been stored and *before* the file was rewritten:
    // credentials captured, plaintext still on disk, and the loud warning below
    // never reached. Deciding first and writing once removes the window.
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut collisions: Vec<String> = Vec::new();
    let existing = match store.list() {
        Ok(names) => names,
        Err(e) => {
            report_skipped(ws, path, &format!("cannot list the secret store: {e:#}"));
            return;
        }
    };

    for line in text.lines() {
        let Some((name, value)) = parse_assignment(line) else { continue };
        if !is_secret_assignment(name, value) {
            continue;
        }
        // One store, one namespace: if this NAME already holds a *different*
        // value (another file's credential), overwriting it would make
        // `ws -secrets restore` write this file's value into that file, and the
        // earlier one would be unrecoverable. Leave the line as plaintext and say
        // so. Same value, or absent, proceeds.
        if existing.iter().any(|n| n == name) {
            match store.get(name) {
                Ok(Some(v)) if v != value => {
                    collisions.push(name.to_string());
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    report_skipped(ws, path, &format!("cannot read {name} from the store: {e:#}"));
                    return;
                }
            }
        }
        pending.push((name.to_string(), value.to_string()));
    }

    for name in &collisions {
        eprintln!(
            "ws: redaction left {name} in {} alone: the secret store already holds a different \
             value under that name (from an earlier redaction); rename one of the variables or \
             store it manually.",
            path.display()
        );
        let _ = append_log(
            ws,
            &format!("redaction skipped: {name} already stored with a different value"),
        );
    }
    if pending.is_empty() {
        return;
    }

    // Store everything, then rewrite once. A failed store leaves the file
    // untouched, which is the safe direction: a placeholder for a value that was
    // never stored destroys the credential outright.
    if let Err(e) = store.set_many(&pending) {
        eprintln!(
            "ws: redaction could not store {} value(s) from {} ({e:#}); the file is unchanged.",
            pending.len(),
            path.display()
        );
        let _ = append_log(ws, &format!("redaction skipped: could not store ({e})"));
        return;
    }

    let redacted: Vec<String> = pending.iter().map(|(n, _)| n.clone()).collect();
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match parse_assignment(line) {
            Some((name, _)) if redacted.iter().any(|n| n == name) => {
                // Preserve the original left-hand side and any surrounding quote.
                let (lhs, rhs) = line.split_once('=').unwrap(); // safe: parse_assignment matched
                let rhs_trim = rhs.trim();
                let quote = if (rhs_trim.starts_with('"') && rhs_trim.ends_with('"'))
                    || (rhs_trim.starts_with('\'') && rhs_trim.ends_with('\''))
                {
                    rhs_trim.chars().next().unwrap().to_string()
                } else {
                    String::new()
                };
                out.push_str(lhs);
                out.push('=');
                out.push_str(&quote);
                out.push_str(&placeholder_for(name));
                out.push_str(&quote);
                out.push('\n');
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    // Every value in `redacted` is now in the store, so neither of these failures
    // may be swallowed. A failed rewrite leaves the file holding plaintext while
    // the store says it was captured — the most dangerous state this function can
    // reach — and a failed manifest write loses the record of what was captured.
    //
    // `mode_of` because this rewrites a file the *user* owns: a `.env` is often
    // 0600, and recreating it under the hook's umask would loosen it to 0644 —
    // and then `ws -secrets restore` would copy that 0644 back onto the restored
    // plaintext.
    if let Err(e) = crate::atomic::atomic_write_with_mode(path, &out, crate::atomic::mode_of(path))
    {
        eprintln!(
            "ws: redaction FAILED to rewrite {} ({e}). {} value(s) were stored but the \
             plaintext is still in that file — remove it by hand.",
            path.display(),
            redacted.len()
        );
        return;
    }
    if let Err(e) = note_manifest(ws, &redacted) {
        eprintln!(
            "ws: redacted {} value(s) from {} but could not record them in the manifest ({e}).",
            redacted.len(),
            path.display()
        );
    }
}

/// A `NAME=VALUE` line (VALUE may be quoted). Returns (name, unquoted value) or None.
fn parse_assignment(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    if t.starts_with('#') {
        return None;
    }
    let (name, rest) = t.split_once('=')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let value = rest.trim().trim_matches('"').trim_matches('\'');
    Some((name, value))
}

/// Substrings that make a NAME credential-shaped. `ACCESS_KEY` is what catches
/// `AWS_ACCESS_KEY_ID` (which `_KEY`-suffix matching missed entirely, because
/// the name does not end there); `DSN` and `WEBHOOK` are here because both
/// routinely carry an embedded credential in their value.
const SECRET_NAME_PARTS: &[&str] = &[
    "SECRET", "TOKEN", "PASSWORD", "PASSWD", "PASSPHRASE", "APIKEY", "API_KEY", "ACCESS_KEY",
    "CREDENTIAL", "WEBHOOK", "DSN", "BEARER",
];

/// Value prefixes that identify a credential on their own, regardless of what
/// the name says. Each is a published, issuer-assigned marker: OpenAI `sk-`,
/// GitHub `ghp_`/`gho_`/`github_pat_`, GitLab `glpat-`, Slack `xox*-`, AWS
/// `AKIA`/`ASIA`, Google `AIza`, npm `npm_`, PyPI `pypi-`.
const SECRET_VALUE_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "xoxa-", "xoxs-", "AKIA",
    "ASIA", "AIza", "npm_", "pypi-",
];

/// Values that are configuration, never credentials, whatever the name is.
const NON_SECRET_VALUES: &[&str] = &["true", "false", "yes", "no", "on", "off", "none", "null"];

/// Shortest value the generic (prefix-free) branch will treat as a credential.
/// Set to exclude the port numbers, booleans, sizes and enum words that make up
/// almost every false positive, while still catching a short API key.
const MIN_SECRET_VALUE_LEN: usize = 12;

/// The two-signal test: a credential-shaped NAME **and** a credential-shaped
/// VALUE, both required.
///
/// Name alone — what this replaces — redacted `PASSWORD_MIN_LENGTH=8`,
/// `TOKENIZER=gpt2`, `TOKEN_BUDGET=4096` and `SECRET_SCAN_ENABLED=true`,
/// replacing working configuration with a placeholder that (until
/// `ws -secrets restore` existed) nothing resolved. Value alone would redact
/// every long unquoted string in every file the agent writes. Requiring both
/// keeps `AWS_ACCESS_KEY_ID=AKIA…` and `GITHUB_PAT=github_pat_…`.
///
/// Known gaps, deliberately out of scope here: JSON/YAML values, credentials
/// embedded in URLs (`DATABASE_URL=postgres://user:pw@host`), and files written
/// by a Bash heredoc rather than a write tool. Those are documented rather than
/// half-caught — see task 6.
fn is_secret_assignment(name: &str, value: &str) -> bool {
    // An already-redacted line is not a candidate: re-storing the placeholder
    // as if it were the value is how a redacted file loses its secret for good.
    !value.starts_with(PLACEHOLDER_OPEN) && name_looks_secret(name) && value_looks_secret(value)
}

fn name_looks_secret(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    if SECRET_NAME_PARTS.iter().any(|p| u.contains(p)) {
        return true;
    }
    if u.ends_with("_KEY") || u == "KEY" {
        return true;
    }
    // `PAT` is three letters that live inside PATH, PATTERN, XPATH and PATCH,
    // so it matches only as a whole `_`-separated segment: `GITHUB_PAT` yes,
    // `PATTERN_FILE` and `XPATH_QUERY` no.
    u.split('_').any(|segment| segment == "PAT")
}

fn value_looks_secret(value: &str) -> bool {
    if SECRET_VALUE_PREFIXES.iter().any(|p| value.starts_with(p)) {
        return true;
    }
    value.chars().count() >= MIN_SECRET_VALUE_LEN
        && !value.chars().any(char::is_whitespace)
        && !value.chars().all(|c| c.is_ascii_digit())
        && !NON_SECRET_VALUES.iter().any(|w| w.eq_ignore_ascii_case(value))
}

/// The placeholder redaction leaves behind, and the only shape
/// `ws -secrets restore` resolves. Writer and reader live next to each other
/// because a disagreement about this string is a file nothing can repair: the
/// hook has already moved the value into the store by the time it writes one.
pub const PLACEHOLDER_OPEN: &str = "{{ws:secret:";
pub const PLACEHOLDER_CLOSE: &str = "}}";

fn placeholder_for(name: &str) -> String {
    format!("{PLACEHOLDER_OPEN}{name}{PLACEHOLDER_CLOSE}")
}

/// Only names redaction could actually have written — the same charset
/// `secrets::validate_name` accepts. Anything else inside the braces is some
/// other templating system's business and is left byte-for-byte alone, rather
/// than handed to a store that would reject the name and turn one stray brace
/// into a hard failure over the whole file.
fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The result of resolving every placeholder in one file.
///
/// No `Debug`, deliberately: `text` is the file *with its credentials back in
/// it*, and a derived `Debug` is one `dbg!` or `{:?}` away from printing every
/// secret in the workspace into a log.
pub struct Restored {
    pub text: String,
    /// Placeholders replaced — occurrences, not distinct names.
    pub resolved: usize,
    /// Distinct names `lookup` had no value for, in first-seen order. Their
    /// placeholders are still in `text`, byte-for-byte.
    pub missing: Vec<String>,
}

/// Replace every `{{ws:secret:NAME}}` in `text` with `lookup(NAME)`.
///
/// A name the store does not have leaves its placeholder exactly as it was and
/// is reported: substituting an empty string, or dropping the line, silently
/// corrupts a config file, and the caller needs to be able to exit non-zero.
/// A `lookup` *error* (an unreadable or wrongly-keyed store) aborts the whole
/// file instead of degrading to "missing" — reporting "not in the store" for a
/// store nobody could read would send the user hunting for a secret that is
/// sitting right there.
pub fn resolve_placeholders(
    text: &str,
    mut lookup: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<Restored> {
    let mut out = String::with_capacity(text.len());
    let mut resolved = 0usize;
    let mut missing: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(PLACEHOLDER_OPEN) {
        let (before, from_open) = rest.split_at(at);
        out.push_str(before);
        let body = &from_open[PLACEHOLDER_OPEN.len()..];
        // An unterminated or non-conforming marker is not ours: copy the opener
        // and resume scanning *after* it, so a stray `{{ws:secret:` cannot spin.
        let Some(end) = body.find(PLACEHOLDER_CLOSE) else {
            out.push_str(PLACEHOLDER_OPEN);
            rest = body;
            continue;
        };
        let name = &body[..end];
        if !is_placeholder_name(name) {
            out.push_str(PLACEHOLDER_OPEN);
            rest = body;
            continue;
        }
        match lookup(name)? {
            Some(value) => {
                out.push_str(&value);
                resolved += 1;
            }
            None => {
                out.push_str(&placeholder_for(name));
                if !missing.iter().any(|m| m == name) {
                    missing.push(name.to_string());
                }
            }
        }
        rest = &body[end + PLACEHOLDER_CLOSE.len()..];
    }
    out.push_str(rest);
    Ok(Restored { text: out, resolved, missing })
}

/// Record which credential names were captured.
///
/// Transacted: Claude issues tool calls in parallel, so two `secret-redact` hook
/// processes can be inside this read-modify-write at once, and the loser's entries
/// vanished. This file is the only record that a credential was moved into the
/// store, which makes a silently partial list the worst possible outcome.
fn note_manifest(ws: &Workspace, names: &[String]) -> std::io::Result<()> {
    let path = ws.ws_dir().join("artifacts").join("MANIFEST.json");
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::txn::transaction(&path, || note_manifest_locked(&path, names))
        .map_err(|e| std::io::Error::other(format!("{e:#}")))
}

fn note_manifest_locked(path: &std::path::Path, names: &[String]) -> anyhow::Result<()> {
    let path = path.to_path_buf();
    // Absent → start fresh; unreadable *or unparseable* → refuse. Defaulting on
    // either would write the new entry back over an empty object, losing every
    // `redacted_secrets` entry already recorded. A credential record is the one
    // thing that must never be silently dropped, so a corrupt manifest is an
    // error the caller reports rather than damage this function papers over.
    let mut val: serde_json::Value = match crate::io_read::read_or_absent(&path)? {
        Some(s) => serde_json::from_str(&s).map_err(|e| {
            anyhow::anyhow!("{} is not valid JSON ({e}); refusing to overwrite it", path.display())
        })?,
        None => serde_json::json!({}),
    };
    // Valid JSON that is not an object is corruption too, not a fresh start.
    if !val.is_object() {
        anyhow::bail!("{} is not a JSON object; refusing to overwrite it", path.display());
    }
    let arr = val
        .as_object_mut()
        .unwrap()
        .entry("redacted_secrets")
        .or_insert_with(|| serde_json::json!([]));
    if let Some(a) = arr.as_array_mut() {
        for n in names {
            a.push(serde_json::json!({ "name": n, "at": crate::now_iso() }));
        }
    }
    crate::atomic::atomic_write(&path, serde_json::to_string_pretty(&val)?)
}

/// `ws internal hook-payload <field>` — print one field of the stdin hook JSON.
fn hook_payload(field: &str) {
    let h = hookio::read_stdin();
    let value = match field {
        "session_id" => h.session_id,
        "cwd" => h.cwd,
        "source" => h.source,
        "prompt" => h.prompt,
        "tool_name" => h.tool_name,
        "command" => h.tool_input.command,
        "agent_id" => h.agent_id.unwrap_or_default(),
        _ => String::new(),
    };
    println!("{value}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[cfg(unix)]
    fn an_unreadable_manifest_is_never_replaced_by_one_missing_prior_entries() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

        let d = TempDir::new().unwrap();
        let ws = Workspace { name: "w".into(), root: d.path().to_path_buf() };
        let path = ws.ws_dir().join("artifacts").join("MANIFEST.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // an existing manifest already recording a prior redaction
        note_manifest(&ws, &["FIRST_SECRET".to_string()]).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(before.contains("FIRST_SECRET"), "sanity: the prior entry was recorded");

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        let result = note_manifest(&ws, &["SECOND_SECRET".to_string()]);

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();

        assert!(result.is_err(), "note_manifest must refuse to overwrite an unreadable manifest");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "the original manifest must survive untouched, not be replaced by one missing FIRST_SECRET"
        );
    }

    /// M7. A manifest that is *readable but corrupt* used to map to `json!({})`
    /// and get written straight back, silently discarding every recorded
    /// `redacted_secrets` entry — the exact damage the unreadable-case test
    /// above pins, arriving through the other door. Truncated JSON is the
    /// realistic shape here: an interrupted write, not a hand edit.
    #[test]
    fn a_corrupt_manifest_is_refused_rather_than_silently_reset() {
        let d = TempDir::new().unwrap();
        let ws = Workspace { name: "w".into(), root: d.path().to_path_buf() };
        let path = ws.ws_dir().join("artifacts").join("MANIFEST.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let corrupt = r#"{"redacted_secrets": [{"name": "FIRST_SECRET", "at": "2026-0"#;
        std::fs::write(&path, corrupt).unwrap();

        let result = note_manifest(&ws, &["SECOND_SECRET".to_string()]);

        assert!(result.is_err(), "a corrupt manifest must be an error, not a fresh start");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            corrupt,
            "the corrupt bytes must survive so the prior entry is recoverable by hand"
        );
        // Discriminating: the old behaviour left a *valid* file that parsed and
        // contained SECOND_SECRET but not FIRST_SECRET. Assert that shape is gone.
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("SECOND_SECRET"), "must not have written the new entry");
        assert!(after.contains("FIRST_SECRET"), "must not have dropped the prior entry");
    }

    /// Claude's shape: one file, named directly.
    #[test]
    fn a_claude_write_payload_names_exactly_the_one_file_it_wrote() {
        let h = hookio::parse(
            r#"{"tool_name":"Write","cwd":"/w","tool_input":{"file_path":"/w/.env"}}"#,
        );
        assert_eq!(written_paths(&h), vec![std::path::PathBuf::from("/w/.env")]);
    }

    /// Codex's shape, verbatim from a captured Codex CLI 0.145.0 payload: the
    /// target is inside the patch envelope and `file_path` is absent entirely.
    /// Before this, `written_paths` had nothing to return and redaction was a
    /// no-op on Codex even with the matcher fixed.
    #[test]
    fn a_codex_apply_patch_payload_names_the_file_inside_the_envelope() {
        let h = hookio::parse(
            r#"{"tool_name":"apply_patch","cwd":"/w","tool_input":{"command":"*** Begin Patch\n*** Add File: /w/.env\n+API_KEY=abc\n*** End Patch"}}"#,
        );
        assert_eq!(written_paths(&h), vec![std::path::PathBuf::from("/w/.env")]);
    }

    /// One patch can touch several files, and a single-path return would silently
    /// redact the first and leave credentials in the rest.
    #[test]
    fn a_multi_file_patch_names_every_written_file_and_skips_deletions() {
        let patch = "*** Begin Patch\n\
                     *** Add File: a.env\n+A_KEY=1\n\
                     *** Update File: /abs/b.env\n+B_TOKEN=2\n\
                     *** Delete File: c.env\n\
                     *** Move to: d.env\n\
                     *** End Patch";
        let raw = serde_json::json!({
            "tool_name": "apply_patch",
            "cwd": "/work",
            "tool_input": { "command": patch }
        })
        .to_string();
        let got = written_paths(&hookio::parse(&raw));

        // Relative paths resolve against cwd; absolute ones are left alone.
        assert!(got.contains(&std::path::PathBuf::from("/work/a.env")), "{got:?}");
        assert!(got.contains(&std::path::PathBuf::from("/abs/b.env")), "{got:?}");
        assert!(got.contains(&std::path::PathBuf::from("/work/d.env")), "Move to target: {got:?}");
        // A deleted file has no content left to scan, and scanning it would be an error.
        assert!(!got.contains(&std::path::PathBuf::from("/work/c.env")), "deletion must be skipped: {got:?}");
        assert_eq!(got.len(), 3, "exactly the three written files: {got:?}");
    }

    /// A patch that writes nothing must produce no targets rather than, say, the
    /// workspace root — which `PathBuf::from(cwd).join("")` would have yielded.
    #[test]
    fn a_patch_with_no_file_headers_yields_no_targets() {
        let h = hookio::parse(
            r#"{"tool_name":"apply_patch","cwd":"/w","tool_input":{"command":"*** Begin Patch\n*** End Patch"}}"#,
        );
        assert!(written_paths(&h).is_empty());
    }

    /// Valid JSON of the wrong type is corruption too. `[]` used to be replaced
    /// by `{}` and written back, which is the same silent data loss.
    #[test]
    fn a_manifest_that_is_valid_json_but_not_an_object_is_refused() {
        let d = TempDir::new().unwrap();
        let ws = Workspace { name: "w".into(), root: d.path().to_path_buf() };
        let path = ws.ws_dir().join("artifacts").join("MANIFEST.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"["FIRST_SECRET"]"#).unwrap();

        assert!(note_manifest(&ws, &["SECOND".to_string()]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"["FIRST_SECRET"]"#);
    }

    // ---------- task 4: two-signal classification ----------

    /// The four false positives the name-only classifier produced, plus the
    /// credentials the two-signal one must still catch. One table, because the
    /// property being pinned is the *pair* of decisions: a change that fixes
    /// one column by breaking the other is not a fix.
    #[test]
    fn classification_needs_both_a_credential_name_and_a_credential_value() {
        for (name, value) in [
            // The advertised false positives: credential-shaped NAME, plainly
            // non-credential VALUE.
            ("PASSWORD_MIN_LENGTH", "8"),
            ("TOKENIZER", "gpt2"),
            ("TOKEN_BUDGET", "4096"),
            ("SECRET_SCAN_ENABLED", "true"),
            ("API_KEY_ENABLED", "FALSE"),          // stop-word, case-insensitively
            ("SESSION_TOKEN_TTL", "3600000000"),   // long but purely digits
            ("TOKEN_HEADER", "X-Auth Token"),      // whitespace: prose, not a token
            ("API_KEY", ""),                       // nothing to store
            // No name signal, however credential-shaped the value looks.
            ("PORT", "8080"),
            ("MONKEY", "banana"),
            ("HOMEPAGE_URL", "https://example.com/some/long/path"),
        ] {
            assert!(
                !is_secret_assignment(name, value),
                "{name}={value:?} must NOT be redacted"
            );
        }

        for (name, value) in [
            ("AWS_ACCESS_KEY_ID", "AKIA0123456789EXAMPLE"),
            ("GITHUB_PAT", "github_pat_11ABCDEFG0123456789"),
            ("API_KEY", "supersecret123"),
            ("MY_PASSWORD", "correcthorsebatterystaple"),
            ("SENTRY_DSN", "https://abc123@o1.ingest.sentry.io/2"),
            ("SLACK_WEBHOOK", "https://hooks.slack.com/services/T0/B0/xxxxxxxx"),
            ("SIGNING_PASSPHRASE", "a-long-enough-passphrase"),
            ("SERVICE_CREDENTIAL", "abcdefghijkl"), // exactly the length floor
            ("AUTH_BEARER", "eyJhbGciOiJIUzI1NiJ9.e30.x"),
            // A known prefix wins on its own: `sk-abc` is under the length
            // floor and is still unmistakably an OpenAI key.
            ("OPENAI_TOKEN", "sk-abc"),
        ] {
            assert!(
                is_secret_assignment(name, value),
                "{name}={value:?} MUST be redacted"
            );
        }
    }

    /// A line already redacted must never be a candidate again: re-storing the
    /// placeholder as if it were the value overwrites the real secret in the
    /// store with the literal text `{{ws:secret:NAME}}`, destroying it.
    #[test]
    fn an_already_redacted_line_is_not_a_candidate() {
        assert!(!is_secret_assignment("API_KEY", "{{ws:secret:API_KEY}}"));
    }

    /// `PAT` as a substring would redact half a build config. As a whole
    /// `_`-separated segment it catches exactly the thing it is for.
    #[test]
    fn pat_is_a_name_signal_only_as_a_whole_segment() {
        for yes in ["GITHUB_PAT", "PAT", "pat", "GH_PAT_OLD"] {
            assert!(name_looks_secret(yes), "{yes} is a personal access token name");
        }
        for no in ["PATH", "PATTERN_FILE", "XPATH_QUERY", "PATCH_LEVEL", "COMPATIBILITY"] {
            assert!(!name_looks_secret(no), "{no} must not match PAT");
        }
    }

    // ---------- task 4: path containment ----------

    /// Containment must survive a symlinked root, and this is not hypothetical:
    /// on macOS `/tmp` and `/var` are symlinks into `/private`, so a temp-rooted
    /// workspace — every workspace in this crate's own test suite — reaches this
    /// function with the two sides spelled differently. Canonicalizing only one
    /// side rejects every write in such a workspace.
    #[test]
    #[cfg(unix)]
    fn containment_holds_when_either_side_is_reached_through_a_symlink() {
        let d = TempDir::new().unwrap();
        let real = d.path().join("real-root");
        std::fs::create_dir_all(&real).unwrap();
        let link = d.path().join("link-root");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::fs::write(real.join(".env"), "x").unwrap();

        assert!(contained(&link, &real.join(".env")).is_ok(), "root via symlink");
        assert!(contained(&real, &link.join(".env")).is_ok(), "file via symlink");
    }

    /// The hook redacted whatever path the payload named. Each of these is a
    /// different way to name a file the workspace has no business rewriting.
    #[test]
    fn a_path_outside_the_root_is_refused_however_it_is_spelled() {
        let d = TempDir::new().unwrap();
        let root = d.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(d.path().join("outside.env"), "API_KEY=x").unwrap();

        let err = contained(&root, &d.path().join("outside.env")).unwrap_err();
        assert!(err.contains("outside the workspace root"), "{err}");

        // Spelled as a traversal from inside the root, resolving to that same file.
        let err = contained(&root, &root.join("../outside.env")).unwrap_err();
        assert!(err.contains("outside the workspace root"), "{err}");

        // Fail-closed: an unresolvable path is refused, not compared literally.
        assert!(contained(&root, &root.join("never-existed.env")).is_err());
    }

    /// The case a textual prefix check passes and a resolved one catches: a
    /// symlink *inside* the workspace pointing at a credential file outside it.
    #[test]
    #[cfg(unix)]
    fn a_symlink_out_of_the_workspace_is_not_inside_it() {
        let d = TempDir::new().unwrap();
        let root = d.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let outside = d.path().join("credentials");
        std::fs::write(&outside, "API_KEY=realsecret123\n").unwrap();
        let bait = root.join("innocent.env");
        std::os::unix::fs::symlink(&outside, &bait).unwrap();

        let err = contained(&root, &bait).unwrap_err();
        assert!(err.contains("outside the workspace root"), "{err}");
    }

    // ---------- task 4: placeholder resolution ----------

    #[test]
    fn resolve_placeholders_replaces_every_occurrence_and_counts_them() {
        let text = "A={{ws:secret:A}}\nB={{ws:secret:A}} {{ws:secret:B}}\n";
        let got = resolve_placeholders(text, |n| Ok(Some(format!("<{n}>")))).unwrap();
        assert_eq!(got.text, "A=<A>\nB=<A> <B>\n");
        assert_eq!(got.resolved, 3, "occurrences, not distinct names");
        assert!(got.missing.is_empty());
    }

    /// An unknown name must keep its placeholder — substituting an empty string
    /// or dropping the line silently corrupts the file — and be reported once
    /// however many times it appears, so the caller's error message is readable.
    #[test]
    fn an_unknown_name_keeps_its_placeholder_and_is_reported_once() {
        let text = "A={{ws:secret:A}}\nB={{ws:secret:GONE}}\nC={{ws:secret:GONE}}\n";
        let got = resolve_placeholders(text, |n| {
            Ok(if n == "A" { Some("v".to_string()) } else { None })
        })
        .unwrap();
        assert_eq!(got.text, "A=v\nB={{ws:secret:GONE}}\nC={{ws:secret:GONE}}\n");
        assert_eq!(got.resolved, 1);
        assert_eq!(got.missing, vec!["GONE".to_string()]);
    }

    /// A store error is not a missing secret. Degrading one to the other would
    /// tell the user their credential was never stored — sending them to
    /// re-create a value that is sitting in a store they mistyped the password
    /// for — and would write the half-resolved file back.
    #[test]
    fn a_lookup_error_aborts_the_file_rather_than_reading_as_missing() {
        // Matched rather than `unwrap_err`'d: `Restored` deliberately has no
        // `Debug`, because its `text` holds restored plaintext credentials and a
        // derived one would print them into any panic message.
        let err = match resolve_placeholders("A={{ws:secret:A}}\n", |_| {
            Err(anyhow::anyhow!("wrong password or corrupt secrets file"))
        }) {
            Err(e) => e,
            Ok(_) => panic!("a store error must not resolve to a rewritten file"),
        };
        assert!(err.to_string().contains("wrong password"), "{err}");
    }

    /// Braces this hook never wrote belong to somebody else's templating and
    /// must survive byte-for-byte — including a malformed one, which must not
    /// reach a store that validates names, and must not loop the scanner.
    #[test]
    fn markers_redaction_never_wrote_are_left_untouched() {
        let text = "x={{ws:secret:UNTERMINATED\ny={{ws:secret:not a name}}\nz={{ other }}\n";
        let got = resolve_placeholders(text, |_: &str| -> Result<Option<String>> {
            panic!("the store must not be consulted for a marker we did not write")
        })
        .unwrap();
        assert_eq!(got.text, text);
        assert_eq!(got.resolved, 0);
        assert!(got.missing.is_empty());
    }

    /// The round trip, at the level where both halves are visible: what
    /// `redact_file` writes is exactly what `resolve_placeholders` reads.
    #[test]
    fn the_placeholder_writer_and_reader_agree() {
        let written = placeholder_for("API_KEY");
        let got = resolve_placeholders(&written, |n| Ok(Some(format!("value-of-{n}")))).unwrap();
        assert_eq!(got.text, "value-of-API_KEY");
        assert_eq!(got.resolved, 1);
    }







}
