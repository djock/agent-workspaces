//! Theme resolution for the console list.
//!
//! Previously typed against `ratatui::style::Color` for the dashboard. The
//! dashboard is gone and the picker writes plain ANSI, so this resolves to escape
//! codes instead — and stays because `config theme` would otherwise be a key that
//! silently does nothing, which `config`'s own module docs call worse than a
//! missing key.
//!
//! `auto` asks the terminal first (`COLORFGBG`, which is what actually knows),
//! then the OS. `NO_COLOR` beats everything: it means "emit no escape codes",
//! not "pick different ones".

/// The inputs to detection, injected so `resolve` stays pure and testable.
#[derive(Debug, Clone, Default)]
pub struct ThemeEnv {
    pub no_color: bool,
    pub colorfgbg: Option<String>,
    /// `Some(true)` = the OS reports a dark appearance; `None` = unknown.
    pub os_dark: Option<bool>,
}

impl ThemeEnv {
    pub fn detect() -> Self {
        ThemeEnv {
            no_color: std::env::var_os("NO_COLOR").is_some(),
            colorfgbg: std::env::var("COLORFGBG").ok(),
            os_dark: os_dark(),
        }
    }
}

#[cfg(target_os = "macos")]
fn os_dark() -> Option<bool> {
    // `defaults read -g AppleInterfaceStyle` prints "Dark" in dark mode and
    // exits non-zero (key absent) in light mode.
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .ok()?;
    if !out.status.success() {
        return Some(false);
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "Dark")
}

#[cfg(not(target_os = "macos"))]
fn os_dark() -> Option<bool> {
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// No escape codes at all, so the terminal's own palette shows through.
    pub plain: bool,
}

impl Theme {
    /// Wrap `s` so it reads as the selected row. Reverse video rather than a
    /// colour, because it is legible on any palette without knowing it — the one
    /// styling choice that cannot be wrong.
    pub fn selected(&self, s: &str) -> String {
        if self.plain {
            s.to_string()
        } else {
            format!("\x1b[7m{s}\x1b[0m")
        }
    }

    /// De-emphasise secondary text (hints, timestamps).
    pub fn dim(&self, s: &str) -> String {
        if self.plain {
            s.to_string()
        } else {
            format!("\x1b[2m{s}\x1b[0m")
        }
    }
}

/// `config theme` ∈ {auto, light, dark}; anything else is treated as `auto`
/// because `config set` already refuses unknown values, so reaching here with one
/// means a hand-edited file — and a readable list beats an error.
pub fn resolve(configured: &str, env: &ThemeEnv) -> Theme {
    if env.no_color {
        return Theme { dark: true, plain: true };
    }
    let dark = match configured {
        "light" => false,
        "dark" => true,
        _ => auto_dark(env),
    };
    Theme { dark, plain: false }
}

/// `COLORFGBG` is `fg;bg` (sometimes `fg;<mid>;bg`); a low background index means
/// a dark terminal. It is checked before the OS because a terminal that reports
/// its own colours knows better than the desktop appearance setting — a light
/// terminal profile on a dark desktop is common, and the OS answer would be wrong.
fn auto_dark(env: &ThemeEnv) -> bool {
    if let Some(raw) = &env.colorfgbg {
        if let Some(bg) = raw.rsplit(';').next() {
            if let Ok(n) = bg.trim().parse::<u8>() {
                return n <= 6 || n == 8;
            }
        }
    }
    env.os_dark.unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ThemeEnv {
        ThemeEnv::default()
    }

    #[test]
    fn no_color_wins_over_everything() {
        let e = ThemeEnv { no_color: true, colorfgbg: Some("0;15".into()), os_dark: Some(false) };
        let t = resolve("dark", &e);
        assert!(t.plain);
        assert_eq!(t.selected("row"), "row", "no escape codes at all");
        assert_eq!(t.dim("hint"), "hint");
    }

    #[test]
    fn an_explicit_setting_beats_detection() {
        let e = ThemeEnv { colorfgbg: Some("15;0".into()), os_dark: Some(true), ..env() };
        assert!(!resolve("light", &e).dark, "explicit light wins over a dark terminal");
        let e2 = ThemeEnv { colorfgbg: Some("0;15".into()), os_dark: Some(false), ..env() };
        assert!(resolve("dark", &e2).dark);
    }

    /// The terminal's own report is preferred over the OS: a light profile on a
    /// dark desktop is common, and the OS answer is wrong for it.
    #[test]
    fn colorfgbg_beats_the_os_appearance() {
        let e = ThemeEnv { colorfgbg: Some("0;15".into()), os_dark: Some(true), ..env() };
        assert!(!resolve("auto", &e).dark, "bg 15 is a light terminal");
    }

    #[test]
    fn colorfgbg_handles_a_three_field_form() {
        let e = ThemeEnv { colorfgbg: Some("15;default;0".into()), ..env() };
        assert!(resolve("auto", &e).dark, "the trailing field is the background");
    }

    #[test]
    fn the_os_answers_when_the_terminal_does_not() {
        let e = ThemeEnv { colorfgbg: None, os_dark: Some(false), ..env() };
        assert!(!resolve("auto", &e).dark);
    }

    /// Unknown everything: dark is the safer default, since a dim style on a dark
    /// terminal is merely subtle while the reverse can be unreadable.
    #[test]
    fn unknown_everything_defaults_to_dark() {
        assert!(resolve("auto", &env()).dark);
    }

    #[test]
    fn an_unrecognised_configured_value_is_treated_as_auto() {
        let e = ThemeEnv { colorfgbg: Some("0;15".into()), ..env() };
        assert_eq!(resolve("chartreuse", &e), resolve("auto", &e));
    }

    #[test]
    fn garbage_colorfgbg_does_not_panic_and_falls_through() {
        let e = ThemeEnv { colorfgbg: Some("not;numbers".into()), os_dark: Some(false), ..env() };
        assert!(!resolve("auto", &e).dark, "falls through to the OS answer");
    }

    #[test]
    fn styling_wraps_and_resets() {
        let t = resolve("dark", &env());
        assert!(t.selected("x").starts_with("\x1b[7m"));
        assert!(t.selected("x").ends_with("\x1b[0m"));
        assert!(t.dim("x").starts_with("\x1b[2m"));
    }
}
