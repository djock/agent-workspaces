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
        assert_eq!(over_threshold(&snap(85.0, 50.0), 85, 90), Some("5h"));   // at threshold counts
        assert_eq!(over_threshold(&snap(50.0, 95.0), 85, 90), Some("week"));
        // 5h takes priority when both cross
        assert_eq!(over_threshold(&snap(90.0, 95.0), 85, 90), Some("5h"));
    }

    #[test]
    fn countdown_formats() {
        assert_eq!(countdown(1_000_000, 1_000_000 - 4800), "1h20m"); // 80 min
        assert_eq!(countdown(1_000_000, 1_000_000 - 45), "0h0m");
        assert_eq!(countdown(1_000_000, 1_000_000 + 10), "0m");      // already passed
        assert_eq!(countdown(0, 1_000_000), "0m");                    // unknown
    }

    #[test]
    fn read_missing_is_none() {
        assert!(read(std::path::Path::new("/no/such/limits.json")).is_none());
    }
}
