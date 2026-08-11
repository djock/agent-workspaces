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

pub fn countdown(resets_at: i64, now: i64) -> String {
    if resets_at <= 0 || resets_at <= now {
        return "0m".to_string();
    }
    let secs = resets_at - now;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    format!("{h}h{m}m")
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

    #[test]
    fn age_is_humanized_at_each_scale() {
        assert_eq!(humanize_age(30), "30s");
        assert_eq!(humanize_age(90), "1m");
        assert_eq!(humanize_age(3 * 3600 + 20 * 60), "3h20m");
        assert_eq!(humanize_age(2 * 86_400 + 4 * 3600), "2d4h");
    }
}
