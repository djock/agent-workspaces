use anyhow::Result;
use std::path::PathBuf;

use crate::actors;
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

    // ONE mail scan, feeding both the rendered context and the watermark.
    let mail = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen());
    let (context, watermark) = session_context(&ws, mail);

    // One atomic_write per session start, not one per message: this is a hook
    // path and atomic_write fsyncs twice.
    if let Some(id) = watermark {
        let _ = crate::mail::mark_seen(&ws.mail_seen(), &id);
    }

    println!("{}", hookio::additional_context("SessionStart", &context));
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

/// The context injected at session start, plus the mail id to mark seen —
/// **both derived from the single `mail` scan passed in**.
///
/// I3: `session_start` used to render from one `mail::unread` scan and then
/// take a *second*, independent scan to pick the watermark. Every message that
/// landed in between — the whole of `build_context`: a README read, a notebook
/// directory walk, several stats — was marked read without ever being shown,
/// and there is no way to get it back. Mail arriving from a sibling workspace
/// or a cron while a session starts was silently swallowed.
///
/// The watermark is therefore the id of the newest message this function
/// actually rendered. Callers must not scan again; this function must not
/// scan at all.
fn session_context(
    ws: &Workspace,
    mail: Result<Vec<crate::mail::Message>, anyhow::Error>,
) -> (String, Option<String>) {
    let watermark = mail.as_ref().ok().and_then(|m| m.last().map(|x| x.id.clone()));
    (build_context(ws, mail), watermark)
}

