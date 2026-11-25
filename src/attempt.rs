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

/// Remove the attempt only if it is still `id`: a late or repeated hook must
/// never clear a newer launch's record.
fn clear_if(ws: &Workspace, id: &str) {
    if read(ws).is_some_and(|a| a.id == id) {
        let _ = std::fs::remove_file(path(ws));
    }
}

/// The attempt this hook process belongs to: the id in its environment, for the
/// agent it runs under. Anything else (a /clear later in the same session, a
/// stray hook from an older launch) matches nothing.
pub fn for_hook(ws: &Workspace) -> Option<Attempt> {
    let id = std::env::var(ENV).ok().filter(|s| !s.is_empty())?;
    let agent = std::env::var("WS_AGENT").ok().filter(|s| !s.is_empty())?;
    read(ws).filter(|a| a.id == id && a.agent == agent)
}

/// The agent started: commit what the launch only intended. Called once per
/// attempt; the record is removed here, so a replayed SessionStart finds none
/// and commits nothing twice.
pub fn confirm(ws: &Workspace, a: &Attempt, session_id: &str) {
    let actor = crate::actors::actor_slug_in(&ws.root);
    if let Some(want) = &a.resume_id {
        if want != session_id {
            let _ = crate::internal::append_log(
                ws,
                &format!("launch asked to resume {want} but {} reported {session_id}", a.agent),
            );
        }
    }
    if let Some(from) = &a.from_agent {
        let _ = crate::meta::set_default_agent(&ws.workspace_toml(), &a.agent);
        let _ = crate::timeline::record(
            &ws.timeline(),
            "agent-switch",
            &actor,
            serde_json::json!({ "from": from, "to": a.agent, "handoff": a.hint }),
        );
    }
    if let Some(name) = &a.handoff {
        // Only the handoff this launch accepted: a newer one armed while the
        // agent was starting stays armed.
        if let Some(armed) = crate::handoff::armed(ws).filter(|x| x.name() == name) {
            crate::handoff::disarm(ws, &armed);
        }
        let _ = crate::timeline::record(
            &ws.timeline(),
            "handoff-consumed",
            &actor,
            serde_json::json!({ "file": name, "source": "launch" }),
        );
    }
    if a.preallocated.is_some() {
        let _ = crate::contract::update_state(&ws.state_toml(), |t| {
            if let Some(e) = t.get_mut(&a.agent).and_then(|v| v.as_table_mut()) {
                e.remove("pending_session_id");
            }
        });
    }
    clear_if(ws, &a.id);
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
