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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
