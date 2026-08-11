use anyhow::Result;
use std::path::Path;

fn is_placeholder(line: &str) -> bool {
    let t = line.trim();
    (t.starts_with("_(") && t.ends_with(")_")) || (t.starts_with('[') && t.ends_with(']'))
}

/// Extract the first non-empty, non-placeholder line of the README `## Objective`
/// section, or None if that section is still the placeholder (or absent).
pub fn objective_of(readme: &str) -> Option<String> {
    let mut in_obj = false;
    for line in readme.lines() {
        if line.starts_with("## ") {
            in_obj = line.trim() == "## Objective";
            continue;
        }
        if in_obj {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if is_placeholder(t) {
                return None;
            }
            return Some(t.to_string());
        }
    }
    None
}

/// Replace the placeholder line inside the `## Objective` section with `objective`,
/// but only while it is still a placeholder (a whole line that is `_(...)_` or
/// `[...]`). First real prompt wins; a real objective is never overwritten.
/// Returns Ok(true) if it wrote a change, Ok(false) if nothing to do.
/// Record the first prompt as the workspace's objective, once.
///
/// Transacted: this is a read-modify-write of a git-tracked file holding the
/// user's own prose, driven from the `UserPromptSubmit` hook — so two sessions
/// starting at once could each read the placeholder and write back only their own
/// replacement.
pub fn capture_objective(readme_path: &Path, objective: &str) -> Result<bool> {
    crate::txn::transaction(readme_path, || capture_objective_locked(readme_path, objective))
}

fn capture_objective_locked(readme_path: &Path, objective: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(readme_path) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };
    let value: String = objective.lines().next().unwrap_or("").trim().chars().take(200).collect();
    if value.is_empty() {
        return Ok(false);
    }
    // Split keeping terminators so untouched lines round-trip byte-for-byte.
    let mut out = String::with_capacity(text.len() + value.len());
    let mut in_obj = false;
    let mut replaced = false;
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let (line, term, next) = split_line(rest); // line without terminator, its terminator, remainder
        rest = next;
        if line.starts_with("## ") {
            in_obj = line.trim() == "## Objective";
        }
        if in_obj && !replaced && is_placeholder(line.trim()) {
            out.push_str(&value);
            out.push_str(term);
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push_str(term);
    }
    if replaced {
        // Atomic, not `fs::write`: this is a read-modify-write of a README
        // whose every other line is the user's, and `fs::write` truncates
        // first. One replaced placeholder must never be able to cost the
        // whole file.
        crate::atomic::atomic_write(readme_path, out)?;
    }
    Ok(replaced)
}

/// Split `s` into (line-without-terminator, terminator, remainder). Terminator is
/// "\r\n", "\n", or "" at EOF.
fn split_line(s: &str) -> (&str, &str, &str) {
    match s.find('\n') {
        Some(i) => {
            if i > 0 && s.as_bytes()[i - 1] == b'\r' {
                (&s[..i - 1], "\r\n", &s[i + 1..])
            } else {
                (&s[..i], "\n", &s[i + 1..])
            }
        }
        None => (s, "", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TEMPLATE: &str =
        "# proj\n\n## Objective\n\n_(captured from the first prompt)_\n\n## Outcome\n\n";

    #[test]
    fn replaces_placeholder_only_within_objective() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, TEMPLATE).unwrap();

        let wrote = capture_objective(&f, "Build the thing\nsecond line ignored").unwrap();
        assert!(wrote);
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("Build the thing"));
        assert!(!s.contains("_(captured from the first prompt)_"));
        // Outcome section placeholder-free area untouched, headings intact
        assert!(s.contains("## Objective"));
        assert!(s.contains("## Outcome"));
        // only first line used
        assert!(!s.contains("second line ignored"));
    }

    #[test]
    fn does_not_overwrite_real_objective() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, "# p\n\n## Objective\n\nAlready written by hand.\n\n## Outcome\n")
            .unwrap();

        let wrote = capture_objective(&f, "New prompt").unwrap();
        assert!(!wrote);
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("Already written by hand."));
        assert!(!s.contains("New prompt"));
    }

    #[test]
    fn bracket_placeholder_also_matches() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, "# p\n\n## Objective\n\n[To be filled]\n\n## Outcome\n").unwrap();
        assert!(capture_objective(&f, "Real goal").unwrap());
        assert!(std::fs::read_to_string(&f).unwrap().contains("Real goal"));
    }

    #[test]
    fn objective_heading_is_exact_not_prefix() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        // A different section whose heading merely starts with "Objective" must be ignored.
        std::fs::write(&f, "# p\n\n## Objectives archive\n\n[old]\n\n## Objective\n\n_(captured from the first prompt)_\n\n## Outcome\n").unwrap();
        assert!(capture_objective(&f, "Real goal").unwrap());
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("Real goal"));
        assert!(s.contains("[old]"), "the 'Objectives archive' section must be untouched");
    }

    #[test]
    fn preserves_crlf_untouched_lines() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, "# p\r\n\r\n## Objective\r\n\r\n_(captured from the first prompt)_\r\n\r\n## Outcome\r\n").unwrap();
        capture_objective(&f, "Goal").unwrap();
        let s = std::fs::read_to_string(&f).unwrap();
        // untouched lines keep their CRLF
        assert!(s.contains("## Outcome\r\n"), "existing CRLF lines must be preserved");
    }

    /// I3, same reasoning as `context.rs`: capturing the objective is a
    /// read-modify-write of a file the user also writes. `fs::write`
    /// truncates and refills one inode, leaving a window with neither the old
    /// README nor the new one on disk; temp-plus-rename lands on a new inode
    /// because the old file is only ever replaced by a complete one.
    #[test]
    #[cfg(unix)]
    fn capturing_the_objective_never_truncates_the_readme_in_place() {
        use std::os::unix::fs::MetadataExt;
        let d = TempDir::new().unwrap();
        let f = d.path().join("README.md");
        std::fs::write(&f, format!("{TEMPLATE}## Notes\n\nuser prose worth keeping\n")).unwrap();
        let before = std::fs::metadata(&f).unwrap().ino();

        assert!(capture_objective(&f, "Build the thing").unwrap());

        let after = std::fs::metadata(&f).unwrap().ino();
        assert_ne!(before, after, "the README must be replaced by rename, not truncated in place");
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("user prose worth keeping"), "and the user's prose is there");
        assert!(s.contains("Build the thing"), "and the objective was captured");
    }
}
