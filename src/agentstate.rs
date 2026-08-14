//! What each running agent is actually doing.
//!
//! `ws` could tell you a workspace was alive — it holds a lock — but nothing
//! about what was happening in it, so three live workspaces rendered as three
//! identical rows and the objective text was the only way to tell them apart.
//!
//! Claude Code publishes a record per running session under
//! `~/.claude/sessions/`, carrying the session's pid, its working directory and
//! a live status (`busy`, `waiting`, `idle`). This reads them.
//!
//! **A record outlives a crash**, so one is never trusted on sight: the pid must
//! still be alive *and* the process must still report the start time the record
//! holds. Without the second half, a pid the kernel has since handed to
//! something else keeps a dead session looking busy — and pids are reused within
//! hours on a busy machine, not within years.
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Record {
    pid: u32,
    cwd: String,
    /// The process start time, as the platform prints it. Compared as text
    /// against `ps`, which is where it came from.
    #[serde(default, rename = "procStart")]
    proc_start: String,
    #[serde(default)]
    status: String,
    #[serde(default, rename = "waitingFor")]
    waiting_for: String,
}

/// What one live session is doing.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentState {
    pub pid: u32,
    /// `busy`, `waiting`, `idle` — whatever the agent published, sanitized.
    pub status: String,
    /// What it is waiting for, when it says.
    pub waiting_for: Option<String>,
}

impl AgentState {
    /// One short label for a list row.
    pub fn label(&self) -> String {
        match self.waiting_for.as_deref() {
            Some(w) if self.status == "waiting" => format!("{} ({w})", self.status),
            _ => self.status.clone(),
        }
    }
}

fn sessions_dir() -> PathBuf {
    // Overridable so the tests can point at a directory they control; this is
    // Claude Code's directory, not ws's, so ws never writes to it.
    if let Ok(d) = std::env::var("WS_AGENT_SESSIONS_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    dirs::home_dir().unwrap_or_default().join(".claude").join("sessions")
}

/// Every live session's state, keyed by the directory it is running in.
///
/// Costs a flat two passes however many sessions are running: one directory
/// read, and one `ps` for all the pids at once. Asking `ps` per session made
/// this scale with the number of live agents, on a path the picker runs on every
/// repaint.
pub fn by_directory() -> HashMap<PathBuf, AgentState> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return out };

    let records: Vec<Record> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            // A record ws cannot parse is skipped, not fatal: the file belongs
            // to another program, whose format may change, and a listing that
            // dies because of it would be worse than one missing a column.
            serde_json::from_str::<Record>(&std::fs::read_to_string(e.path()).ok()?).ok()
        })
        .filter(|r| !r.cwd.is_empty() && r.pid != 0)
        .collect();
    if records.is_empty() {
        return out;
    }

    let starts = process_starts(&records.iter().map(|r| r.pid).collect::<Vec<_>>());
    for r in records {
        // Both halves: alive, and still the same process. A record whose
        // `procStart` is empty (an older agent build) is trusted on liveness
        // alone rather than dropped — the check exists to catch pid reuse, and
        // refusing to say anything is not the safer answer for a display.
        let Some(actual) = starts.get(&r.pid) else { continue };
        if !r.proc_start.is_empty() && actual != &r.proc_start {
            continue;
        }
        let status = crate::term::display_safe(&r.status);
        if status.is_empty() {
            continue;
        }
        let waiting = crate::term::display_safe(&r.waiting_for);
        out.insert(
            PathBuf::from(&r.cwd),
            AgentState {
                pid: r.pid,
                status,
                waiting_for: (!waiting.is_empty()).then_some(waiting),
            },
        );
    }
    out
}

/// Start times for the pids that are still running, from one `ps`.
///
/// The format is whatever `ps -o lstart=` prints, which is exactly what the
/// agent recorded — comparing the two as text needs no date parsing and cannot
/// disagree with itself about time zones.
fn process_starts(pids: &[u32]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let Ok(o) = std::process::Command::new("ps").args(["-o", "pid=,lstart=", "-p", &list]).output()
    else {
        return out;
    };
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let line = line.trim();
        let Some((pid, start)) = line.split_once(char::is_whitespace) else { continue };
        if let Ok(pid) = pid.trim().parse::<u32>() {
            out.insert(pid, start.trim().to_string());
        }
    }
    out
}

