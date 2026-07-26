//! Theme resolution. `auto` reads the terminal's own hint (`COLORFGBG`) first,
//! then the OS appearance; an explicit `config theme` always wins. No tmux DCS
//! passthrough (spec §13).
use ratatui::style::Color;

/// The inputs to theme detection, injected so `resolve` stays pure and testable.
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
            os_dark: macos_dark(),
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_dark() -> Option<bool> {
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
fn macos_dark() -> Option<bool> {
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// NO_COLOR: every color resolves to `Color::Reset` so the terminal's own
    /// palette shows through untouched.
    pub plain: bool,
    pub accent: Color,
    pub dim: Color,
    pub live: Color,
    pub warn: Color,
}

/// Parse the background field of `COLORFGBG` ("<fg>;<bg>" or "<fg>;<x>;<bg>").
/// ANSI 0-6 and 8 are the dark backgrounds; 7 and 9-15 are light.
fn fgbg_is_dark(v: &str) -> Option<bool> {
    let bg: u8 = v.rsplit(';').next()?.trim().parse().ok()?;
    Some(matches!(bg, 0..=6 | 8))
}

pub fn resolve(cfg_theme: &str, env: &ThemeEnv) -> Theme {
    let dark = match cfg_theme {
        "dark" => true,
        "light" => false,
        // "auto" and anything unrecognized: the terminal's hint, then the OS,
        // then dark — the overwhelmingly common terminal background.
        _ => env
            .colorfgbg
            .as_deref()
            .and_then(fgbg_is_dark)
            .or(env.os_dark)
            .unwrap_or(true),
    };

    if env.no_color {
        return Theme {
            dark,
            plain: true,
            accent: Color::Reset,
            dim: Color::Reset,
            live: Color::Reset,
            warn: Color::Reset,
        };
    }

    Theme {
        dark,
        plain: false,
        accent: if dark { Color::Cyan } else { Color::Blue },
        dim: if dark { Color::DarkGray } else { Color::Gray },
        live: Color::Green,
        warn: if dark { Color::Yellow } else { Color::Red },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(no_color: bool, fgbg: Option<&str>, os_dark: Option<bool>) -> ThemeEnv {
        ThemeEnv { no_color, colorfgbg: fgbg.map(String::from), os_dark }
    }

    #[test]
    fn config_override_beats_detection() {
        // OS says light, COLORFGBG says light — an explicit config wins anyway.
        let t = resolve("dark", &env(false, Some("0;15"), Some(false)));
        assert!(t.dark, "config theme = dark must win over detection");
        let t = resolve("light", &env(false, Some("15;0"), Some(true)));
        assert!(!t.dark);
    }

    #[test]
    fn auto_reads_colorfgbg_background_field() {
        // COLORFGBG is "<fg>;<bg>"; a background of 0-6 or 8 means a dark terminal.
        assert!(resolve("auto", &env(false, Some("15;0"), None)).dark);
        assert!(!resolve("auto", &env(false, Some("0;15"), None)).dark);
        // Three-field form ("<fg>;<default>;<bg>") — the background is still last.
        assert!(resolve("auto", &env(false, Some("15;default;0"), None)).dark);
    }

    #[test]
    fn auto_falls_back_to_os_appearance_then_to_dark() {
        assert!(!resolve("auto", &env(false, None, Some(false))).dark);
        assert!(resolve("auto", &env(false, None, Some(true))).dark);
        assert!(resolve("auto", &env(false, None, None)).dark, "unknowable → dark, the common terminal");
        // Garbage COLORFGBG must not win over a known OS appearance.
        assert!(!resolve("auto", &env(false, Some("nonsense"), Some(false))).dark);
    }

    #[test]
    fn no_color_forces_plain() {
        let t = resolve("dark", &env(true, Some("15;0"), Some(true)));
        assert!(t.plain, "NO_COLOR must strip color regardless of theme");
        assert_eq!(t.accent, Color::Reset);
        assert_eq!(t.live, Color::Reset);
    }
}
