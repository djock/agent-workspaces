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
    /// `-task rm`. Append-only: the task is retired by a later record rather
    /// than by rewriting the log, because a rewrite would need a lock this
    /// O_APPEND-only file deliberately does not have.
    Drop { ts: String, id: String },
}

/// Ceiling on one appended **line**, in bytes.
///
/// `add` joins the whole record into one JSON line and appends it with a single
/// `O_APPEND` write (`append`, below); POSIX only guarantees that write atomic
/// up to a platform-specific limit in practice, and a multi-KiB line risks being
/// torn by a concurrent append from another `ws` process. `tasks` treats any
/// line it cannot parse as a hard error (`corrupt queue record`), so one torn
/// line does not lose one task — it bricks the whole queue for every reader.
///
/// The cap is on the serialized line, not on the caller's text, because that is
/// what the invariant is actually about. Checking `text.len()` under-counted
/// twice over: the JSON envelope adds ~110 bytes, and JSON escapes a control
/// character to six (`\u0000`), so 8,192 bytes of control characters produced a
/// ~49 KiB line — six times the limit the cap existed to enforce.
pub const MAX_TASK_LINE_BYTES: usize = 8192;

fn append(tasks_path: &Path, rec: &Record) -> Result<()> {
    if let Some(dir) = tasks_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut line = serde_json::to_string(rec)?;
    if line.len() + 1 > MAX_TASK_LINE_BYTES {
        anyhow::bail!(
            "this task serializes to {} bytes, over the {MAX_TASK_LINE_BYTES}-byte line cap: \
             each record is appended as one line with a single O_APPEND write, and a line this \
             large risks being torn by a concurrent append from another `ws` process — which \
             corrupts the whole queue, not just this task. Shorten it, or split it into several \
             smaller tasks.",
            line.len() + 1
        );
    }
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

/// Retire a task. Append-only, like every other mutation here.
pub fn remove(tasks_path: &Path, id: &str) -> Result<()> {
    append(tasks_path, &Record::Drop { ts: crate::now_iso(), id: id.to_string() })
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
            Record::Drop { id, .. } => {
                out.retain(|t| t.id != id);
            }
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
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn q(td: &TempDir) -> std::path::PathBuf {
        td.path().join("queue/tasks.jsonl")
    }

    #[test]
    fn add_rejects_a_line_over_the_size_cap_and_writes_nothing() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let big = "x".repeat(MAX_TASK_LINE_BYTES + 1);

        let err = add(&p, &big, "alice").unwrap_err();
        assert!(
            err.to_string().contains(&MAX_TASK_LINE_BYTES.to_string()),
            "the error must name the cap: {err}"
        );
        assert!(!p.exists(), "an oversized task must never be appended");
    }

    /// The discriminator for capping the serialized line rather than the input
    /// text. JSON escapes a control character to six bytes (`\u0000`), so text
    /// that passes a naive `text.len()` check by a wide margin still produces a
    /// line several times over the cap — which is the torn-write this cap exists
    /// to prevent.
    #[test]
    fn add_rejects_text_whose_escaped_form_blows_the_cap() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let sneaky = "\u{0}".repeat(2000); // 2 KiB of input, ~12 KiB serialized

        assert!(sneaky.len() < MAX_TASK_LINE_BYTES, "the input itself is under the cap");
        let err = add(&p, &sneaky, "alice").unwrap_err();
        assert!(
            err.to_string().contains("serializes to"),
            "the error must be about the serialized line: {err}"
        );
        assert!(!p.exists(), "nothing may be appended");
    }

    #[test]
    fn add_accepts_text_that_fits_once_serialized() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        // Leave room for the ~110-byte JSON envelope.
        let exact = "x".repeat(MAX_TASK_LINE_BYTES - 200);
        assert!(add(&p, &exact, "alice").is_ok());
        assert_eq!(tasks(&p).unwrap()[0].text.len(), MAX_TASK_LINE_BYTES - 200);
    }

    #[test]
    fn remove_retires_a_task_and_leaves_the_others() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "keep me", "alice").unwrap();
        let b = add(&p, "drop me", "alice").unwrap();

        remove(&p, &b).unwrap();

        let ts = tasks(&p).unwrap();
        assert_eq!(ts.len(), 1, "one task retired");
        assert_eq!(ts[0].id, a);
        assert_eq!(ts[0].text, "keep me");
    }

    /// `remove` appends rather than rewriting, so dropping an id that is not
    /// there must be a no-op and must not corrupt the fold.
    #[test]
    fn removing_an_unknown_id_changes_nothing() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        add(&p, "only task", "alice").unwrap();
        remove(&p, "not-a-real-id").unwrap();
        assert_eq!(tasks(&p).unwrap().len(), 1);
    }

    #[test]
    fn a_removed_task_is_not_pending() {
        let td = TempDir::new().unwrap();
        let p = q(&td);
        let a = add(&p, "task", "alice").unwrap();
        assert_eq!(pending(&p).unwrap().len(), 1);
        remove(&p, &a).unwrap();
        assert_eq!(pending(&p).unwrap().len(), 0);
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

}
