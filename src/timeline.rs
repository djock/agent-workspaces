use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

/// Append one event as a JSON line to `timeline_path`. The line is an object
/// with ts (ISO-8601 UTC), kind, actor, merged with the fields of `extra`
/// (which must be a JSON object or Null). Best-effort creation of the parent dir.
pub fn record(timeline_path: &Path, kind: &str, actor: &str, extra: Value) -> Result<()> {
    let mut event = json!({
        "ts": crate::now_iso(),
        "kind": kind,
        "actor": actor,
    });
    if let Value::Object(extra_map) = extra {
        if let Value::Object(base) = &mut event {
            for (k, v) in extra_map {
                base.insert(k, v);
            }
        }
    }
    if let Some(dir) = timeline_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(&event)?;
    line.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(timeline_path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// What one actor did in a workspace.
#[derive(Debug, PartialEq)]
pub struct ActorSummary {
    pub actor: String,
    pub events: usize,
    /// Distinct event kinds, in first-seen order — this is the "what did they do"
    /// half, which a commit count cannot answer.
    pub kinds: Vec<String>,
    pub first: String,
    pub last: String,
}

/// Aggregate the timeline per actor, busiest first.
///
/// Absent → nobody has done anything yet (the caller falls back to the commit
/// ranking). Unreadable → refuse: reporting "no activity" for a file we could not
/// open would be a lie in the one command whose whole job is attribution.
/// Individual malformed lines are skipped rather than fatal — the file is
/// append-only from several processes and one torn line must not hide the rest.
pub fn by_actor(timeline_path: &Path) -> Result<Vec<ActorSummary>> {
    let raw = match crate::io_read::read_or_absent(timeline_path)? {
        None => return Ok(Vec::new()),
        Some(s) => s,
    };

    let mut order: Vec<String> = Vec::new();
    let mut acc: std::collections::HashMap<String, ActorSummary> = std::collections::HashMap::new();

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let actor = v.get("actor").and_then(|a| a.as_str()).unwrap_or("unknown").to_string();
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("?").to_string();
        let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("").to_string();

        match acc.get_mut(&actor) {
            Some(e) => {
                e.events += 1;
                if !e.kinds.contains(&kind) {
                    e.kinds.push(kind);
                }
                // The file is append-ordered, but `ts` can be equal or (across
                // processes) slightly out of order, so take the extremes rather
                // than assuming the last line is the latest.
                if !ts.is_empty() {
                    if e.first.is_empty() || ts < e.first {
                        e.first = ts.clone();
                    }
                    if ts > e.last {
                        e.last = ts;
                    }
                }
            }
            None => {
                order.push(actor.clone());
                acc.insert(
                    actor.clone(),
                    ActorSummary {
                        actor,
                        events: 1,
                        kinds: vec![kind],
                        first: ts.clone(),
                        last: ts,
                    },
                );
            }
        }
    }

    let mut out: Vec<ActorSummary> = order.into_iter().filter_map(|a| acc.remove(&a)).collect();
    // Busiest first; ties keep first-seen order so the output is deterministic.
    out.sort_by_key(|a| std::cmp::Reverse(a.events));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn by_actor_is_empty_for_an_absent_timeline() {
        let d = TempDir::new().unwrap();
        assert!(by_actor(&d.path().join("timeline.jsonl")).unwrap().is_empty());
    }

    #[test]
    fn by_actor_counts_events_and_collects_kinds() {
        let d = TempDir::new().unwrap();
        let tl = d.path().join("timeline.jsonl");
        record(&tl, "created", "alice", Value::Null).unwrap();
        record(&tl, "opened", "alice", Value::Null).unwrap();
        record(&tl, "opened", "alice", Value::Null).unwrap();
        record(&tl, "opened", "bob", Value::Null).unwrap();

        let out = by_actor(&tl).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].actor, "alice", "busiest first");
        assert_eq!(out[0].events, 3);
        assert_eq!(out[0].kinds, vec!["created", "opened"], "distinct, first-seen order");
        assert_eq!(out[1].actor, "bob");
        assert_eq!(out[1].events, 1);
    }

    #[test]
    fn by_actor_records_the_time_span() {
        let d = TempDir::new().unwrap();
        let tl = d.path().join("timeline.jsonl");
        std::fs::write(
            &tl,
            "{\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"a\"}\n\
             {\"ts\":\"2026-07-01T00:00:00Z\",\"kind\":\"closed\",\"actor\":\"a\"}\n",
        )
        .unwrap();
        let out = by_actor(&tl).unwrap();
        assert_eq!(out[0].first, "2026-07-01T00:00:00Z", "earliest, not first line");
        assert_eq!(out[0].last, "2026-07-02T00:00:00Z");
    }

    /// The timeline is appended by several processes; one torn line must not hide
    /// every other actor's work.
    #[test]
    fn by_actor_skips_a_corrupt_line_and_keeps_the_rest() {
        let d = TempDir::new().unwrap();
        let tl = d.path().join("timeline.jsonl");
        std::fs::write(
            &tl,
            "{\"ts\":\"2026-07-01T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"a\"}\n\
             {this is torn\n\
             {\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"b\"}\n",
        )
        .unwrap();
        let out = by_actor(&tl).unwrap();
        assert_eq!(out.len(), 2, "both intact actors survive");
    }

    #[test]
    fn an_event_with_no_actor_is_attributed_to_unknown_not_dropped() {
        let d = TempDir::new().unwrap();
        let tl = d.path().join("timeline.jsonl");
        std::fs::write(&tl, "{\"ts\":\"2026-07-01T00:00:00Z\",\"kind\":\"opened\"}\n").unwrap();
        let out = by_actor(&tl).unwrap();
        assert_eq!(out[0].actor, "unknown");
    }

    #[test]
    fn appends_json_lines() {
        let d = TempDir::new().unwrap();
        let tl = d.path().join("timeline.jsonl");
        record(&tl, "created", "alice", serde_json::json!({"agent":"claude"})).unwrap();
        record(&tl, "opened", "alice", serde_json::Value::Null).unwrap();

        let body = std::fs::read_to_string(&tl).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["kind"], "created");
        assert_eq!(first["actor"], "alice");
        assert_eq!(first["agent"], "claude");
        assert!(first["ts"].as_str().unwrap().ends_with('Z'));

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["kind"], "opened");
    }
}
