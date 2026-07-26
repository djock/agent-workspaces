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
