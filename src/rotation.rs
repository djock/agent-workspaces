//! How full the current conversation is, as the status line last saw it.
//!
//! Claude Code gives the context % only to the status line, which repaints
//! about once a second. The Stop hook needs the same number, so the status line
//! stamps it here, keyed by the conversation's session id. The key matters: a
//! teammate or a second conversation in the same workspace writes its own id,
//! and a reading for another conversation must never trigger this one's nudge.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::workspace::Workspace;

/// How old a reading may be before it is ignored. The status line renders
/// continuously while a conversation is open, so an older one describes a
/// conversation nobody is looking at.
pub const READING_FRESH_SECS: i64 = 600;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextReading {
    pub session_id: String,
    pub pct: u8,
    pub stamped_at: i64,
}

fn reading_path(ws: &Workspace) -> PathBuf {
    ws.local_dir().join("context.json")
}

pub fn write_reading(ws: &Workspace, session_id: &str, used_pct: f64) -> Result<()> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Ok(()); // nothing to key it by; a reading for "anyone" is worse than none
    }
    // `as u8` saturates and maps NaN to 0; the clamp is for readability.
    let pct = used_pct.round().clamp(0.0, 100.0) as u8;
    let r = ContextReading {
        session_id: session_id.to_string(),
        pct,
        stamped_at: crate::limits::now_epoch(),
    };
    crate::atomic::atomic_write(&reading_path(ws), serde_json::to_string(&r)?)
}

#[cfg_attr(not(test), allow(dead_code))] // read by the Stop hook (Task 3)
pub fn reading_for(ws: &Workspace, session_id: &str, now: i64) -> Option<u8> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let raw = std::fs::read_to_string(reading_path(ws)).ok()?;
    let r: ContextReading = serde_json::from_str(&raw).ok()?;
    let fresh = now - r.stamped_at <= READING_FRESH_SECS;
    (r.session_id == session_id && fresh).then_some(r.pct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(td: &TempDir) -> Workspace {
        Workspace { name: "t".into(), root: td.path().to_path_buf() }
    }

    #[test]
    fn a_reading_is_returned_for_its_own_session() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "abc", 66.6).unwrap();
        let now = crate::limits::now_epoch();
        assert_eq!(reading_for(&w, "abc", now), Some(67));
    }

    #[test]
    fn another_sessions_reading_is_not_this_ones() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "teammate", 90.0).unwrap();
        assert_eq!(reading_for(&w, "lead", crate::limits::now_epoch()), None);
    }

    #[test]
    fn an_empty_session_id_writes_nothing_and_matches_nothing() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "  ", 90.0).unwrap();
        assert!(!w.local_dir().join("context.json").exists());
        assert_eq!(reading_for(&w, "", crate::limits::now_epoch()), None);
    }

    #[test]
    fn a_stale_reading_is_ignored() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        write_reading(&w, "abc", 90.0).unwrap();
        let later = crate::limits::now_epoch() + READING_FRESH_SECS + 1;
        assert_eq!(reading_for(&w, "abc", later), None);
    }

    #[test]
    fn out_of_range_percentages_are_clamped() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let now = crate::limits::now_epoch();
        write_reading(&w, "abc", 250.0).unwrap();
        assert_eq!(reading_for(&w, "abc", now), Some(100));
        write_reading(&w, "abc", f64::NAN).unwrap();
        assert_eq!(reading_for(&w, "abc", now), Some(0));
    }

    #[test]
    fn a_corrupt_reading_reads_as_none() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        std::fs::create_dir_all(w.local_dir()).unwrap();
        std::fs::write(w.local_dir().join("context.json"), "{not json").unwrap();
        assert_eq!(reading_for(&w, "abc", crate::limits::now_epoch()), None);
    }
}
