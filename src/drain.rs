use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

use crate::agents;
use crate::config;
use crate::queue::{self, Task, TaskState};

/// Consecutive failures that stop a drain. See the plan's Safety Model.
const BREAKER_LIMIT: usize = 2;

/// Hard ceiling on tasks attempted in one `drive()` call. The circuit breaker
/// only trips on consecutive *failures*, but a drained agent inherits
/// WS_WORKSPACE/WS_DIR and can run `ws -queue add` itself — a task whose
/// prompt leads it to enqueue a follow-up produces an unbounded loop where
/// every iteration "succeeds" and the breaker never engages. This cap is the
/// only thing that stops unbounded *success*; it is not expected to be hit by
/// a well-behaved queue.
const MAX_DRAIN_ITERATIONS: usize = 50;

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub ran: usize,
    pub failed: usize,
    pub tripped: bool,
    /// True if the drain stopped because it hit MAX_DRAIN_ITERATIONS, not
    /// because the queue emptied or the breaker tripped. Remaining tasks are
    /// left pending, same as a breaker trip.
    pub capped: bool,
}

fn journal(path: &Path, line: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot open journal {}", path.display()))?;
    writeln!(f, "[{}] {}", crate::now_iso(), line)?;
    Ok(())
}

/// Run pending tasks one at a time through `exec`, which returns Ok(true) for a
/// successful task. Marks each task running *before* executing it, so a crash is
/// recognisable afterwards. Stops after BREAKER_LIMIT consecutive failures,
/// leaving untried tasks pending.
pub fn drive<F>(tasks_path: &Path, journal_path: &Path, mut exec: F) -> Result<Outcome>
where
    F: FnMut(&Task) -> Result<bool>,
{
    let reaped = queue::reap_orphans(tasks_path)?;
    if reaped > 0 {
        journal(journal_path, &format!("reaped {reaped} interrupted task(s) as failed"))?;
    }

    let mut out = Outcome { ran: 0, failed: 0, tripped: false, capped: false };
    let mut consecutive = 0usize;
    let mut iterations = 0usize;

    // Re-read pending each iteration: a task's own run may have appended more.
    loop {
        let next = match queue::pending(tasks_path)?.into_iter().next() {
            Some(t) => t,
            None => break,
        };
        if iterations >= MAX_DRAIN_ITERATIONS {
            out.capped = true;
            journal(
                journal_path,
                &format!(
                    "iteration cap ({MAX_DRAIN_ITERATIONS}) reached — stopping drain, remaining task(s) left pending"
                ),
            )?;
            break;
        }
        iterations += 1;
        queue::set_state(tasks_path, &next.id, TaskState::Running, None)?;
        journal(journal_path, &format!("start: {}", next.text))?;

        let ok = match exec(&next) {
            Ok(ok) => ok,
            Err(e) => {
                journal(journal_path, &format!("error: {} — {e}", next.text))?;
                false
            }
        };
        out.ran += 1;

        if ok {
            consecutive = 0;
            queue::set_state(tasks_path, &next.id, TaskState::Done, None)?;
            journal(journal_path, &format!("ok: {}", next.text))?;
        } else {
            consecutive += 1;
            out.failed += 1;
            queue::set_state(tasks_path, &next.id, TaskState::Failed, Some("agent run failed"))?;
            journal(journal_path, &format!("failed: {}", next.text))?;
            if consecutive >= BREAKER_LIMIT {
                out.tripped = true;
                journal(
                    journal_path,
                    &format!("circuit breaker open after {consecutive} consecutive failures"),
                )?;
                break;
            }
        }
    }
    Ok(out)
}

