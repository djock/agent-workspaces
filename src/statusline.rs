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
    let out = std::process::Command::new("git")
        .args(["-C", cwd, "--no-optional-locks", "branch", "--show-current"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b.is_empty() {
        None
    } else {
        Some(b)
    }
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

#[derive(Debug, Default, Deserialize)]
pub struct SubagentInput {
    #[serde(default)]
    pub tasks: Vec<Task>,
}
#[derive(Debug, Default, Deserialize)]
#[allow(non_snake_case)]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tokenCount: i64,
    #[serde(default)]
    pub contextWindowSize: i64,
    #[serde(default)]
    pub start: i64,
}

fn elapsed(start_ms: i64, now_ms: i64) -> String {
    if start_ms <= 0 || now_ms <= start_ms {
        return "0m0s".to_string();
    }
    let secs = (now_ms - start_ms) / 1000;
    format!("{}m{}s", secs / 60, secs % 60)
}

pub fn subagent_row(t: &Task, now_ms: i64) -> String {
    let name = if !t.name.is_empty() { &t.name } else { &t.type_ };
    let ctx = if t.contextWindowSize > 0 {
        (t.tokenCount.saturating_mul(100) / t.contextWindowSize).clamp(0, 100)
    } else {
        0
    };
    format!(
        "\u{21b7} {}  {} \u{b7} {} \u{b7} ctx {}% \u{b7} {}",
        t.model,
        name,
        t.description,
        ctx,
        elapsed(t.start, now_ms)
    )
}

pub fn run_subagent() {
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let input: SubagentInput = serde_json::from_str(&raw).unwrap_or_default();
    let now_ms = std::env::var("WS_SUBAGENT_NOW_MS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| limits::now_epoch() * 1000);
    for t in &input.tasks {
        let row = serde_json::json!({ "id": t.id, "content": subagent_row(t, now_ms) });
        let _ = writeln!(std::io::stdout(), "{row}");
    }
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
