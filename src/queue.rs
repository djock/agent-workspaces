use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub text: String,
    pub state: TaskState,
    pub added: String,
    pub note: Option<String>,
}

/// One line of the log. `add` records carry the text; `state` records carry a
/// transition. Current state is the left fold of the whole file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Record {
    Add { ts: String, id: String, text: String, actor: String },
    State { ts: String, id: String, state: TaskState, note: Option<String> },
}

fn append(tasks_path: &Path, rec: &Record) -> Result<()> {
    if let Some(dir) = tasks_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut line = serde_json::to_string(rec)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tasks_path)
        .with_context(|| format!("cannot open {}", tasks_path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("cannot append to {}", tasks_path.display()))?;
    Ok(())
}

pub fn add(tasks_path: &Path, text: &str, actor: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    append(
        tasks_path,
        &Record::Add {
            ts: crate::now_iso(),
            id: id.clone(),
            text: text.to_string(),
            actor: actor.to_string(),
        },
    )?;
    Ok(id)
}

pub fn set_state(tasks_path: &Path, id: &str, state: TaskState, note: Option<&str>) -> Result<()> {
    append(
        tasks_path,
        &Record::State {
            ts: crate::now_iso(),
            id: id.to_string(),
            state,
            note: note.map(str::to_string),
        },
    )
}

/// Fold the log into current task state, in add order. A missing file is an
/// empty queue; an unparseable line is an error.
pub fn tasks(tasks_path: &Path) -> Result<Vec<Task>> {
    let raw = match std::fs::read_to_string(tasks_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {}", tasks_path.display()))
        }
    };
    let mut out: Vec<Task> = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = serde_json::from_str(line).with_context(|| {
            format!("corrupt queue record at {}:{}", tasks_path.display(), i + 1)
        })?;
        match rec {
            Record::Add { ts, id, text, .. } => out.push(Task {
                id,
                text,
                state: TaskState::Pending,
                added: ts,
                note: None,
            }),
            Record::State { id, state, note, .. } => {
                // An id we have never seen is ignored rather than invented: a
                // state record cannot conjure a task with no text.
                if let Some(t) = out.iter_mut().find(|t| t.id == id) {
                    t.state = state;
                    if note.is_some() {
                        t.note = note;
                    }
                }
            }
        }
    }
    Ok(out)
}

pub fn pending(tasks_path: &Path) -> Result<Vec<Task>> {
    Ok(tasks(tasks_path)?
        .into_iter()
        .filter(|t| t.state == TaskState::Pending)
        .collect())
}

/// Mark every `Running` task `Failed`. Called at the start of a drain: a task
/// still marked running when no drain holds the lock is one whose process died.
/// It is failed, never re-run — re-running a half-finished task could repeat
/// destructive work, and re-queueing by hand is cheap.
pub fn reap_orphans(tasks_path: &Path) -> Result<usize> {
    let orphans: Vec<String> = tasks(tasks_path)?
        .into_iter()
        .filter(|t| t.state == TaskState::Running)
        .map(|t| t.id)
        .collect();
    for id in &orphans {
        set_state(
            tasks_path,
            id,
            TaskState::Failed,
            Some("interrupted: no drain was holding the lock"),
        )?;
    }
    Ok(orphans.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn q(td: &TempDir) -> std::path::PathBuf {
        td.path().join("queue/tasks.jsonl")
    }

    #[test]
    fn added_tasks_are_pending_in_add_order() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "first task", "alice").unwrap();
        let b = add(&p, "second task", "alice").unwrap();
        assert_ne!(a, b);

        let ts = tasks(&p).unwrap();
        assert_eq!(ts.len(), 2);
        assert_eq!(ts[0].text, "first task");
        assert_eq!(ts[1].text, "second task");
        assert!(ts.iter().all(|t| t.state == TaskState::Pending));
        assert_eq!(pending(&p).unwrap().len(), 2);
    }

    #[test]
    fn the_last_state_record_wins_and_pending_shrinks() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "first", "alice").unwrap();
        add(&p, "second", "alice").unwrap();

        set_state(&p, &a, TaskState::Running, None).unwrap();
        assert_eq!(tasks(&p).unwrap()[0].state, TaskState::Running);
        assert_eq!(pending(&p).unwrap().len(), 1, "running is not pending");

        set_state(&p, &a, TaskState::Done, Some("finished cleanly")).unwrap();
        let ts = tasks(&p).unwrap();
        assert_eq!(ts[0].state, TaskState::Done);
        assert_eq!(ts[0].note.as_deref(), Some("finished cleanly"));
        assert_eq!(pending(&p).unwrap().len(), 1);
    }

    #[test]
    fn a_state_record_for_an_unknown_id_is_ignored_not_a_phantom_task() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        add(&p, "real", "alice").unwrap();
        set_state(&p, "no-such-id", TaskState::Done, None).unwrap();
        assert_eq!(tasks(&p).unwrap().len(), 1);
    }

    #[test]
    fn a_missing_queue_is_empty_but_a_corrupt_line_is_an_error() {
        let td = TempDir::new().unwrap();
        assert!(tasks(&td.path().join("queue/tasks.jsonl")).unwrap().is_empty());

        let p = q(&td);
        add(&p, "real", "alice").unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{{not json").unwrap();
        // A queue that runs an agent unattended must never guess at its own
        // contents. Half a queue silently presented as the whole queue is how
        // a drain skips work the user asked for.
        assert!(tasks(&p).is_err());
    }

    #[test]
    fn reap_orphans_fails_running_tasks_and_leaves_the_rest_alone() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "crashed", "alice").unwrap();
        let b = add(&p, "untouched", "alice").unwrap();
        set_state(&p, &a, TaskState::Running, None).unwrap();

        assert_eq!(reap_orphans(&p).unwrap(), 1);
        let ts = tasks(&p).unwrap();
        assert_eq!(ts[0].state, TaskState::Failed, "a crashed task is failed, never retried");
        assert_eq!(ts[1].state, TaskState::Pending, "an untouched task is left pending");
        assert_eq!(pending(&p).unwrap()[0].id, b);

        assert_eq!(reap_orphans(&p).unwrap(), 0, "reaping twice is a no-op");
    }
}