/// Entry point for `ws -queue drain`. Holds the workspace lock for the whole
/// drain, refuses to start when another live process holds it, and refuses
/// while the circuit breaker is open.
pub fn run(name: Option<String>, reset: bool) -> Result<()> {
    let cfg = config::load();
    // Same resolution as -queue list and -who: honours $WS_WORKSPACE. Expose
    // commands::current_or_named to the crate (make it `pub(crate)`) rather than
    // hand-rolling a second lookup here.
    let (ws_name, root) = crate::commands::current_or_named(name)?;
    let ws = crate::workspace::Workspace { name: ws_name, root };

    let marker = ws.circuit_marker();
    if reset {
        if marker.exists() {
            std::fs::remove_file(&marker)
                .with_context(|| format!("cannot clear {}", marker.display()))?;
            println!("circuit breaker reset");
        }
    } else if marker.exists() {
        anyhow::bail!(
            "the circuit breaker is open for {} — inspect {} then rerun with --reset",
            ws.name,
            ws.queue_journal().display()
        );
    }

    // live_pid_checked, never live_pid: starting an unattended agent alongside a
    // live one is not a display decision. Note it takes the LOCK FILE, not the root.
    if let Some(pid) = crate::lock::live_pid_checked(&ws.lock_file())? {
        anyhow::bail!("{} is in use by pid {pid} — not starting a drain", ws.name);
    }

    // Same resolution order as commands::launch, minus the --agent override:
    // workspace default, then config default.
    let agent_id = crate::meta::read(&ws.workspace_toml())
        .default_agent
        .unwrap_or_else(|| cfg.default_agent.clone());
    let agent = agents::for_id(&agent_id)?;
    if !agent.is_installed() {
        anyhow::bail!("{agent_id} is not installed — cannot drain {}", ws.name);
    }

    // force=false: never steal a lock for an unattended run.
    let guard = crate::lock::acquire(&ws.lock_file(), false)?;
    let actor = crate::actors::actor_slug_in(&ws.root);
    crate::timeline::record(&ws.timeline(), "drain-start", &actor, serde_json::json!({}))?;

    let mut first = true;
    let outcome = drive(&ws.queue_tasks(), &ws.queue_journal(), |task| {
        let ctx = agents::LaunchCtx {
            // The first task of a drain starts fresh; later ones chain onto it.
            fresh: first,
            sessions_root: config::sessions_root(&cfg),
        };
        first = false;
        // A per-attempt, per-process-unique scratch path: codex's
        // `headless_succeeded` reads the agent's final message from here
        // instead of trusting stdout (C1). Not created by us — the agent
        // (or nothing, on refusal) writes it.
        let out_file = ws.local_dir().join(format!("headless-out-{}.json", uuid::Uuid::new_v4()));
        let mut cmd = agent.headless(&ws, &task.text, &ctx, &out_file)?;
        let out = cmd.output().with_context(|| format!("cannot run {agent_id}"))?;
        let ok = agent.headless_succeeded(&out, &out_file);
        let _ = std::fs::remove_file(&out_file); // best-effort; never block on cleanup
        Ok(ok)
    })?;

    crate::timeline::record(
        &ws.timeline(),
        "drain-end",
        &actor,
        serde_json::json!({
            "ran": outcome.ran,
            "failed": outcome.failed,
            "tripped": outcome.tripped,
            "capped": outcome.capped,
        }),
    )?;

    if outcome.tripped {
        crate::atomic::atomic_write(&marker, crate::now_iso().as_bytes())?;
        println!(
            "drained {} task(s), {} failed — circuit breaker open; see {}",
            outcome.ran,
            outcome.failed,
            ws.queue_journal().display()
        );
        // I3: `process::exit` does not unwind, so `LockGuard::drop` would
        // never run and `.ws/local/lock` would survive holding a now-dead
        // pid. Release it explicitly before the non-unwinding exit — the
        // exit status still needs to be non-zero, so this can't just
        // `return Err(..)` and let `main` unwind for us.
        drop(guard);
        std::process::exit(1);
    }
    if outcome.capped {
        println!(
            "drained {} task(s), {} failed — stopped at the {MAX_DRAIN_ITERATIONS}-task iteration cap; remaining task(s) left pending",
            outcome.ran, outcome.failed
        );
        return Ok(());
    }
    println!("drained {} task(s), {} failed", outcome.ran, outcome.failed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{self, TaskState};
    use tempfile::TempDir;

    fn setup() -> (TempDir, std::path::PathBuf) {
        let td = TempDir::new().unwrap();
        let tasks = td.path().join(".ws/queue/tasks.jsonl");
        std::fs::create_dir_all(td.path().join(".ws/local")).unwrap();
        (td, tasks)
    }

    #[test]
    fn drains_every_pending_task_in_order() {
        let (td, tasks) = setup();
        queue::add(&tasks, "one", "alice").unwrap();
        queue::add(&tasks, "two", "alice").unwrap();
        let seen = std::cell::RefCell::new(Vec::new());

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            seen.borrow_mut().push(t.text.clone());
            Ok(true)
        })
        .unwrap();

        assert_eq!(out.ran, 2);
        assert_eq!(out.failed, 0);
        assert!(!out.tripped);
        assert_eq!(*seen.borrow(), vec!["one".to_string(), "two".to_string()]);
        assert!(queue::tasks(&tasks).unwrap().iter().all(|t| t.state == TaskState::Done));
    }

    #[test]
    fn two_consecutive_failures_trip_the_breaker_and_leave_the_rest_pending() {
        let (td, tasks) = setup();
        for t in ["one", "two", "three", "four"] {
            queue::add(&tasks, t, "alice").unwrap();
        }
        let attempts = std::cell::Cell::new(0);

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |_| {
            attempts.set(attempts.get() + 1);
            Ok(false)
        })
        .unwrap();

        assert!(out.tripped, "breaker trips");
        assert_eq!(attempts.get(), 2, "stops after the second failure, does not attempt the rest");
        let ts = queue::tasks(&tasks).unwrap();
        assert_eq!(ts[0].state, TaskState::Failed);
        assert_eq!(ts[1].state, TaskState::Failed);
        assert_eq!(ts[2].state, TaskState::Pending, "untried tasks stay pending");
        assert_eq!(ts[3].state, TaskState::Pending);
    }

    #[test]
    fn a_success_between_two_failures_resets_the_breaker() {
        let (td, tasks) = setup();
        for t in ["fail", "ok", "fail2"] {
            queue::add(&tasks, t, "alice").unwrap();
        }
        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            Ok(t.text == "ok")
        })
        .unwrap();

        // Consecutive, not cumulative: 3 tasks, 2 failures, never 2 in a row.
        assert!(!out.tripped, "non-consecutive failures must not trip the breaker");
        assert_eq!(out.ran, 3);
        assert_eq!(out.failed, 2);
    }

    #[test]
    fn a_task_is_marked_running_before_it_executes() {
        let (td, tasks) = setup();
        queue::add(&tasks, "one", "alice").unwrap();
        let tasks2 = tasks.clone();

        drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            // Mid-flight, the log must already say running — that is what lets
            // reap_orphans recognise a crash instead of re-running the task.
            let live = queue::tasks(&tasks2).unwrap();
            assert_eq!(live.iter().find(|x| x.id == t.id).unwrap().state, TaskState::Running);
            Ok(true)
        })
        .unwrap();
    }

    /// Minor 2: bare `"one"`/`"two"` substrings already appear in the
    /// `start:` lines, so they don't distinguish "the outcome was recorded"
    /// from "the task was merely started". Assert the composed outcome lines
    /// instead — these only exist if the ok/failed journal write actually ran.
    #[test]
    fn the_journal_records_every_attempts_outcome() {
        let (td, tasks) = setup();
        queue::add(&tasks, "one", "alice").unwrap();
        queue::add(&tasks, "two", "alice").unwrap();
        let journal = td.path().join(".ws/local/journal.log");

        drive(&tasks, &journal, |t| Ok(t.text == "one")).unwrap();

        let log = std::fs::read_to_string(&journal).unwrap();
        assert!(log.contains("ok: one"), "task one's outcome is recorded: {log}");
        assert!(log.contains("failed: two"), "task two's outcome is recorded: {log}");
    }

    #[test]
    fn an_orphaned_running_task_is_failed_not_rerun() {
        let (td, tasks) = setup();
        let a = queue::add(&tasks, "crashed", "alice").unwrap();
        queue::add(&tasks, "fresh", "alice").unwrap();
        queue::set_state(&tasks, &a, TaskState::Running, None).unwrap();
        let seen = std::cell::RefCell::new(Vec::new());

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |t| {
            seen.borrow_mut().push(t.text.clone());
            Ok(true)
        })
        .unwrap();

        assert_eq!(*seen.borrow(), vec!["fresh".to_string()], "the crashed task is NOT re-run");
        assert_eq!(out.ran, 1);
        assert_eq!(queue::tasks(&tasks).unwrap()[0].state, TaskState::Failed);
    }

    #[test]
    fn an_empty_queue_drains_to_nothing_without_error() {
        let (td, tasks) = setup();
        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), |_| Ok(true)).unwrap();
        assert_eq!(out.ran, 0);
        assert!(!out.tripped);
    }

    /// I4. A drained agent has WS_WORKSPACE/WS_DIR and can run `ws -queue
    /// add` itself; every self-enqueue "succeeds", so the breaker (which
    /// only counts consecutive failures) never engages. Simulate exactly
    /// that: every task succeeds AND enqueues one more task from inside
    /// `exec`. Without a cap this loops forever; with it, `drive` must stop
    /// at MAX_DRAIN_ITERATIONS, report `capped`, and leave work pending.
    #[test]
    fn a_self_enqueuing_agent_is_stopped_by_the_iteration_cap() {
        let (td, tasks) = setup();
        queue::add(&tasks, "seed", "alice").unwrap();
        let tasks_for_closure = tasks.clone();

        let out = drive(&tasks, &td.path().join(".ws/local/journal.log"), move |t| {
            queue::add(&tasks_for_closure, &format!("child-of-{}", t.text), "alice").unwrap();
            Ok(true)
        })
        .unwrap();

        assert!(out.capped, "the cap must be the reason this drain stopped");
        assert!(!out.tripped, "every task succeeded — the breaker must never engage");
        assert_eq!(out.ran, MAX_DRAIN_ITERATIONS, "must stop at exactly the cap");

        let remaining = queue::pending(&tasks).unwrap();
        assert!(!remaining.is_empty(), "the self-enqueued surplus must be left pending, not silently dropped");
    }
}
