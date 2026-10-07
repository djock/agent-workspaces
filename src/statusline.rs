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
    #[serde(default)]
    pub session_id: String,
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
    /// Unread cross-workspace messages, counted the same way `ws -msg` and the
    /// prompt digest count them — one definition, so the badge cannot say two
    /// while the digest shows none.
    pub unread: usize,
    /// Feature worktrees marked done — see `done::chip_count`.
    pub done: usize,
}

/// How the bar is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// `NO_COLOR`: emit no escape codes at all, so the terminal's palette shows
    /// through. Falls back to the middot-separated text line.
    pub plain: bool,
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
const RED: (u8, u8, u8) = (240, 82, 82); // a gauge past critical
/// The backing for a gauge with headroom and for a workspace without a color: a
/// dark slate a step above the terminal, so the pill reads as a shape without
/// the light-grey slab the old bar put behind every number.
const TINT: (u8, u8, u8) = (38, 43, 54);
/// Text on `TINT`: a soft grey, quieter than the near-white on the accents, so a
/// healthy number does not compete with the model and the branch.
const TINT_TEXT: (u8, u8, u8) = (199, 205, 216);

/// The rounded ends of a pill: Powerline's half-circles (U+E0B6, U+E0B4). They
/// live in the Private Use Area, so they need a Nerd Font or a terminal that draws
/// Powerline glyphs itself (iTerm: Profiles › Text › Use built-in Powerline glyphs).
const CAP_LEFT: char = '\u{e0b6}';
const CAP_RIGHT: char = '\u{e0b4}';

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

/// A gauge's pill, escalating on its own value.
fn gauge(text: String, pct: i64, warn: i64, crit: i64) -> Seg {
    if pct >= crit {
        Seg::new(text, RED)
    } else if pct >= warn {
        Seg::new(text, AMBER)
    } else {
        Seg { text, bg: TINT, fg: Some(TINT_TEXT) }
    }
}

/// One pill.
struct Seg {
    text: String,
    bg: (u8, u8, u8),
    /// Text color when `ink_for` would be too loud for the pill's role.
    fg: Option<(u8, u8, u8)>,
}

impl Seg {
    fn new(text: impl Into<String>, bg: (u8, u8, u8)) -> Self {
        Seg { text: text.into(), bg, fg: None }
    }
}

/// Rounded pills, one space apart. The caps are drawn in the pill's color on the
/// terminal's own background, which is what makes the ends look round. One space
/// of padding inside each cap, so the text does not touch the curve.
fn draw(segs: &[Seg]) -> String {
    // Lead with a reset: residual SGR state from whatever drew last must not
    // bleed into the first pill.
    let mut out = String::from("\x1b[0m");
    for (i, seg) in segs.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let (r, g, b) = seg.bg;
        let (tr, tg, tb) = seg.fg.unwrap_or_else(|| ink_for(seg.bg));
        out.push_str(&format!("\x1b[49m\x1b[38;2;{r};{g};{b}m{CAP_LEFT}"));
        out.push_str(&format!("\x1b[48;2;{r};{g};{b}m\x1b[38;2;{tr};{tg};{tb}m {} ", seg.text));
        out.push_str(&format!("\x1b[49m\x1b[38;2;{r};{g};{b}m{CAP_RIGHT}\x1b[0m"));
    }
    out
}

