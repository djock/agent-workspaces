//! A launch that has not reported back yet.
//!
//! `ws <name>` used to commit everything before the agent ran: the new
//! `default_agent`, the `agent-switch` row, the consumed handoff and, for a
//! fresh Claude launch, the new session id. `exec` never returns, so an agent
//! that failed to start left the workspace switched to it, the handoff gone and
//! the previous conversation no longer the one a bare launch resumes.
//!
//! Now the launcher writes down what it intends under the workspace lock, hands
//! the attempt id to the child, and the agent's own SessionStart hook commits
//! it ([`confirm`]). A launch that never reports back leaves the workspace as
//! it was; the next launch says so ([`stale_notice`]).

use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;

/// The child sees its attempt id here.
pub const ENV: &str = "WS_LAUNCH_ATTEMPT";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub agent: String,
    /// The agent this launch switches away from, when it is a switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_agent: Option<String>,
    /// The conversation an exact resume asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_id: Option<String>,
    /// The id a fresh Claude launch passed as `--session-id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preallocated: Option<String>,
    /// The armed handoff the user accepted at the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
    /// The handoff file the launch pointed the agent at (context hint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub started: i64,
}

fn path(ws: &Workspace) -> std::path::PathBuf {
    ws.local_dir().join("launch-attempt.toml")
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn begin(ws: &Workspace, a: &Attempt) -> anyhow::Result<()> {
    crate::atomic::atomic_write(&path(ws), toml::to_string(a)?)
}

pub fn read(ws: &Workspace) -> Option<Attempt> {
    toml::from_str(&std::fs::read_to_string(path(ws)).ok()?).ok()
}

/// The attempt this hook process belongs to: the id in its environment, for the
/// agent it runs under. Anything else (a /clear later in the same session, a
/// stray hook from an older launch) matches nothing.
pub fn for_hook(ws: &Workspace) -> Option<Attempt> {
    let id = std::env::var(ENV).ok().filter(|s| !s.is_empty())?;
    let agent = std::env::var("WS_AGENT").ok().filter(|s| !s.is_empty())?;
    read(ws).filter(|a| a.id == id && a.agent == agent)
}

/// Whether `session_id` is the conversation this launch asked for. An exact
/// resume or a preallocated Claude id names it; a fresh Codex or agy launch
/// learns it from the hook, so any non-empty id is that conversation.
pub fn accepts(a: &Attempt, session_id: &str) -> bool {
    let id = session_id.trim();
    !id.is_empty() && a.resume_id.as_ref().or(a.preallocated.as_ref()).is_none_or(|want| want == id)
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Confirmed,
    /// The hook reported a different (or no) conversation: nothing committed,
    /// the attempt stays for a matching event or a retry.
    Mismatch,
    /// Another hook already confirmed it, or a newer launch replaced it.
    Gone,
}

/// The agent started: commit what the launch only intended.
///
/// Matching and finishing happen under one lock on the attempt file, so two
/// copies of the same hook cannot both commit. Every required write returns
/// its error and leaves the attempt in place to retry; each timeline row
/// carries the attempt id, so a retry after a partial success does not record
/// the switch or the consumption twice.
pub fn confirm(ws: &Workspace, a: &Attempt, session_id: &str) -> anyhow::Result<Outcome> {
    crate::txn::transaction(&path(ws), || {
        let Some(cur) = read(ws).filter(|c| c.id == a.id) else { return Ok(Outcome::Gone) };
        if !accepts(&cur, session_id) {
            let _ = crate::internal::append_log(
                ws,
                &format!(
                    "launch {} of {} expected {} but the hook reported {:?}; still pending",
                    cur.id,
                    cur.agent,
                    cur.resume_id.as_ref().or(cur.preallocated.as_ref()).map_or("an id", |s| s),
                    session_id
                ),
            );
            return Ok(Outcome::Mismatch);
        }
        let actor = crate::actors::actor_slug_in(&ws.root);
        let done = |kind: &str| recorded(ws, kind, &cur.id);
        if let Some(from) = &cur.from_agent {
            crate::meta::set_default_agent(&ws.workspace_toml(), &cur.agent)?;
            if !done("agent-switch") {
                crate::timeline::record(
                    &ws.timeline(),
                    "agent-switch",
                    &actor,
                    serde_json::json!({
                        "from": from, "to": cur.agent, "handoff": cur.hint, "attempt": cur.id,
                    }),
                )?;
            }
        }
        if let Some(name) = &cur.handoff {
            // Only the handoff this launch accepted: a newer one armed while the
            // agent was starting stays armed.
            if let Some(armed) = crate::handoff::armed(ws).filter(|x| x.name() == name) {
                crate::handoff::disarm(ws, &armed);
            }
            if !done("handoff-consumed") {
                crate::timeline::record(
                    &ws.timeline(),
                    "handoff-consumed",
                    &actor,
                    serde_json::json!({ "file": name, "source": "launch", "attempt": cur.id }),
                )?;
            }
        }
        if cur.preallocated.is_some() {
            crate::contract::update_state(&ws.state_toml(), |t| {
                if let Some(e) = t.get_mut(&cur.agent).and_then(|v| v.as_table_mut()) {
                    e.remove("pending_session_id");
                }
            })?;
        }
        match std::fs::remove_file(path(ws)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Outcome::Confirmed)
    })
}

/// Whether the timeline already holds a `kind` row for this attempt.
fn recorded(ws: &Workspace, kind: &str, attempt: &str) -> bool {
    std::fs::read_to_string(ws.timeline()).unwrap_or_default().lines().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .is_ok_and(|v| v["kind"] == kind && v["attempt"].as_str() == Some(attempt))
    })
}

/// A previous launch that never reported back, told to the user before the
/// next one replaces its record.
pub fn stale_notice(ws: &Workspace) -> Option<String> {
    let a = read(ws)?;
    let age = (crate::time::now_unix() - a.started).max(0) as u64;
    let mut s = format!(
        "ws: the last launch of {} ({} ago) never reported that it started",
        a.agent,
        crate::handoff::age_label(age)
    );
    if let Some(from) = &a.from_agent {
        s.push_str(&format!(", so {} is still on {from}", ws.name));
    }
    if let Some(h) = &a.handoff {
        s.push_str(&format!("; the handoff {h} is still waiting"));
    }
    s.push_str(
        ". If it did start, its SessionStart hook is not running: check `ws -doctor` \
         (Codex needs its hooks trusted via /hooks).\n",
    );
    Some(s)
}
