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

/// The workspace a status line is being drawn for, when it is inside one.
/// Passed in rather than read from the environment so `render` stays pure.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub name: String,
    pub color: Option<String>,
}

/// How the bar is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// `NO_COLOR`: emit no escape codes at all, so the terminal's palette shows
    /// through. Falls back to the middot-separated text line.
    pub plain: bool,
    /// A dark terminal. Only shifts the quiet `surface` shade; every other color
    /// holds on both.
    pub dark: bool,
}

// --- the bar's palette -------------------------------------------------------
//
/// Text on a dark block: a near-white that holds on every saturated accent below.
const CHIPTEXT: (u8, u8, u8) = crate::term::CHIPTEXT;
/// Text on a light block. Warm and not near-black, so it reads as tonal rather
/// than as a hole punched in the bar.
const INK: (u8, u8, u8) = (30, 30, 30);
const PERIWINKLE: (u8, u8, u8) = (138, 134, 236); // model
const SLATE: (u8, u8, u8) = (79, 91, 140); // git branch
const AMBER: (u8, u8, u8) = (255, 183, 77); // a gauge at its warning threshold
const RED: (u8, u8, u8) = (220, 38, 38); // a gauge past critical
/// The divider between two blocks that share a background. Without it, three
/// healthy gauges in a row merge into one long slab and stop reading as three
/// numbers. It has to read against whichever background it lands on, so it is
/// derived per block rather than fixed: the block's own ink, softened.
fn hairline_for(bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let (r, g, b) = bg;
    let (ir, ig, ib) = ink_for(bg);
    // Halfway between the block and its text: present as a seam, not as a stripe.
    ((r as u16 + ir as u16) as u8 / 2, (g as u16 + ig as u16) as u8 / 2, (b as u16 + ib as u16) as u8 / 2)
}

/// The quiet backing for a gauge with headroom: a neutral light grey, a shade
/// deeper on a light terminal so the block still reads as a block rather than
/// dissolving into the background. Both shades are light enough to take dark ink,
/// which `ink_for` works out rather than being told.
fn surface(dark: bool) -> (u8, u8, u8) {
    if dark {
        (212, 212, 212)
    } else {
        (190, 190, 190)
    }
}

