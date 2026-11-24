use std::path::PathBuf;

use crate::workspace::Workspace;

/// Newest `.ws/handoffs/*.md` by mtime, if any exist.
pub fn latest_handoff(ws: &Workspace) -> Option<PathBuf> {
    let dir = ws.ws_dir().join("handoffs");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
            if newest.as_ref().is_none_or(|(t, _)| m > *t) {
                newest = Some((m, p));
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// An armed handoff older than this is out of date. Generous, because the
/// user is now asked before one is used: a rate-limit handoff can wait out a
/// weekly window, and a question about an old handoff costs one "no".
pub const MARKER_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

fn marker(ws: &Workspace) -> PathBuf {
    ws.local_dir().join("pending-handoff")
}

fn stamp(ws: &Workspace) -> PathBuf {
    ws.local_dir().join("handoff-notice.stamp")
}

/// Arm `handoff` for the next conversation. Stores the file name only (the
/// consumer resolves it under `.ws/handoffs/`, so the marker can never point
/// anywhere else) and, on a second line, the session that armed it: that
/// conversation resuming is not a new one and must not be asked.
pub fn arm(ws: &Workspace, handoff: &std::path::Path, by: Option<&str>) -> anyhow::Result<()> {
    let name = handoff
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("handoff has no file name: {}", handoff.display()))?;
    let body = match by {
        Some(id) if !id.trim().is_empty() => format!("{name}\n{}\n", id.trim()),
        _ => name.to_string(),
    };
    crate::atomic::atomic_write(&marker(ws), body)
}

/// A handoff armed by `ws -rotate` and not yet offered.
#[derive(Debug, PartialEq)]
pub struct Armed {
    pub path: PathBuf,
    /// The session that armed it, when known.
    pub by: Option<String>,
    pub age_secs: u64,
}

impl Armed {
    pub fn name(&self) -> &str {
        self.path.file_name().and_then(|f| f.to_str()).unwrap_or_default()
    }
}

/// The armed handoff, without consuming it. A marker that is invalid, stale or
/// points at a missing file is removed here, not left to fire later.
pub fn armed(ws: &Workspace) -> Option<Armed> {
    let m = marker(ws);
    let age = std::fs::metadata(&m).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    let raw = std::fs::read_to_string(&m).unwrap_or_default();
    let mut lines = raw.lines().map(str::trim);
    let name = lines.next().unwrap_or_default();
    let by = lines.next().filter(|s| !s.is_empty()).map(str::to_string);
    let valid = age.as_secs() <= MARKER_MAX_AGE_SECS
        && !(name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']));
    let p = ws.ws_dir().join("handoffs").join(name);
    if !valid || !p.is_file() {
        let _ = std::fs::remove_file(&m);
        return None;
    }
    Some(Armed { path: p, by, age_secs: age.as_secs() })
}

/// The handoff has been offered: drop the marker, and stamp it as announced so
/// the "handoff waiting" banner does not bring it up again.
pub fn disarm(ws: &Workspace, a: &Armed) {
    let _ = std::fs::remove_file(marker(ws));
    let _ = std::fs::create_dir_all(ws.local_dir());
    let _ = std::fs::write(stamp(ws), a.name());
}

/// "3m", "5h", "2d": how long ago a handoff was armed.
pub fn age_label(secs: u64) -> String {
    match secs {
        s if s < 3600 => format!("{}m", (s / 60).max(1)),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The opening message a launch hands the agent once the user said yes.
pub fn continue_prompt(name: &str) -> String {
    format!(
        "Continue from the handoff .ws/handoffs/{name}: read it before anything else and take \
         its Next step as your first action. When that step is done, append a \
         `## Successor report` to the handoff: what you had to look up again, re-derive, or \
         found wrong — or `none`."
    )
}

/// The terminal line shown when a conversation opens with a handoff armed.
/// Claude Code gives a hook no way to make the agent speak first, so the user
/// is told how to answer, and the agent reads their first message as the answer.
pub fn ask_banner(a: &Armed) -> String {
    format!(
        "ws: there is a handoff active: .ws/handoffs/{} ({} old). Start it? Type y to start \
         it, or n to start fresh.",
        a.name(),
        age_label(a.age_secs)
    )
}

/// The model-side half of `ask_banner`.
pub fn ask_directive(a: &Armed) -> String {
    let name = a.name();
    format!(
        "\n\nThere is a handoff active: .ws/handoffs/{name} (armed {age} ago). The terminal \
         told the user: \"Start it? Type y to start it, or n to start fresh.\" Read their first \
         message as the answer:\n\
         - y / yes / start / continue: read .ws/handoffs/{name} before anything else and take \
         its Next step as your first action. When that step is done, append a \
         `## Successor report` to the handoff: what you had to look up again, re-derive, or \
         found wrong — or `none`.\n\
         - n / no / fresh: do not read the handoff. Say in one line that you are starting \
         fresh, then do what they ask.\n\
         - anything else: before acting on it, ask \"There is a handoff active ({name}). \
         Start it?\" with the options \"Yes, start it\" and \"No, start fresh\" (Claude: \
         AskUserQuestion with header \"Handoff\"; Codex and agy: one line, then wait). On yes, \
         continue the handoff as above; on no, do what their message asked.",
        age = age_label(a.age_secs)
    )
}

/// The one-line terminal banner for a fresh conversation with no handoff
/// armed: the newest handoff not yet announced. Each handoff is announced once
/// (the stamp holds its name), so ignoring it is a durable "no". It goes out as
/// `systemMessage`, which the user sees and the model never does.
pub fn notice(ws: &Workspace) -> Option<String> {
    let path = latest_handoff(ws)?;
    let name = path.file_name()?.to_str()?.to_string();
    if std::fs::read_to_string(stamp(ws)).is_ok_and(|s| s.trim() == name) {
        return None;
    }
    let _ = std::fs::create_dir_all(ws.local_dir());
    let _ = std::fs::write(stamp(ws), &name);
    Some(format!(
        "ws: handoff waiting: .ws/handoffs/{name}. To pick it up, ask the agent to continue \
         from it. Otherwise ignore this; it won't be shown again."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(td: &TempDir) -> Workspace {
        Workspace { name: "t".into(), root: td.path().to_path_buf() }
    }

    fn handoff(w: &Workspace, name: &str) -> PathBuf {
        let dir = w.ws_dir().join("handoffs");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "# Handoff").unwrap();
        p
    }

    #[test]
    fn an_armed_handoff_is_offered_until_disarmed() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "2026-09-30T120000Z-me.md");
        arm(&w, &h, Some("s-1")).unwrap();
        let a = armed(&w).expect("armed");
        assert_eq!((a.path.clone(), a.by.as_deref()), (h, Some("s-1")));
        assert!(armed(&w).is_some(), "peeking does not consume");
        disarm(&w, &a);
        assert_eq!(armed(&w), None);
        assert_eq!(notice(&w), None, "an offered handoff is not announced again");
    }

    #[test]
    fn a_marker_from_before_the_session_line_still_arms() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        handoff(&w, "old-format.md");
        std::fs::create_dir_all(w.local_dir()).unwrap();
        std::fs::write(w.local_dir().join("pending-handoff"), "old-format.md").unwrap();
        assert_eq!(armed(&w).unwrap().by, None);
    }

    #[test]
    fn a_marker_naming_a_path_is_refused_and_removed() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        std::fs::create_dir_all(w.local_dir()).unwrap();
        for bad in ["../../etc/passwd", "a/b.md", "a\\b.md", ".."] {
            std::fs::write(w.local_dir().join("pending-handoff"), bad).unwrap();
            assert_eq!(armed(&w), None, "{bad}");
            assert!(!w.local_dir().join("pending-handoff").exists(), "{bad} left armed");
        }
    }

    #[test]
    fn a_marker_whose_handoff_is_gone_offers_nothing() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "x.md");
        arm(&w, &h, None).unwrap();
        std::fs::remove_file(&h).unwrap();
        assert_eq!(armed(&w), None);
    }

    #[test]
    fn a_marker_older_than_the_max_age_is_dropped() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "old.md");
        arm(&w, &h, None).unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(MARKER_MAX_AGE_SECS + 60);
        let f = std::fs::File::options()
            .write(true)
            .open(w.local_dir().join("pending-handoff"))
            .unwrap();
        f.set_modified(old).unwrap();
        assert_eq!(armed(&w), None);
        assert!(!w.local_dir().join("pending-handoff").exists());
    }

    #[test]
    fn a_day_old_handoff_is_still_offered() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "day.md");
        arm(&w, &h, None).unwrap();
        let f = std::fs::File::options()
            .write(true)
            .open(w.local_dir().join("pending-handoff"))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(26 * 3600))
            .unwrap();
        let a = armed(&w).expect("a rate-limit handoff can wait a day");
        assert!(ask_banner(&a).contains("(1d old)"), "{}", ask_banner(&a));
    }

    #[test]
    fn the_question_names_both_answers() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        arm(&w, &handoff(&w, "q.md"), None).unwrap();
        let a = armed(&w).unwrap();
        let b = ask_banner(&a);
        assert!(
            b.contains("handoff active") && b.contains("q.md") && b.contains("Start it?"),
            "{b}"
        );
        let d = ask_directive(&a);
        assert!(d.contains("Yes, start it") && d.contains("No, start fresh"), "{d}");
        assert!(d.contains("Successor report"), "{d}");
    }

    #[test]
    fn a_waiting_handoff_is_announced_once() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        handoff(&w, "a.md");
        let n = notice(&w).expect("first open announces it");
        assert!(n.contains(".ws/handoffs/a.md"), "{n}");
        assert_eq!(notice(&w), None, "already announced");
    }

    #[test]
    fn a_newer_handoff_is_announced_after_an_older_one() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let a = handoff(&w, "a.md");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&a).unwrap().set_modified(old).unwrap();
        notice(&w).unwrap();
        handoff(&w, "b.md");
        assert!(notice(&w).unwrap().contains("b.md"));
    }

    #[test]
    fn no_handoffs_means_no_notice() {
        let td = TempDir::new().unwrap();
        assert_eq!(notice(&ws(&td)), None);
    }
}
