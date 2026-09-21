use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Window {
    pub used_pct: f64,
    pub resets_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LimitsSnapshot {
    pub agent: String,
    pub five_hour: Window,
    pub seven_day: Window,
    pub stamped_at: i64,
}

pub fn global_path() -> PathBuf {
    crate::config::ws_config_dir().join("limits.json")
}

pub fn write(path: &Path, snap: &LimitsSnapshot) -> Result<()> {
    crate::atomic::atomic_write(path, serde_json::to_string_pretty(snap)?)?;
    Ok(())
}

pub fn read(path: &Path) -> Option<LimitsSnapshot> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn over_threshold(snap: &LimitsSnapshot, warn_5h: u8, warn_week: u8) -> Option<&'static str> {
    if snap.five_hour.used_pct >= warn_5h as f64 {
        return Some("5h");
    }
    if snap.seven_day.used_pct >= warn_week as f64 {
        return Some("week");
    }
    None
}

/// How much of a rollout's tail to scan for the latest rate-limit event. Codex
/// emits one with every `token_count`, so the newest is always near the end; a
/// long session's rollout runs to megabytes and is read on every Stop.
const ROLLOUT_TAIL_BYTES: u64 = 512 * 1024;

/// Codex's rate limits, read from its session rollout.
///
/// Codex has no statusline for ws to hook, so `limits.json` only ever holds
/// Claude's numbers — and a Stop hook in Codex that read it blocked Codex over
/// Claude's weekly window. Codex records its own windows in the rollout as
/// `event_msg`/`token_count` payloads (verified against Codex CLI 0.155.1):
/// `rate_limits.primary`/`secondary`, each `{used_percent, window_minutes,
/// resets_at}`. The windows are placed by `window_minutes` rather than by
/// primary/secondary, since those names say nothing about which is which.
pub fn from_codex_rollout(path: &Path) -> Option<LimitsSnapshot> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(ROLLOUT_TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let stamped_at = f
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Newest first; the first line may be cut mid-way by the seek and simply
    // fails to parse.
    text.lines()
        .rev()
        .filter(|l| l.contains("\"rate_limits\""))
        .find_map(|l| codex_windows(l, stamped_at))
}

fn codex_windows(line: &str, stamped_at: i64) -> Option<LimitsSnapshot> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let rl = v.get("payload")?.get("rate_limits")?;
    let mut snap = LimitsSnapshot { agent: "codex".into(), stamped_at, ..Default::default() };
    let mut found = false;
    for key in ["primary", "secondary"] {
        let Some(w) = rl.get(key).filter(|w| w.is_object()) else { continue };
        let window = Window {
            used_pct: w.get("used_percent")?.as_f64()?,
            resets_at: w.get("resets_at").and_then(|r| r.as_i64()).unwrap_or(0),
        };
        match w.get("window_minutes").and_then(|m| m.as_i64()) {
            Some(m) if m <= 300 => snap.five_hour = window,
            Some(_) => snap.seven_day = window,
            None => continue,
        }
        found = true;
    }
    found.then_some(snap)
}

pub fn countdown(resets_at: i64, now: i64) -> String {
    if resets_at <= 0 || resets_at <= now {
        return "0m".to_string();
    }
    let secs = resets_at - now;
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    // The weekly window runs to three digits of hours, which reads as a number
    // you have to divide yourself. Days only appear once there is one to show,
    // so the 5-hour countdown keeps its compact two-part form.
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else {
        format!("{h}h{m}m")
    }
}

/// Beyond this, a snapshot is reported as stale rather than current.
///
/// The 5-hour window is the shortest thing these numbers describe, so anything
/// older than roughly one window has had time to reset completely and the
/// percentages may bear no relation to reality. `stamped_at` was always recorded
/// and never read, which meant `-limits` printed week-old figures in exactly the
/// same format as live ones.
pub const STALE_AFTER_SECS: i64 = 5 * 3600;

/// How old a snapshot is, and whether that is too old to present as current.
/// `None` when the snapshot has no usable timestamp — treated as stale, since an
/// unknown age is not evidence of freshness.
pub fn age_secs(snap: &LimitsSnapshot, now: i64) -> Option<i64> {
    if snap.stamped_at <= 0 || now < snap.stamped_at {
        return None;
    }
    Some(now - snap.stamped_at)
}

pub fn is_stale(snap: &LimitsSnapshot, now: i64) -> bool {
    match age_secs(snap, now) {
        Some(age) => age > STALE_AFTER_SECS,
        None => true,
    }
}