pub fn render(input: &StatuslineInput, chip: Option<&Chip>, style: Style) -> String {
    let cwd = if !input.workspace.current_dir.is_empty() {
        input.workspace.current_dir.as_str()
    } else {
        input.cwd.as_str()
    };

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
            if c.unread > 0 {
                parts.push(format!("mail {}", c.unread));
            }
            if c.done > 0 {
                parts.push(format!("done {}", c.done));
            }
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
        let bg = c.color.as_deref().and_then(crate::term::rgb).unwrap_or(TINT);
        segs.push(Seg::new(&c.name, bg));
        // Amber, beside the workspace it belongs to, and only when there is
        // something to read: a badge that is always present is one nobody sees.
        if c.unread > 0 {
            segs.push(Seg::new(format!("\u{2709} {}", c.unread), AMBER));
        }
        // Same rule as the badge above: silent until a worktree is done.
        if c.done > 0 {
            segs.push(Seg::new(format!("done {}", c.done), AMBER));
        }
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
    segs.push(gauge(format!("ctx {ctx}%"), ctx, 50, 80));
    segs.push(gauge(format!("5h {five}%{five_cd}"), five, 70, 90));
    // The weekly window warns far later than the 5-hour one. A weekly figure
    // climbing through 70% is normal mid-week; warning there would leave the
    // block amber for days and teach you to ignore it.
    segs.push(gauge(format!("wk {week}%{week_cd}"), week, 90, 95));
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
        let _ = crate::rotation::write_reading(
            &ws,
            &input.session_id,
            input.context_window.used_percentage,
        );
        Chip {
            unread: crate::mail::unread_count(&ws.root),
            name: ws.name.clone(),
            color: crate::meta::read(&ws.workspace_toml()).color,
            done: crate::done::chip_count(&ws.name, &ws.root),
        }
    });

    let style = Style { plain: std::env::var_os("NO_COLOR").is_some() };
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
            session_id: String::new(),
        }
    }

    const PLAIN: Style = Style { plain: true };
    const BAR: Style = Style { plain: false };

    fn chip(color: Option<&str>) -> Chip {
        Chip { name: "ws-ui".into(), color: color.map(str::to_string), unread: 0, done: 0 }
    }

    #[test]
    fn the_chip_shows_how_many_worktrees_are_done_and_nothing_when_none() {
        let mut c = chip(None);
        c.done = 2;
        let with = render(&input("Sonnet", 10.0, 1.0, 1.0), Some(&c), PLAIN);
        assert!(with.contains("done 2"), "{with}");
        c.done = 0;
        let without = render(&input("Sonnet", 10.0, 1.0, 1.0), Some(&c), PLAIN);
        assert!(!without.contains("done"), "{without}");
    }

    fn chip_with_mail(unread: usize) -> Chip {
        Chip { name: "ws-ui".into(), color: None, unread, done: 0 }
    }

    /// A badge that is always there is one nobody sees, so it appears only when
    /// there is something to read.
    #[test]
    fn the_mail_badge_shows_only_when_there_is_mail() {
        let i = input("Opus", 10.0, 10.0, 10.0);
        let none = text_of(&render(&i, Some(&chip_with_mail(0)), BAR));
        assert!(!none.contains('\u{2709}'), "no badge with no mail: {none:?}");

        let some = text_of(&render(&i, Some(&chip_with_mail(3)), BAR));
        assert!(some.contains("\u{2709} 3"), "the count is the point: {some:?}");
    }

    #[test]
    fn the_done_segment_shows_only_when_a_worktree_is_done() {
        let i = input("Opus", 10.0, 10.0, 10.0);
        let mut c = chip_with_mail(0);
        let none = text_of(&render(&i, Some(&c), BAR));
        assert!(!none.contains("done"), "no segment with nothing done: {none:?}");
        c.done = 2;
        let raw = render(&i, Some(&c), BAR);
        assert!(raw.contains('\u{1b}'), "the coloured bar carries escapes: {raw:?}");
        let some = text_of(&raw);
        assert!(some.contains("done 2"), "{some:?}");
    }

    #[test]
    fn the_mail_badge_survives_no_color() {
        let i = input("Opus", 10.0, 10.0, 10.0);
        let plain = render(&i, Some(&chip_with_mail(2)), PLAIN);
        assert!(plain.contains("mail 2"), "{plain:?}");
        assert!(!plain.contains('\u{1b}'), "NO_COLOR is absolute: {plain:?}");
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
        assert!(text_of(&render(&i, None, BAR)).contains("\u{e0b6} Sonnet 5 xhigh \u{e0b4}"));
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
        let quiet = format!("\x1b[48;2;{};{};{}", TINT.0, TINT.1, TINT.2);

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
        let amber_bg = format!("48;2;{};{};{}", AMBER.0, AMBER.1, AMBER.2);
        assert!(s[..five_at].contains(&amber_bg));
        assert!(
            !s[five_at..week_at].contains(&amber_bg),
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
            (TINT, CHIPTEXT, "dark tint"),
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
        let at = s.find(" ctx 55%").unwrap();
        assert!(s[..at].ends_with(&format!("\x1b[38;2;{};{};{}m", INK.0, INK.1, INK.2)), "{s:?}");
    }

    /// Every pill is closed by its own rounded caps and set one space from the
    /// next, so neighbours that share the tint still read as separate numbers.
    #[test]
    fn every_segment_is_a_rounded_pill_one_space_apart() {
        let s = text_of(&render(&input("Opus", 1.0, 1.0, 1.0), Some(&chip(Some("green"))), BAR));
        assert_eq!(
            s,
            "\u{e0b6} ws-ui \u{e0b4} \u{e0b6} Opus \u{e0b4} \u{e0b6} ctx 1% \u{e0b4} \u{e0b6} 5h 1% \u{e0b4} \u{e0b6} wk 1% \u{e0b4}"
        );
    }

    /// A healthy gauge keeps its numbers quiet; the old light-grey slab is gone.
    #[test]
    fn a_healthy_gauge_sits_on_the_dark_tint_with_soft_text() {
        let s = render(&input("m", 1.0, 1.0, 1.0), None, BAR);
        let (r, g, b) = TINT;
        let (tr, tg, tb) = TINT_TEXT;
        assert!(
            s.contains(&format!("\x1b[48;2;{r};{g};{b}m\x1b[38;2;{tr};{tg};{tb}m ctx 1%")),
            "{s:?}"
        );
        assert!(!s.contains("48;2;212;212;212"), "no grey slab: {s:?}");
    }

    /// The caps sit on the terminal's own background, or the pill would show
    /// square corners in the bar's color.
    #[test]
    fn the_caps_are_drawn_on_the_terminal_background() {
        let s = render(&input("m", 1.0, 1.0, 1.0), None, BAR);
        assert_eq!(s.matches("\x1b[49m").count(), 2 * s.matches('\u{e0b6}').count(), "{s:?}");
        assert!(s.ends_with("\x1b[0m"), "the line closes with a reset: {s:?}");
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
        assert!(
            s.starts_with(&format!("\x1b[0m\x1b[49m\x1b[38;2;{r};{g};{b}m\u{e0b6}")),
            "chip first: {s:?}"
        );
        assert!(text_of(&s).starts_with("\u{e0b6} ws-ui \u{e0b4}"), "{s:?}");
        assert!(text_of(&s).contains("Sonnet 5"), "the rest of the bar survives: {s:?}");
    }

    /// The whole point of the feature is that ws draws the workspace identity
    /// itself, so nothing has to inject Claude's `/color` and put a pill on the
    /// prompt divider. If the name vanishes, that reason is gone.
    #[test]
    fn a_workspace_without_a_color_still_shows_its_name() {
        let s = render(&input("m", 0.0, 0.0, 0.0), Some(&chip(None)), BAR);
        let (r, g, b) = TINT;
        assert!(text_of(&s).starts_with("\u{e0b6} ws-ui \u{e0b4}"), "{s:?}");
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
            .starts_with("\u{e0b6} Sonnet 5"));
    }

    #[test]
    fn git_branch_is_none_outside_a_repo() {
        let d = tempfile::TempDir::new().unwrap();
        assert_eq!(git_branch(d.path().to_str().unwrap()), None);
        assert_eq!(git_branch(""), None, "an empty cwd must not shell out");
    }
}
