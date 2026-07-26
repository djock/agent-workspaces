use std::process::Command;

/// Slugify an identifier: lowercase, non-alphanumerics → '-', collapse repeats, trim.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn actor_slug() -> String {
    // Prefer git user.email.
    if let Ok(o) = Command::new("git").args(["config", "user.email"]).output() {
        if o.status.success() {
            let email = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !email.is_empty() {
                return slugify(&email);
            }
        }
    }
    // Fallback: $USER / whoami.
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() {
            return slugify(&u);
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slug_is_nonempty_and_lowercase() {
        let s = actor_slug();
        assert!(!s.is_empty());
        assert_eq!(s, s.to_lowercase());
        assert!(!s.contains(' '));
    }
    #[test]
    fn slugify_rules() {
        assert_eq!(slugify("Im.Ionut@Gmail.com"), "im-ionut-gmail-com");
        assert_eq!(slugify("a__b"), "a-b");
    }
}
