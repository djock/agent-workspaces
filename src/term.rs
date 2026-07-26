use std::io::{IsTerminal, Write};

fn rgb(color: &str) -> Option<(u8, u8, u8)> {
    let c = match color.to_ascii_lowercase().as_str() {
        "black" => (0, 0, 0),
        "red" => (204, 0, 0),
        "green" => (0, 153, 0),
        "yellow" => (204, 204, 0),
        "blue" => (0, 102, 204),
        "magenta" | "purple" => (153, 0, 204),
        "cyan" => (0, 153, 204),
        "white" => (255, 255, 255),
        "orange" => (230, 126, 34),
        "grey" | "gray" => (128, 128, 128),
        _ => return None,
    };
    Some(c)
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
}