/// Text color for a block, chosen by how light its background is.
///
/// Perceived brightness, not the raw average: the eye weights green far above
/// blue, so `(138,134,236)` periwinkle is dark to look at despite a high blue
/// channel. The 150 pivot puts amber and the grey surface on dark ink and leaves
/// every saturated accent on near-white.
fn ink_for(bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let (r, g, b) = bg;
    let lum = (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000;
    if lum >= 150 {
        INK
    } else {
        CHIPTEXT
    }
}

/// The time-to-reset suffix: a clock glyph and a countdown.
///
/// Empty when the reset moment is unknown or already past. `limits::countdown`
/// renders that case as "0m", which read acceptably inside "(resets in 0m)" but
/// is just noise beside a bare clock — better to show no clock than a stopped one.
fn reset_suffix(resets_at: i64, now: i64) -> String {
    if resets_at <= 0 || resets_at <= now {
        return String::new();
    }
    // U+25F7 — a geometric clock, single-width, same family as the U+2387 branch
    // glyph. An emoji clock would be double-width and wreck the block widths.
    format!(" \u{25f7} {}", limits::countdown(resets_at, now))
}

/// A gauge's backing, escalating on its own value.
fn gauge(pct: i64, warn: i64, crit: i64, dark: bool) -> (u8, u8, u8) {
    if pct >= crit {
        RED
    } else if pct >= warn {
        AMBER
    } else {
        surface(dark)
    }
}

/// One filled block.
struct Seg {
    text: String,
    bg: (u8, u8, u8),
}

impl Seg {
    fn new(text: impl Into<String>, bg: (u8, u8, u8)) -> Self {
        Seg { text: text.into(), bg }
    }
}

/// Squared blocks that abut: the change of background *is* the separator, which
/// is the most discreet one available. Neighbours sharing a background get a
/// one-eighth bar (U+258F) in the shared color so the boundary does not vanish.
fn draw(segs: &[Seg]) -> String {
    // Lead with a reset: residual SGR state from whatever drew last must not
    // bleed into the first block.
    let mut out = String::from("\x1b[0m");
    for (i, seg) in segs.iter().enumerate() {
        let (r, g, b) = seg.bg;
        if i > 0 && segs[i - 1].bg == seg.bg {
            let (hr, hg, hb) = hairline_for(seg.bg);
            out.push_str(&format!("\x1b[48;2;{r};{g};{b}m\x1b[38;2;{hr};{hg};{hb}m\u{258f}"));
        }
        let (tr, tg, tb) = ink_for(seg.bg);
        out.push_str(&format!("\x1b[48;2;{r};{g};{b}m\x1b[38;2;{tr};{tg};{tb}m {} ", seg.text));
    }
    out.push_str("\x1b[0m");
    out
}

pub fn render(input: &StatuslineInput, chip: Option<&Chip>, style: Style) -> String {
    let cwd = if !input.workspace.current_dir.is_empty() {
        input.workspace.current_dir.as_str()
    } else {
        input.cwd.as_str()
    };
    let dark = style.dark;

    let branch = git_branch(cwd);
    let ctx = input.context_window.used_percentage.round() as i64;
    let five = input.rate_limits.five_hour.used_percentage.round() as i64;
    let week = input.rate_limits.seven_day.used_percentage.round() as i64;
    let now = limits::now_epoch();
    let five_cd = reset_suffix(input.rate_limits.five_hour.resets_at, now);
    let week_cd = reset_suffix(input.rate_limits.seven_day.resets_at, now);

    if style.plain {
        // NO_COLOR is absolute: no blocks, no escapes — the middot line.
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = chip {
            parts.push(c.name.clone());
        }
        if !input.model.display_name.is_empty() {
            parts.push(match input.effort.level.as_str() {
                "" => input.model.display_name.clone(),
                e => format!("{} ({})", input.model.display_name, e),
            });
        }
        if let Some(b) = &branch {
            parts.push(format!("\u{2387} {b}"));
        }
        parts.push(format!("ctx {ctx}%"));
        parts.push(format!("5h {five}%{five_cd}"));
        parts.push(format!("wk {week}%{week_cd}"));
        return parts.join(" \u{b7} ");
    }

    let mut segs: Vec<Seg> = Vec::new();
    if let Some(c) = chip {
        // An unknown or absent color falls back to the quiet backing: the
        // workspace name is the point, its color is the decoration.
        let bg = c.color.as_deref().and_then(crate::term::rgb).unwrap_or(surface(dark));
        segs.push(Seg::new(&c.name, bg));
    }
    if !input.model.display_name.is_empty() {
        // No parentheses here: the block boundary already separates the effort
        // from the model name, so the punctuation is noise.
        let model = match input.effort.level.as_str() {
            "" => input.model.display_name.clone(),
            e => format!("{} {}", input.model.display_name, e),
        };
        segs.push(Seg::new(model, PERIWINKLE));
    }
    if let Some(b) = branch {
        segs.push(Seg::new(format!("\u{2387} {b}"), SLATE));
    }
    // Context pressure is worth seeing early because it is actionable — compact
    // or rotate — so it warns at half full.
    segs.push(Seg::new(format!("ctx {ctx}%"), gauge(ctx, 50, 80, dark)));
    segs.push(Seg::new(format!("5h {five}%{five_cd}"), gauge(five, 70, 90, dark)));
    // The weekly window warns far later than the 5-hour one. A weekly figure
    // climbing through 70% is normal mid-week; warning there would leave the
    // block amber for days and teach you to ignore it.
    segs.push(Seg::new(format!("wk {week}%{week_cd}"), gauge(week, 90, 95, dark)));
    draw(&segs)
}

pub fn run() {
    let raw = std::io::read_to_string(std::io::stdin()).unwrap_or_default();
    let input: StatuslineInput = serde_json::from_str(&raw).unwrap_or_default();

    // Best-effort limit capture: workspace copy (if in a ws launch) + global copy.
    let snap = to_snapshot(&input);
    let _ = limits::write(&limits::global_path(), &snap);
    let chip = crate::internal::current_ws().map(|ws| {
        let _ = limits::write(&ws.local_dir().join("limits.json"), &snap);
        Chip { name: ws.name.clone(), color: crate::meta::read(&ws.workspace_toml()).color }
    });

    // Theme detection without `ThemeEnv::detect()`: that shells out to
    // `defaults read` on macOS, and this line repaints once a second. COLORFGBG
    // is a plain env var, and a dark terminal is the right guess when it is absent.
    let env = crate::theme::ThemeEnv {
        no_color: std::env::var_os("NO_COLOR").is_some(),
        colorfgbg: std::env::var("COLORFGBG").ok(),
        os_dark: None,
    };
    let theme = crate::theme::resolve("auto", &env);
    let style = Style { plain: theme.plain, dark: theme.dark };
    let _ = writeln!(std::io::stdout(), "{}", render(&input, chip.as_ref(), style));
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

    const PLAIN: Style = Style { plain: true, dark: true };
    const BAR: Style = Style { plain: false, dark: true };

    fn chip(color: Option<&str>) -> Chip {
        Chip { name: "ws-ui".into(), color: color.map(str::to_string) }
    }

    /// Strip every SGR escape, leaving the text the bar actually shows. Lets the
    /// content tests read the bar without asserting on color codes.
    fn text_of(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn renders_the_segments_it_was_given() {
        for style in [PLAIN, BAR] {
            let s = text_of(&render(&input("Sonnet 5", 41.4, 12.0, 45.0), None, style));
            assert!(s.contains("Sonnet 5"), "{s}");
            assert!(s.contains("ctx 41%"), "rounded, not truncated: {s}");
            assert!(s.contains("5h 12%"), "{s}");
            assert!(s.contains("wk 45%"), "{s}");
        }
    }

    #[test]
    fn the_effort_level_is_shown_only_when_present() {
        let mut i = input("Sonnet 5", 0.0, 0.0, 0.0);
        let model_seg = |s: &str| s.split(" \u{b7} ").next().unwrap().to_string();
        assert_eq!(model_seg(&render(&i, None, PLAIN)), "Sonnet 5", "no effort, no parens");
        i.effort = EffortInfo { level: "xhigh".into() };
        assert_eq!(model_seg(&render(&i, None, PLAIN)), "Sonnet 5 (xhigh)");
        // In the bar the block boundary separates them, so the parentheses go.
        assert!(text_of(&render(&i, None, BAR)).contains(" Sonnet 5 xhigh "));
    }

    #[test]
    fn an_empty_payload_still_renders_something() {
        // The status line runs on every prompt; a malformed payload must degrade,
        // never blank the line or panic.
        for style in [PLAIN, BAR] {
            let s = text_of(&render(&StatuslineInput::default(), None, style));
            assert!(s.contains("ctx 0%"), "{s}");
        }
    }

    #[test]
    fn no_color_emits_no_escape_codes_even_past_the_threshold() {
        let s = render(&input("m", 0.0, 99.0, 99.0), Some(&chip(Some("green"))), PLAIN);
        assert!(!s.contains('\x1b'), "NO_COLOR must be absolute: {s:?}");
    }

    /// Each gauge escalates on its own value, so one hot window cannot make the
    /// others look hot too.
    #[test]
    fn gauges_escalate_independently_on_their_own_value() {
        let bg = |seg: &str, s: &str| {
            // The SGR run immediately preceding this segment's text is its background.
            let at = s.find(seg).unwrap_or_else(|| panic!("{seg:?} missing from {s:?}"));
            let head = &s[..at];
            head[head.rfind("\x1b[48;2;").unwrap()..].split('m').next().unwrap().to_string()
        };
        let amber = format!("\x1b[48;2;{};{};{}", AMBER.0, AMBER.1, AMBER.2);
        let red = format!("\x1b[48;2;{};{};{}", RED.0, RED.1, RED.2);
        let quiet = {
            let (r, g, b) = surface(true);
            format!("\x1b[48;2;{r};{g};{b}")
        };

        // ctx warns at 50, 5h at 70, wk not until 90.
        let s = render(&input("m", 55.0, 75.0, 75.0), None, BAR);
        assert_eq!(bg("ctx 55%", &s), amber, "ctx warns at 50");
        assert_eq!(bg("5h 75%", &s), amber, "5h warns at 70");
        assert_eq!(bg("wk 75%", &s), quiet, "wk stays quiet at 75: {s:?}");

        let s = render(&input("m", 85.0, 95.0, 96.0), None, BAR);
        assert_eq!(bg("ctx 85%", &s), red, "ctx is critical at 80");
        assert_eq!(bg("5h 95%", &s), red, "5h is critical at 90");
        assert_eq!(bg("wk 96%", &s), red, "wk is critical at 95");

        let s = render(&input("m", 10.0, 10.0, 10.0), None, BAR);
        assert_eq!(bg("ctx 10%", &s), quiet, "a healthy gauge is quiet");
    }

    /// The weekly window warns far later than the 5-hour one: a weekly figure
    /// climbing through 70% is normal mid-week, and a block that is amber for
    /// days teaches you to ignore it.
    #[test]
    fn the_weekly_window_has_a_higher_threshold_than_the_five_hour_one() {
        let s = render(&input("m", 0.0, 75.0, 75.0), None, BAR);
        let five_at = s.find("5h 75%").unwrap();
        let week_at = s.find("wk 75%").unwrap();
        assert!(s[..five_at].contains(&format!("{};{};{}", AMBER.0, AMBER.1, AMBER.2)));
        assert!(
            !s[five_at..week_at].contains(&format!("{};{};{}", AMBER.0, AMBER.1, AMBER.2)),
            "the same value must not warn in both windows: {s:?}"
        );
    }

    /// Every background the bar can draw must land on the readable side of the
    /// ink pivot. Perceived brightness is weighted, not averaged, so this is not
    /// obvious from the RGB triples by eye — periwinkle carries a higher blue
    /// channel than amber carries red, yet needs the opposite text color.
    #[test]
    fn every_background_gets_readable_text() {
        for (bg, want, what) in [
            (surface(true), INK, "grey surface, dark terminal"),
            (surface(false), INK, "grey surface, light terminal"),
            (AMBER, INK, "amber"),
            (PERIWINKLE, CHIPTEXT, "periwinkle"),
            (SLATE, CHIPTEXT, "slate"),
            (RED, CHIPTEXT, "red"),
        ] {
            assert_eq!(ink_for(bg), want, "{what} took the wrong ink");
        }
        // Every color a workspace can be allocated, too: the block is drawn the
        // same way whichever one it lands on.
        for name in crate::term::PALETTE {
            let bg = crate::term::rgb(name).unwrap();
            assert_eq!(ink_for(bg), CHIPTEXT, "{name} is a saturated accent");
        }
        // `white` is accepted from a hand-written workspace.toml, and is the one
        // color that would be unreadable if this rule were a fixed list.
        assert_eq!(ink_for(crate::term::rgb("white").unwrap()), INK);
    }

    /// The seam must contrast whichever block it sits on. A single fixed hairline
    /// color vanished against some backgrounds and glared against others.
    #[test]
    fn the_hairline_contrasts_the_block_it_sits_on() {
        for bg in [surface(true), AMBER, RED] {
            let h = hairline_for(bg);
            assert_ne!(h, bg, "a seam the color of its block is not a seam");
            assert_ne!(h, ink_for(bg), "a seam as strong as the text reads as a stripe");
        }
    }

    /// A clock with a countdown, and nothing at all when there is no reset to
    /// count down to — `limits::countdown` reports that as "0m", and a bare
    /// "◷ 0m" reads as a stopped clock rather than as missing information.
    #[test]
    fn the_reset_clock_appears_only_when_there_is_a_reset() {
        assert_eq!(reset_suffix(0, 1_000), "", "unknown reset shows nothing");
        assert_eq!(reset_suffix(900, 1_000), "", "a reset already past shows nothing");
        assert_eq!(reset_suffix(1_000, 1_000), "", "the exact moment counts as past");
        assert_eq!(reset_suffix(1_000 + 9_000, 1_000), " \u{25f7} 2h30m");

        // And end to end, in both renderings.
        let mut i = input("m", 0.0, 10.0, 10.0);
        i.rate_limits.five_hour.resets_at = limits::now_epoch() + 9_000;
        for style in [PLAIN, BAR] {
            let s = text_of(&render(&i, None, style));
            assert!(s.contains("5h 10% \u{25f7} "), "clock on the known window: {s:?}");
            assert_eq!(s.matches('\u{25f7}').count(), 1, "none on the unknown one: {s:?}");
            assert!(!s.contains("resets in"), "the prose is gone: {s:?}");
        }
    }

    /// Amber is light enough that near-white text would glare.
    #[test]
    fn a_warning_block_takes_dark_text() {
        let s = render(&input("m", 55.0, 0.0, 0.0), None, BAR);
        let at = s.find("ctx 55%").unwrap();
        assert!(s[..at].ends_with(&format!("\x1b[38;2;{};{};{}m ", INK.0, INK.1, INK.2)), "{s:?}");
    }

    /// Three healthy gauges share the quiet backing; without a divider they merge
    /// into one slab and stop reading as three separate numbers.
    #[test]
    fn same_colored_neighbours_get_a_hairline_between_them() {
        let s = render(&input("m", 1.0, 1.0, 1.0), None, BAR);
        assert_eq!(s.matches('\u{258f}').count(), 2, "ctx|5h and 5h|wk: {s:?}");
        // Differing backgrounds need no divider — the color change is the divider.
        let s = render(&input("m", 55.0, 1.0, 1.0), None, BAR);
        assert_eq!(s.matches('\u{258f}').count(), 1, "only 5h|wk still share: {s:?}");
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
    fn the_workspace_chip_leads_the_line() {
        let s = render(&input("Sonnet 5", 0.0, 0.0, 0.0), Some(&chip(Some("green"))), BAR);
        let (r, g, b) = crate::term::rgb("green").unwrap();
        assert!(s.starts_with(&format!("\x1b[0m\x1b[48;2;{r};{g};{b}m")), "chip first: {s:?}");
        assert!(s.contains(" ws-ui "), "{s:?}");
        assert!(text_of(&s).contains("Sonnet 5"), "the rest of the bar survives: {s:?}");
    }

    /// The whole point of the feature is that ws draws the workspace identity
    /// itself, so nothing has to inject Claude's `/color` and put a pill on the
    /// prompt divider. If the name vanishes, that reason is gone.
    #[test]
    fn a_workspace_without_a_color_still_shows_its_name() {
        let s = render(&input("m", 0.0, 0.0, 0.0), Some(&chip(None)), BAR);
        let (r, g, b) = surface(true);
        assert!(text_of(&s).starts_with(" ws-ui "), "{s:?}");
        assert!(s.contains(&format!("\x1b[48;2;{r};{g};{b}m")), "falls back to quiet: {s:?}");
    }

    #[test]
    fn no_color_keeps_the_name_and_drops_every_escape() {
        let s = render(&input("m", 0.0, 0.0, 0.0), Some(&chip(Some("green"))), PLAIN);
        assert!(s.starts_with("ws-ui \u{b7} "), "{s:?}");
        assert!(!s.contains('\x1b'), "NO_COLOR must be absolute: {s:?}");
    }

    /// Outside a ws launch the status line is still just the status line.
    #[test]
    fn no_workspace_means_no_prefix_at_all() {
        let bare = render(&input("Sonnet 5", 1.0, 2.0, 3.0), None, PLAIN);
        assert!(bare.starts_with("Sonnet 5"), "{bare:?}");
        assert!(text_of(&render(&input("Sonnet 5", 1.0, 2.0, 3.0), None, BAR))
            .starts_with(" Sonnet 5"));
    }

    #[test]
    fn git_branch_is_none_outside_a_repo() {
        let d = tempfile::TempDir::new().unwrap();
        assert_eq!(git_branch(d.path().to_str().unwrap()), None);
        assert_eq!(git_branch(""), None, "an empty cwd must not shell out");
    }
}