/// "3h20m" / "2d4h" for display next to a stale reading.
pub fn humanize_age(secs: i64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else {
        format!("{m}m")
    }
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn snap(five: f64, week: f64) -> LimitsSnapshot {
        LimitsSnapshot {
            agent: "claude".into(),
            five_hour: Window { used_pct: five, resets_at: 1_000_000 },
            seven_day: Window { used_pct: week, resets_at: 2_000_000 },
            stamped_at: 500_000,
        }
    }

    #[test]
    fn write_then_read_roundtrips() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("limits.json");
        let s = snap(43.0, 61.0);
        write(&p, &s).unwrap();
        let back = read(&p).unwrap();
        assert_eq!(back.five_hour.used_pct, 43.0);
        assert_eq!(back.seven_day.resets_at, 2_000_000);
        assert_eq!(back.agent, "claude");
    }

    #[test]
    fn threshold_detection() {
        assert_eq!(over_threshold(&snap(50.0, 50.0), 85, 90), None);
        assert_eq!(over_threshold(&snap(85.0, 50.0), 85, 90), Some("5h")); // at threshold counts
        assert_eq!(over_threshold(&snap(50.0, 95.0), 85, 90), Some("week"));
        // 5h takes priority when both cross
        assert_eq!(over_threshold(&snap(90.0, 95.0), 85, 90), Some("5h"));
    }

    #[test]
    fn countdown_formats() {
        assert_eq!(countdown(1_000_000, 1_000_000 - 4800), "1h20m"); // 80 min
        assert_eq!(
            countdown(1_000_000, 1_000_000 - (2 * 86_400 + 12 * 3600 + 34 * 60)),
            "2d 12h 34m"
        );
        assert_eq!(countdown(1_000_000, 1_000_000 - (86_400 - 60)), "23h59m"); // just under a day
        assert_eq!(countdown(1_000_000, 1_000_000 - 86_400), "1d 0h 0m");
        assert_eq!(countdown(1_000_000, 1_000_000 - 45), "0h0m");
        assert_eq!(countdown(1_000_000, 1_000_000 + 10), "0m"); // already passed
        assert_eq!(countdown(0, 1_000_000), "0m"); // unknown
    }

    #[test]
    fn read_missing_is_none() {
        assert!(read(std::path::Path::new("/no/such/limits.json")).is_none());
    }

    /// `stamped_at` was recorded and never read, so `-limits` printed a week-old
    /// reading in the same format as a live one. The boundary is the 5-hour
    /// window: past it, the numbers describe a window that has had time to reset.
    #[test]
    fn staleness_is_judged_against_the_window_the_numbers_describe() {
        let s = snap(40.0, 50.0); // stamped_at = 500_000
        assert!(!is_stale(&s, 500_000), "just written is fresh");
        assert!(!is_stale(&s, 500_000 + STALE_AFTER_SECS), "at the boundary is still fresh");
        assert!(is_stale(&s, 500_000 + STALE_AFTER_SECS + 1), "one second past is stale");
        assert!(is_stale(&s, 500_000 + 7 * 86_400), "a week old is stale");
    }

    /// An unknown or impossible age must read as stale, not fresh. A missing
    /// `stamped_at` deserialises to 0, and `now_iso`'s old empty-string failure
    /// mode shows this family of bug is not hypothetical.
    #[test]
    fn an_unusable_timestamp_is_treated_as_stale_not_fresh() {
        let mut s = snap(40.0, 50.0);
        s.stamped_at = 0;
        assert_eq!(age_secs(&s, 1_000_000), None);
        assert!(is_stale(&s, 1_000_000), "no timestamp is not evidence of freshness");

        // A clock that moved backwards, or a snapshot from another machine.
        let s2 = snap(40.0, 50.0);
        assert_eq!(age_secs(&s2, 400_000), None, "future stamp has no meaningful age");
        assert!(is_stale(&s2, 400_000));
    }

    /// The shape Codex CLI 0.155.1 writes, trimmed. Windows are placed by
    /// `window_minutes`, and the newest event wins over older ones.
    #[test]
    fn codex_rollout_yields_the_newest_windows() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("rollout.jsonl");
        let ev = |five: f64, week: f64| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"limit_id":"codex","primary":{{"used_percent":{five},"window_minutes":300,"resets_at":111}},"secondary":{{"used_percent":{week},"window_minutes":10080,"resets_at":222}}}}}}}}"#
            )
        };
        let body = format!(
            "{}\n{{\"type\":\"response_item\"}}\n{}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"rate_limits\":null}}}}\n",
            ev(90.0, 95.0),
            ev(8.0, 1.0)
        );
        std::fs::write(&p, body).unwrap();
        let s = from_codex_rollout(&p).unwrap();
        assert_eq!(s.agent, "codex");
        assert_eq!(s.five_hour.used_pct, 8.0);
        assert_eq!(s.five_hour.resets_at, 111);
        assert_eq!(s.seven_day.used_pct, 1.0);
        assert_eq!(s.seven_day.resets_at, 222);
    }

    #[test]
    fn codex_rollout_without_limits_is_none() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("rollout.jsonl");
        std::fs::write(&p, "{\"type\":\"session_meta\"}\n").unwrap();
        assert!(from_codex_rollout(&p).is_none());
        assert!(from_codex_rollout(&d.path().join("missing.jsonl")).is_none());
    }

    #[test]
    fn age_is_humanized_at_each_scale() {
        assert_eq!(humanize_age(30), "30s");
        assert_eq!(humanize_age(90), "1m");
        assert_eq!(humanize_age(3 * 3600 + 20 * 60), "3h20m");
        assert_eq!(humanize_age(2 * 86_400 + 4 * 3600), "2d4h");
    }
}
