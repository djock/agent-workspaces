use std::io::{IsTerminal, Write};

/// The colors a workspace can be allocated, in the order `ws -color` lists them.
///
/// These are Claude Code's own theme tokens rather than terminal primaries, so a
/// workspace's tab background and its status-line chip are the same color as the
/// accent Claude paints its own session with. `rgb` accepts a few names outside
/// this list (see below), but only these are ever handed out automatically.
pub const PALETTE: &[&str] =
    &["red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan"];

/// Foreground for text sitting on a palette background. One value for all eight:
/// a near-white that keeps its contrast on the darkest (`blue`) and the lightest
/// (`yellow`) of them, so the chip never needs a per-color text rule.
pub const CHIPTEXT: (u8, u8, u8) = (240, 242, 255);

/// A color name's RGB, or `None` if the name is not one we know.
///
/// Beyond `PALETTE` this also resolves `magenta` (an alias for `purple`) and the
/// neutrals `white`/`black`/`grey`, which no allocation produces but a
/// hand-written `workspace.toml` may already carry.
pub fn rgb(color: &str) -> Option<(u8, u8, u8)> {
    let c = match color.to_ascii_lowercase().as_str() {
        "red" => (220, 38, 38),
        "blue" => (106, 155, 204),
        "green" => (22, 163, 74),
        "yellow" => (202, 138, 4),
        "purple" | "magenta" => (130, 125, 189),
        "orange" => (217, 119, 87),
        "pink" => (196, 102, 134),
        "cyan" => (8, 145, 178),
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "grey" | "gray" => (128, 128, 128),
        _ => return None,
    };
    Some(c)
}

/// Pick a color for a new workspace. Random rather than hashed from the name so
/// that two workspaces created back to back are unlikely to look alike; the
/// result is written to `workspace.toml` and never re-rolled.
pub fn alloc_color() -> &'static str {
    use rand::seq::SliceRandom;
    PALETTE.choose(&mut rand::thread_rng()).copied().unwrap_or("blue")
}

/// OSC 2: set window/tab title.
pub fn title_seq(title: &str) -> String {
    format!("\x1b]2;{title}\x07")
}

/// iTerm2 tab background color (three OSC-6 channel sequences).
pub fn color_seq(color: Option<&str>) -> String {
    let Some((r, g, b)) = color.and_then(rgb) else {
        return String::new();
    };
    format!(
        "\x1b]6;1;bg;red;brightness;{r}\x07\
         \x1b]6;1;bg;green;brightness;{g}\x07\
         \x1b]6;1;bg;blue;brightness;{b}\x07"
    )
}

/// Emit title and (unless NO_COLOR) tab color, only when stdout is a TTY.
pub fn set_tab(title: &str, color: Option<&str>) {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(title_seq(title).as_bytes());
    if std::env::var_os("NO_COLOR").is_none() {
        let _ = out.write_all(color_seq(color).as_bytes());
    }
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tab background and the status bar's workspace block read from one
    /// table, so a workspace cannot be one green in the tab and another in the bar.
    #[test]
    fn the_tab_color_comes_from_the_shared_palette() {
        let (r, _, _) = rgb("green").unwrap();
        assert!(color_seq(Some("green")).contains(&format!("red;brightness;{r}")));
    }

    #[test]
    fn title_has_osc2() {
        let s = title_seq("proj");
        assert!(s.starts_with("\x1b]2;"));
        assert!(s.ends_with('\x07'));
        assert!(s.contains("proj"));
    }

    #[test]
    fn known_color_emits_three_channels() {
        let s = color_seq(Some("orange"));
        assert_eq!(s.matches("\x1b]6;1;bg;").count(), 3);
        assert!(s.contains("red;brightness"));
        assert!(s.contains("green;brightness"));
        assert!(s.contains("blue;brightness"));
    }

    #[test]
    fn unknown_or_none_color_is_empty() {
        assert_eq!(color_seq(Some("chartreuse-plaid")), "");
        assert_eq!(color_seq(None), "");
    }

    #[test]
    fn every_palette_entry_resolves() {
        for name in PALETTE {
            assert!(rgb(name).is_some(), "{name} is allocatable but has no RGB");
        }
    }

    #[test]
    fn allocation_only_ever_returns_a_palette_member() {
        // Sampled rather than exhaustive: the point is that no draw escapes the
        // palette, and 200 draws over 8 colors makes an off-list value very hard
        // to miss.
        for _ in 0..200 {
            let c = alloc_color();
            assert!(PALETTE.contains(&c), "allocated {c}, which is not in the palette");
        }
    }

    /// `magenta` was the old name for this slot; a `workspace.toml` written
    /// before the palette was retuned must not lose its color.
    #[test]
    fn magenta_still_resolves_as_purple() {
        assert_eq!(rgb("magenta"), rgb("purple"));
    }
}
