use std::io::Write;

use serde::Deserialize;

use crate::limits::{self, LimitsSnapshot, Window};

#[derive(Debug, Default, Deserialize)]
pub struct CtxInfo {
    #[serde(default)]
    pub used_percentage: f64,
}
#[derive(Debug, Default, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub display_name: String,
}
#[derive(Debug, Default, Deserialize)]
pub struct EffortInfo {
    #[serde(default)]
    pub level: String,
}
#[derive(Debug, Default, Deserialize)]
pub struct WorkspaceInfo {
    #[serde(default)]
    pub current_dir: String,
}
#[derive(Debug, Default, Deserialize)]
pub struct LimitWindow {
    #[serde(default)]
    pub used_percentage: f64,
    #[serde(default)]
    pub resets_at: i64,
}
#[derive(Debug, Default, Deserialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: LimitWindow,
    #[serde(default)]
    pub seven_day: LimitWindow,
}
#[derive(Debug, Default, Deserialize)]
pub struct StatuslineInput {
    #[serde(default)]
    pub model: ModelInfo,
    #[serde(default)]
    pub effort: EffortInfo,
    #[serde(default)]
    pub context_window: CtxInfo,
    #[serde(default)]
    pub rate_limits: RateLimits,
    #[serde(default)]
    pub workspace: WorkspaceInfo,
    #[serde(default)]
    pub cwd: String,
}

pub fn to_snapshot(input: &StatuslineInput) -> LimitsSnapshot {
    LimitsSnapshot {
        agent: "claude".into(),
        five_hour: Window {
            used_pct: input.rate_limits.five_hour.used_percentage,
            resets_at: input.rate_limits.five_hour.resets_at,
        },
        seven_day: Window {
            used_pct: input.rate_limits.seven_day.used_percentage,
            resets_at: input.rate_limits.seven_day.resets_at,
        },
        stamped_at: limits::now_epoch(),
    }
}

fn git_branch(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    // `--no-optional-locks` because this runs once a second from the status line
    // and must never contend with the user's own git commands.
    crate::git::maybe(
        std::path::Path::new(cwd),
        &["--no-optional-locks", "branch", "--show-current"],
    )
}

pub fn render(input: &StatuslineInput, no_color: bool) -> String {
    let cwd = if !input.workspace.current_dir.is_empty() {
        input.workspace.current_dir.as_str()
    } else {
        input.cwd.as_str()
    };
    let mut parts: Vec<String> = Vec::new();
    if !input.model.display_name.is_empty() {
        let model = if input.effort.level.is_empty() {
            input.model.display_name.clone()
        } else {
            format!("{} ({})", input.model.display_name, input.effort.level)
        };
        parts.push(model);
    }
    if let Some(b) = git_branch(cwd) {
        parts.push(format!("\u{2387} {b}")); // ⎇ branch
    }
    parts.push(format!("ctx {}%", input.context_window.used_percentage.round() as i64));

    let five = input.rate_limits.five_hour.used_percentage.round() as i64;
    let cd = limits::countdown(input.rate_limits.five_hour.resets_at, limits::now_epoch());
    let five_seg = format!("5h {five}% (resets in {cd})");
    parts.push(colorize(five_seg, five, 85, no_color));

    let week = input.rate_limits.seven_day.used_percentage.round() as i64;
    let cd = limits::countdown(input.rate_limits.seven_day.resets_at, limits::now_epoch());
    let week_seg = format!("wk {week}% (resets in {cd})");
    parts.push(colorize(week_seg, week, 90, no_color));
    parts.join(" \u{b7} ") // middot separator
}

/// Escalate a limit segment at its warning threshold, then red at 95%.
fn colorize(seg: String, pct: i64, warn_at: i64, no_color: bool) -> String {
    if no_color {
        return seg;
    }
    let code = if pct >= 95 {
        "31" // red
    } else if pct >= warn_at {
        "33" // yellow
    } else {
        return seg;
    };
    format!("\x1b[{code}m{seg}\x1b[0m")
}