fn build_context(ws: &Workspace, mail: Result<Vec<crate::mail::Message>, anyhow::Error>) -> String {
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

    // Unread mail, from the caller's single scan. A read error is reported,
    // not swallowed: silently showing an empty mailbox would be
    // indistinguishable from having no mail.
    match mail {
        Ok(msgs) if !msgs.is_empty() => {
            s.push_str(&format!("Unread mail ({}):\n", msgs.len()));
            for m in &msgs {
                s.push_str(&format!("- from {}: {}\n", m.from, m.body));
            }
            s.push('\n');
        }
        Ok(_) => {}
        Err(e) => {
            s.push_str(&format!("Mail could not be read ({e}); check .ws/mail/.\n\n"));
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

fn secret_redact() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if h.tool_name != "Write" && h.tool_name != "Edit" {
        return;
    }
    let path = std::path::PathBuf::from(&h.tool_input.file_path);
    if h.tool_input.file_path.is_empty() || !path.is_file() {
        return;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Cheap pre-scan (no store/keyring touch): is there any secret-looking line?
    let has_secret = text.lines().any(|line| {
        parse_assignment(line).is_some_and(|(name, value)| {
            name_looks_secret(name) && !value.is_empty() && !value.starts_with("{{ws:secret:")
        })
    });
    if !has_secret {
        return;
    }
    let store = match crate::secrets::open(&ws.name) {
        Ok(s) => s,
        Err(_) => return, // e.g. file backend without a password → skip silently
    };

    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    let mut redacted: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some((name, value)) = parse_assignment(line) {
            // NAME must look secret-ish (anchored KEY/TOKEN/SECRET/PASSWORD/PASSWD/API_KEY
            // patterns); a bare `.env` extension is not by itself enough — e.g. PORT=8080
            // and API_URL stay untouched.
            let looks_secret = name_looks_secret(name);
            let already = value.starts_with("{{ws:secret:");
            if looks_secret && !already && !value.is_empty() && store.set(name, value).is_ok() {
                // Preserve the original left-hand side and any surrounding quote.
                let (lhs, rhs) = line.split_once('=').unwrap(); // safe: parse_assignment matched
                let rhs_trim = rhs.trim();
                let quote = if (rhs_trim.starts_with('"') && rhs_trim.ends_with('"'))
                    || (rhs_trim.starts_with('\'') && rhs_trim.ends_with('\'')) {
                    rhs_trim.chars().next().unwrap().to_string()
                } else {
                    String::new()
                };
                out.push_str(lhs);
                out.push('=');
                out.push_str(&quote);
                out.push_str(&format!("{{{{ws:secret:{name}}}}}"));
                out.push_str(&quote);
                out.push('\n');
                redacted.push(name.to_string());
                changed = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if changed {
        // preserve trailing-newline shape roughly; atomic rewrite
        let _ = crate::atomic::atomic_write(&path, &out);
        let _ = note_manifest(&ws, &redacted);
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

fn name_looks_secret(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    u.contains("SECRET") || u.contains("TOKEN") || u.contains("PASSWORD") || u.contains("PASSWD")
        || u.contains("APIKEY") || u.contains("API_KEY")
        || u.ends_with("_KEY") || u == "KEY"
}

fn note_manifest(ws: &Workspace, names: &[String]) -> std::io::Result<()> {
    let path = ws.ws_dir().join("artifacts").join("MANIFEST.json");
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // Absent → start fresh; unreadable → refuse. Defaulting to an empty
    // manifest on a read error would write the new entry back over an empty
    // object, losing every `redacted_secrets` entry already recorded.
    let mut val: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({})),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    if !val.is_object() {
        val = serde_json::json!({});
    }
    let arr = val.as_object_mut().unwrap().entry("redacted_secrets").or_insert_with(|| serde_json::json!([]));
    if let Some(a) = arr.as_array_mut() {
        for n in names {
            a.push(serde_json::json!({ "name": n, "at": crate::now_iso() }));
        }
    }
    crate::atomic::atomic_write(&path, serde_json::to_string_pretty(&val)?)
        .map_err(|e| std::io::Error::other(e.to_string()))
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

    #[test]
    fn build_context_lists_unread_mail_and_says_nothing_when_there_is_none() {
        let td = TempDir::new().unwrap();
        let ws = Workspace { name: "proj".into(), root: td.path().to_path_buf() };
        std::fs::create_dir_all(ws.ws_dir()).unwrap();

        let scan = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen());
        let quiet = build_context(&ws, scan);
        assert!(!quiet.contains("Unread mail"), "no mail: no mail section");

        crate::mail::send(&ws.mail_dir(), "alice", "please review the plan").unwrap();
        let scan = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen());
        let loud = build_context(&ws, scan);
        assert!(loud.contains("Unread mail (1)"), "count is shown: {loud}");
        assert!(loud.contains("please review the plan"), "body is shown: {loud}");
        assert!(loud.contains("alice"), "sender is shown: {loud}");
    }

    /// I3, the real lost-mail path. `session_start` rendered from one mail
    /// scan and then marked seen from a *second*, independent one, so a
    /// message arriving between the two was marked read having never been
    /// shown. This test reproduces that window exactly: the scan is taken,
    /// then a message arrives, then the context is built.
    ///
    /// The discriminator is that `session_context` must derive the watermark
    /// from the vector it was **given**. Restore the second scan inside it and
    /// the watermark becomes `late`, both assertions below fail, and the
    /// message is gone.
    #[test]
    fn mail_arriving_after_the_scan_is_not_marked_seen() {
        let td = TempDir::new().unwrap();
        let ws = Workspace { name: "proj".into(), root: td.path().to_path_buf() };
        std::fs::create_dir_all(ws.ws_dir()).unwrap();
        std::fs::create_dir_all(ws.local_dir()).unwrap();

        let early = crate::mail::send(&ws.mail_dir(), "alice", "the early one").unwrap();
        // Exactly what session_start does: scan once, up front.
        let scan = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen());
        // ...and now mail lands while the context is still being assembled.
        let late = crate::mail::send(&ws.mail_dir(), "bob", "the late one").unwrap();
        assert_ne!(early, late);

        let (context, watermark) = session_context(&ws, scan);

        assert!(context.contains("the early one"), "the scanned message is rendered: {context}");
        assert!(
            !context.contains("the late one"),
            "the late message cannot have been rendered — it did not exist yet: {context}"
        );
        assert_eq!(
            watermark.as_deref(),
            Some(early.as_str()),
            "the watermark must be the newest message actually RENDERED, not the newest on disk"
        );

        // And the consequence that matters: after marking seen, the late
        // message is still waiting rather than silently consumed.
        crate::mail::mark_seen(&ws.mail_seen(), &watermark.unwrap()).unwrap();
        let still = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen()).unwrap();
        assert_eq!(still.len(), 1, "the unshown message must survive: {still:?}");
        assert_eq!(still[0].id, late);
        assert_eq!(still[0].body, "the late one");
    }

    /// The other half of the I3 constraint: at most ONE `atomic_write` per
    /// session start regardless of how many messages are unread. The
    /// watermark is a single id, so marking is a single write.
    #[test]
    fn many_unread_messages_produce_one_watermark_not_one_per_message() {
        let td = TempDir::new().unwrap();
        let ws = Workspace { name: "proj".into(), root: td.path().to_path_buf() };
        std::fs::create_dir_all(ws.ws_dir()).unwrap();
        std::fs::create_dir_all(ws.local_dir()).unwrap();

        let mut ids = Vec::new();
        for body in ["one", "two", "three"] {
            ids.push(crate::mail::send(&ws.mail_dir(), "alice", body).unwrap());
        }
        let scan = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen());
        let (context, watermark) = session_context(&ws, scan);

        assert!(context.contains("Unread mail (3)"), "all three are rendered: {context}");
        assert_eq!(
            watermark.as_deref(),
            ids.last().map(String::as_str),
            "one watermark, the newest rendered id"
        );
        crate::mail::mark_seen(&ws.mail_seen(), &watermark.unwrap()).unwrap();
        assert!(
            crate::mail::unread(&ws.mail_dir(), &ws.mail_seen()).unwrap().is_empty(),
            "one write clears all three"
        );
    }

    /// A mail read *error* must not produce a watermark: marking seen on the
    /// strength of a failed scan would consume mail nobody could read.
    #[test]
    fn an_unreadable_mailbox_yields_no_watermark() {
        let td = TempDir::new().unwrap();
        let ws = Workspace { name: "proj".into(), root: td.path().to_path_buf() };
        std::fs::create_dir_all(ws.ws_dir()).unwrap();
        std::fs::create_dir_all(ws.mail_dir()).unwrap();
        std::fs::write(ws.mail_dir().join("9-garbage.json"), "{not json").unwrap();

        let scan = crate::mail::unread(&ws.mail_dir(), &ws.mail_seen());
        assert!(scan.is_err(), "the fixture must actually fail to scan");
        let (context, watermark) = session_context(&ws, scan);

        assert_eq!(watermark, None, "a failed scan must never advance the watermark");
        assert!(context.contains("Mail could not be read"), "and it is reported: {context}");
    }
}