/// The state of the agent running in `root`, if one is.
pub fn for_directory(states: &HashMap<PathBuf, AgentState>, root: &Path) -> Option<AgentState> {
    if let Some(s) = states.get(root) {
        return Some(s.clone());
    }
    // A workspace reached through a symlink — `/var` against `/private/var` on
    // macOS is the everyday case — is the same directory the agent recorded.
    let canonical = root.canonicalize().ok()?;
    states
        .iter()
        .find(|(dir, _)| dir.canonicalize().map(|c| c == canonical).unwrap_or(false))
        .map(|(_, s)| s.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write a session record like Claude Code's, for `pid`.
    fn record(dir: &Path, pid: u32, cwd: &str, status: &str, proc_start: &str) {
        std::fs::write(
            dir.join(format!("{pid}.json")),
            serde_json::json!({
                "pid": pid,
                "sessionId": "abc",
                "cwd": cwd,
                "procStart": proc_start,
                "status": status,
                "waitingFor": "input needed",
            })
            .to_string(),
        )
        .unwrap();
    }

    /// This process's own start time, as `ps` prints it — the value a live
    /// record would carry.
    fn my_start() -> String {
        process_starts(&[std::process::id()]).get(&std::process::id()).cloned().unwrap()
    }

    fn with_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        std::env::set_var("WS_AGENT_SESSIONS_DIR", dir);
        let out = f();
        std::env::remove_var("WS_AGENT_SESSIONS_DIR");
        out
    }

    #[test]
    fn a_live_session_reports_what_it_is_doing() {
        let d = TempDir::new().unwrap();
        record(d.path(), std::process::id(), "/tmp/proj", "waiting", &my_start());

        let states = with_dir(d.path(), by_directory);
        let s = states.get(Path::new("/tmp/proj")).expect("the live session");
        assert_eq!(s.status, "waiting");
        assert_eq!(s.label(), "waiting (input needed)");
    }

    /// A record outlives the process that wrote it, and pids are reused within
    /// hours on a busy machine. Liveness alone would keep a dead session looking
    /// busy the moment the kernel hands its pid to something else.
    #[test]
    fn a_record_whose_pid_belongs_to_another_process_is_ignored() {
        let d = TempDir::new().unwrap();
        // This pid *is* alive, but the recorded start time is not its own.
        record(d.path(), std::process::id(), "/tmp/proj", "busy", "Mon Jan  1 00:00:00 2001");

        let states = with_dir(d.path(), by_directory);
        assert!(states.is_empty(), "a reused pid must not report the old session's state");
    }

    #[test]
    fn a_record_for_a_dead_process_is_ignored() {
        let d = TempDir::new().unwrap();
        record(d.path(), 999_999, "/tmp/proj", "busy", "Mon Jan  1 00:00:00 2001");
        assert!(with_dir(d.path(), by_directory).is_empty());
    }

    /// The file belongs to another program, whose format may change. One
    /// unreadable record must not take the rest of the listing down with it —
    /// the shape of failure cs hit when a single `jq` invocation abandoned the
    /// whole run at the first parse error.
    #[test]
    fn one_unparseable_record_does_not_hide_the_others() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("garbage.json"), "{not json").unwrap();
        record(d.path(), std::process::id(), "/tmp/proj", "busy", &my_start());

        let states = with_dir(d.path(), by_directory);
        assert_eq!(states.len(), 1, "the intact record must still be read");
    }

    /// The record is written by another program and rendered on this terminal.
    #[test]
    fn a_status_cannot_carry_a_control_sequence_to_the_terminal() {
        let d = TempDir::new().unwrap();
        record(d.path(), std::process::id(), "/tmp/proj", "busy\x1b[2J", &my_start());

        let states = with_dir(d.path(), by_directory);
        let s = states.get(Path::new("/tmp/proj")).unwrap();
        assert!(!s.status.contains('\u{1b}'), "{:?}", s.status);
    }

    #[test]
    fn no_sessions_directory_means_no_state_and_no_error() {
        let d = TempDir::new().unwrap();
        assert!(with_dir(&d.path().join("nope"), by_directory).is_empty());
    }

    /// `/var` against `/private/var` on macOS is the everyday case, not an
    /// exotic one: the agent records one spelling and ws holds the other.
    #[test]
    fn a_directory_reached_through_a_symlink_still_matches() {
        let d = TempDir::new().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut states = HashMap::new();
        states
            .insert(real.clone(), AgentState { pid: 1, status: "busy".into(), waiting_for: None });

        assert!(for_directory(&states, &link).is_some(), "the same directory, spelled differently");
    }
}
