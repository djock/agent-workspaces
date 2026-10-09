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

/// An armed handoff older than this is out of date: the user rotated, then
/// carried on in the old conversation instead of clearing.
pub const MARKER_MAX_AGE_SECS: u64 = 6 * 3600;

fn marker(ws: &Workspace) -> PathBuf {
    ws.local_dir().join("pending-handoff")
}

/// Arm `handoff` for the next fresh conversation. Stores the file name only:
/// the consumer resolves it under `.ws/handoffs/`, so the marker can never
/// point anywhere else.
pub fn arm(ws: &Workspace, handoff: &std::path::Path) -> anyhow::Result<()> {
    let name = handoff
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("handoff has no file name: {}", handoff.display()))?;
    crate::atomic::atomic_write(&marker(ws), name)
}

/// Consume the armed handoff. The marker is removed whatever it held: a marker
/// that is invalid, stale or points at a missing file is dropped, not left to
/// fire later.
pub fn take(ws: &Workspace) -> Option<PathBuf> {
    let m = marker(ws);
    let age = std::fs::metadata(&m).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    let raw = std::fs::read_to_string(&m).unwrap_or_default();
    let _ = std::fs::remove_file(&m);
    if age.as_secs() > MARKER_MAX_AGE_SECS {
        return None;
    }
    let name = raw.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return None;
    }
    let p = ws.ws_dir().join("handoffs").join(name);
    p.is_file().then_some(p)
}

/// The one-line terminal banner for a fresh conversation: the rotation it is
/// continuing (`consumed`), or else the newest handoff not yet announced. Each
/// handoff is announced once — the stamp holds its name — so ignoring it is a
/// durable "no". The banner goes out as `systemMessage`, which the user sees
/// and the model never does: telling the user costs no tokens, and the handoff
/// is read only if they ask for it.
pub fn notice(ws: &Workspace, consumed: Option<&std::path::Path>) -> Option<String> {
    let stamp = ws.local_dir().join("handoff-notice.stamp");
    let path = match consumed {
        Some(p) => p.to_path_buf(),
        None => latest_handoff(ws)?,
    };
    let name = path.file_name()?.to_str()?.to_string();
    if consumed.is_none() && std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == name) {
        return None;
    }
    let _ = std::fs::create_dir_all(ws.local_dir());
    let _ = std::fs::write(&stamp, &name);
    Some(match consumed {
        Some(_) => format!("ws: Continuing from handoff .ws/handoffs/{name}"),
        None => format!(
            "ws: handoff waiting: .ws/handoffs/{name}. To pick it up, ask the agent to \
             continue from it. Otherwise ignore this; it won't be shown again."
        ),
    })
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
    fn an_armed_handoff_is_taken_once() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "2026-09-30T120000Z-me.md");
        arm(&w, &h).unwrap();
        assert_eq!(take(&w), Some(h));
        assert_eq!(take(&w), None, "consumed");
    }

    #[test]
    fn a_marker_naming_a_path_is_refused_and_removed() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        std::fs::create_dir_all(w.local_dir()).unwrap();
        for bad in ["../../etc/passwd", "a/b.md", "a\\b.md", ".."] {
            std::fs::write(w.local_dir().join("pending-handoff"), bad).unwrap();
            assert_eq!(take(&w), None, "{bad}");
            assert!(!w.local_dir().join("pending-handoff").exists(), "{bad} left armed");
        }
    }

    #[test]
    fn a_marker_whose_handoff_is_gone_takes_nothing() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "x.md");
        arm(&w, &h).unwrap();
        std::fs::remove_file(&h).unwrap();
        assert_eq!(take(&w), None);
    }

    #[test]
    fn a_marker_older_than_the_max_age_is_dropped_not_taken() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "old.md");
        arm(&w, &h).unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(MARKER_MAX_AGE_SECS + 60);
        let f = std::fs::File::options()
            .write(true)
            .open(w.local_dir().join("pending-handoff"))
            .unwrap();
        f.set_modified(old).unwrap();
        assert_eq!(take(&w), None);
        assert!(!w.local_dir().join("pending-handoff").exists());
    }

    #[test]
    fn a_waiting_handoff_is_announced_once() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        handoff(&w, "a.md");
        let n = notice(&w, None).expect("first open announces it");
        assert!(n.contains(".ws/handoffs/a.md"), "{n}");
        assert_eq!(notice(&w, None), None, "already announced");
    }

    #[test]
    fn a_newer_handoff_is_announced_after_an_older_one() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let a = handoff(&w, "a.md");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&a).unwrap().set_modified(old).unwrap();
        notice(&w, None).unwrap();
        handoff(&w, "b.md");
        assert!(notice(&w, None).unwrap().contains("b.md"));
    }

    #[test]
    fn no_handoffs_means_no_notice() {
        let td = TempDir::new().unwrap();
        assert_eq!(notice(&ws(&td), None), None);
    }

    #[test]
    fn a_consumed_handoff_says_it_continues_and_is_not_announced_again() {
        let td = TempDir::new().unwrap();
        let w = ws(&td);
        let h = handoff(&w, "r.md");
        let n = notice(&w, Some(&h)).unwrap();
        assert!(n.contains("Continuing") && n.contains("r.md"), "{n}");
        assert_eq!(notice(&w, None), None, "the rotation already used it");
    }
}
