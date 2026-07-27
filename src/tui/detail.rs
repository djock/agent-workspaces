//! Everything the detail pane shows for one workspace, read on demand.
//!
//! Nothing here is fatal: a workspace mid-creation, a half-written notebook, or
//! a timeline line from a future version all degrade to "less to show".
use crate::rows::WorkspaceRow;

#[derive(Debug, Clone, PartialEq)]
pub struct ChainEntry {
    pub ts: String,
    pub kind: String,
    pub actor: String,
}

#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub objective: Option<String>,
    /// Tail of the most recently modified notebook, oldest first.
    pub notebook: Vec<String>,
    /// Tail of `timeline.jsonl`, oldest first — the workspace's conversation chain.
    pub chain: Vec<ChainEntry>,
    /// Pending task count, or None when the queue could not be read.
    pub queue: Option<usize>,
    /// Unread count, or None when the mailbox could not be read. Rendering "?"
    /// for unreadable beats rendering "0", which would be a lie.
    pub mail: Option<usize>,
}

/// The most recently modified `notebook.<actor>.md`.
fn newest_notebook(notebook_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(notebook_dir).ok()?.flatten() {
        let p = e.path();
        let name = p.file_name()?.to_string_lossy().to_string();
        if !(name.starts_with("notebook.") && name.ends_with(".md")) {
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

pub fn gather(row: &WorkspaceRow, max_lines: usize) -> Detail {
    let ws = row.path.join(".ws");

    let objective = std::fs::read_to_string(ws.join("README.md"))
        .ok()
        .and_then(|s| crate::readme::objective_of(&s));

    let notebook = newest_notebook(&ws.join("notebook"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            let lines: Vec<String> = s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
            lines[lines.len().saturating_sub(max_lines)..].to_vec()
        })
        .unwrap_or_default();

    let chain = std::fs::read_to_string(ws.join("timeline.jsonl"))
        .map(|s| {
            let all: Vec<ChainEntry> = s
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .map(|v| ChainEntry {
                    ts: v["ts"].as_str().unwrap_or_default().to_string(),
                    kind: v["kind"].as_str().unwrap_or_default().to_string(),
                    actor: v["actor"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            all[all.len().saturating_sub(max_lines)..].to_vec()
        })
        .unwrap_or_default();

    Detail {
        objective,
        notebook,
        chain,
        queue: crate::queue::pending(&ws.join("queue/tasks.jsonl")).ok().map(|t| t.len()),
        mail: crate::mail::unread(&ws.join("mail"), &ws.join("local/mail-seen"))
            .ok()
            .map(|m| m.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{RowState, WorkspaceRow};
    use tempfile::TempDir;

    fn ws_at(path: std::path::PathBuf) -> WorkspaceRow {
        WorkspaceRow {
            name: "alpha".into(), path, state: RowState::Ok, agent: "claude".into(),
            live_pid: None, archived: false, tags: vec![], status: None, color: None,
            last_activity: None, limits: None,
        }
    }

    #[test]
    fn gathers_objective_notebook_tail_and_chain() {
        let d = TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("notebook")).unwrap();
        std::fs::write(
            ws.join("README.md"),
            "# Workspace: alpha\n\n## Objective\n\nShip the TUI.\n\n## Environment\n\nmacOS\n",
        )
        .unwrap();
        std::fs::write(ws.join("notebook/notebook.me.md"), "line one\nline two\nline three\n").unwrap();
        std::fs::write(
            ws.join("timeline.jsonl"),
            "{\"ts\":\"2026-07-01T00:00:00Z\",\"kind\":\"created\",\"actor\":\"me\"}\n\
             {\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"me\"}\n",
        )
        .unwrap();

        let det = gather(&ws_at(d.path().to_path_buf()), 2);
        assert_eq!(det.objective.as_deref(), Some("Ship the TUI."));
        assert_eq!(det.notebook, vec!["line two".to_string(), "line three".to_string()],
                   "the tail, newest last");
        assert_eq!(det.chain.len(), 2);
        assert_eq!(det.chain[1].kind, "opened", "newest last");
        assert_eq!(det.queue, Some(0));
        assert_eq!(det.mail, Some(0));
    }

    #[test]
    fn a_workspace_with_nothing_in_it_gathers_empty_not_panicking() {
        let d = TempDir::new().unwrap();
        let det = gather(&ws_at(d.path().to_path_buf()), 5);
        assert!(det.objective.is_none());
        assert!(det.notebook.is_empty());
        assert!(det.chain.is_empty());
    }

    #[test]
    fn a_corrupt_timeline_line_is_skipped_not_fatal() {
        let d = TempDir::new().unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("timeline.jsonl"),
            "not json at all\n{\"ts\":\"2026-07-02T00:00:00Z\",\"kind\":\"opened\",\"actor\":\"me\"}\n",
        )
        .unwrap();
        let det = gather(&ws_at(d.path().to_path_buf()), 5);
        assert_eq!(det.chain.len(), 1);
    }

    #[test]
    fn counts_pending_queue_tasks_and_reports_a_corrupt_queue_as_none() {
        let td = TempDir::new().unwrap();
        let ws = td.path().join(".ws");
        std::fs::create_dir_all(ws.join("local")).unwrap();
        let tasks = ws.join("queue/tasks.jsonl");
        let a = crate::queue::add(&tasks, "one", "alice").unwrap();
        crate::queue::add(&tasks, "two", "alice").unwrap();
        assert_eq!(gather(&ws_at(td.path().to_path_buf()), 5).queue, Some(2));

        crate::queue::set_state(&tasks, &a, crate::queue::TaskState::Done, None).unwrap();
        assert_eq!(
            gather(&ws_at(td.path().to_path_buf()), 5).queue,
            Some(1),
            "a finished task is not pending"
        );

        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&tasks).unwrap();
        writeln!(f, "{{not json").unwrap();
        assert_eq!(gather(&ws_at(td.path().to_path_buf()), 5).queue, None);
    }

    #[test]
    fn counts_unread_mail_and_reports_unreadable_as_none() {
        let td = TempDir::new().unwrap();
        let ws = td.path().join(".ws");
        std::fs::create_dir_all(ws.join("local")).unwrap();
        crate::mail::send(&ws.join("mail"), "alice", "hi").unwrap();
        let det = gather(&ws_at(td.path().to_path_buf()), 5);
        assert_eq!(det.mail, Some(1));

        std::fs::write(ws.join("mail/bad.json"), "{not json").unwrap();
        let det = gather(&ws_at(td.path().to_path_buf()), 5);
        assert_eq!(det.mail, None, "a corrupt message reads as unknown, not as zero");
    }
}