pub fn run() {
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let input: StatuslineInput = serde_json::from_str(&raw).unwrap_or_default();

    // Best-effort limit capture: workspace copy (if in a ws launch) + global copy.
    let snap = to_snapshot(&input);
    let _ = limits::write(&limits::global_path(), &snap);
    if let Some(ws) = crate::internal::current_ws() {
        let _ = limits::write(&ws.local_dir().join("limits.json"), &snap);
    }

    let no_color = std::env::var_os("NO_COLOR").is_some();
    let _ = writeln!(std::io::stdout(), "{}", render(&input, no_color));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(model: &str, ctx: f64, five: f64, week: f64) -> StatuslineInput {
        StatuslineInput {
            model: ModelInfo { display_name: model.into() },
            effort: EffortInfo::default(),
            context_window: CtxInfo { used_percentage: ctx },
            rate_limits: RateLimits {
                five_hour: LimitWindow { used_percentage: five, resets_at: 0 },
                seven_day: LimitWindow { used_percentage: week, resets_at: 0 },
            },
            workspace: WorkspaceInfo::default(),
            cwd: String::new(),
        }
    }

    #[test]
    fn renders_the_segments_it_was_given() {
        let s = render(&input("Sonnet 5", 41.4, 12.0, 45.0), true);
        assert!(s.contains("Sonnet 5"), "{s}");
        assert!(s.contains("ctx 41%"), "rounded, not truncated: {s}");
        assert!(s.contains("5h 12%"), "{s}");
        assert!(s.contains("wk 45%"), "{s}");
    }

    #[test]
    fn the_effort_level_is_shown_only_when_present() {
        let mut i = input("Sonnet 5", 0.0, 0.0, 0.0);
        let model_seg = |s: &str| s.split(" \u{b7} ").next().unwrap().to_string();
        assert_eq!(model_seg(&render(&i, true)), "Sonnet 5", "no effort, no parens");
        i.effort = EffortInfo { level: "xhigh".into() };
        assert_eq!(model_seg(&render(&i, true)), "Sonnet 5 (xhigh)");
    }

    #[test]
    fn an_empty_payload_still_renders_something() {
        // The status line runs on every prompt; a malformed payload must degrade,
        // never blank the line or panic.
        let s = render(&StatuslineInput::default(), true);
        assert!(s.contains("ctx 0%"), "{s}");
    }

    #[test]
    fn no_color_emits_no_escape_codes_even_past_the_threshold() {
        let s = render(&input("m", 0.0, 99.0, 99.0), true);
        assert!(!s.contains('\x1b'), "NO_COLOR must be absolute: {s:?}");
    }

    #[test]
    fn limits_escalate_at_their_thresholds() {
        assert_eq!(colorize("x".into(), 10, 85, false), "x", "quiet below the warning");
        assert!(colorize("x".into(), 85, 85, false).contains("33m"), "yellow at the threshold");
        assert!(colorize("x".into(), 95, 85, false).contains("31m"), "red at 95");
        assert_eq!(colorize("x".into(), 99, 85, true), "x", "no_color wins");
    }

    /// The weekly window warns later than the 5-hour one, because a weekly figure
    /// climbing through 85% is normal mid-week and not worth alarming about.
    #[test]
    fn the_weekly_window_has_a_higher_threshold_than_the_five_hour_one() {
        let s = render(&input("m", 0.0, 88.0, 88.0), false);
        let five = s.split(" \u{b7} ").find(|p| p.contains("5h")).unwrap();
        let week = s.split(" \u{b7} ").find(|p| p.contains("wk")).unwrap();
        assert!(five.contains("33m"), "5h warns at 85: {five:?}");
        assert!(!week.contains('\x1b'), "wk stays quiet until 90: {week:?}");
    }

    #[test]
    fn to_snapshot_carries_both_windows_and_names_the_agent() {
        let snap = to_snapshot(&input("m", 0.0, 12.5, 45.5));
        assert_eq!(snap.agent, "claude", "ws can only capture Claude's limits");
        assert_eq!(snap.five_hour.used_pct, 12.5);
        assert_eq!(snap.seven_day.used_pct, 45.5);
        assert!(snap.stamped_at > 0, "a snapshot must be datable or it cannot go stale");
    }

    #[test]
    fn git_branch_is_none_outside_a_repo() {
        let d = tempfile::TempDir::new().unwrap();
        assert_eq!(git_branch(d.path().to_str().unwrap()), None);
        assert_eq!(git_branch(""), None, "an empty cwd must not shell out");
    }
}
