use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SESSION: &str = "ws";

#[derive(Debug, PartialEq)]
pub enum Attach {
    /// Already inside tmux: switch to the window, never attach (that nests).
    AlreadyInside,
    FromOutside,
}

#[derive(Debug, PartialEq)]
pub struct TmuxPlan {
    pub session: String,
    pub window: String,
    pub dir: PathBuf,
    pub command: String,
    pub attach: Attach,
}

pub fn plan(ws_name: &str, dir: &Path, inside_tmux: bool, drain: bool, ws_bin: &str) -> TmuxPlan {
    let command = if drain {
        format!("{ws_bin} -queue drain {ws_name}")
    } else {
        format!("{ws_bin} {ws_name}")
    };
    TmuxPlan {
        session: SESSION.to_string(),
        window: ws_name.to_string(),
        dir: dir.to_path_buf(),
        command,
        attach: if inside_tmux { Attach::AlreadyInside } else { Attach::FromOutside },
    }
}

/// The tmux argv lists to run, in order.
pub fn commands_for(plan: &TmuxPlan, session_exists: bool) -> Vec<Vec<String>> {
    let dir = plan.dir.to_string_lossy().to_string();
    let mut out: Vec<Vec<String>> = Vec::new();
    if session_exists {
        out.push(vec![
            "new-window".into(), "-t".into(), format!("{}:", plan.session),
            "-n".into(), plan.window.clone(), "-c".into(), dir, plan.command.clone(),
        ]);
    } else {
        out.push(vec![
            "new-session".into(), "-d".into(), "-s".into(), plan.session.clone(),
            "-n".into(), plan.window.clone(), "-c".into(), dir, plan.command.clone(),
        ]);
    }
    match plan.attach {
        Attach::AlreadyInside => out.push(vec![
            "select-window".into(),
            "-t".into(),
            format!("{}:{}", plan.session, plan.window),
        ]),
        Attach::FromOutside => {
            out.push(vec!["attach".into(), "-t".into(), plan.session.clone()])
        }
    }
    out
}

/// How many tasks are pending, and how many of them the user just added.
fn pending_summary(ws: &crate::workspace::Workspace) -> Result<usize> {
    Ok(crate::queue::pending(&ws.queue_tasks())?.len())
}

/// The line printed before a `--task` spawn. Kept separate from the tmux work
/// so it can be asserted without a terminal or a tmux server.
fn drain_scope_notice(pending: usize) -> String {
    if pending <= 1 {
        format!("queued the task; the window will drain {pending} pending task(s) unattended")
    } else {
        format!(
            "queued the task — note the window runs a full drain: {pending} pending task(s) \
             will run unattended, not just this one. `ws -queue list` to see them."
        )
    }
}

pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn run(name: String, task: Option<String>) -> Result<()> {
    let cfg = crate::config::load();
    let ws = crate::workspace::resolve(&name, &cfg);
    if !ws.exists() {
        anyhow::bail!("no workspace named {name}");
    }
    if !tmux_available() {
        anyhow::bail!("ws -spawn needs tmux, which is not installed (brew install tmux)");
    }

    let drain = task.is_some();
    if let Some(text) = task {
        let actor = crate::actors::actor_slug_in(&ws.root);
        crate::queue::add(&ws.queue_tasks(), &text, &actor)?;

        // I6: `--task "one thing"` reads as "run this one thing", but the
        // window runs a full `ws -queue drain`, which executes EVERY pending
        // task in the workspace — including ones queued days ago by another
        // actor, or self-enqueued by an agent — up to the drain's 50-task cap.
        // Say so before spawning rather than surprising the user with 50
        // unattended agent runs they did not ask for in this invocation.
        // (Scoping the drain to the seeded task was the alternative; it was
        // rejected because a partial drain leaves the rest of the queue
        // silently unrun with no journal entry saying why, and because the
        // pending-count line is also useful when the extra work is wanted.)
        println!("{}", drain_scope_notice(pending_summary(&ws)?));
    }

    let ws_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "ws".to_string());
    let inside = std::env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false);
    let p = plan(&name, &ws.root, inside, drain, &ws_bin);

    for argv in commands_for(&p, session_exists(&p.session)) {
        let status = Command::new("tmux")
            .args(&argv)
            .status()
            .with_context(|| format!("cannot run tmux {}", argv.join(" ")))?;
        if !status.success() {
            anyhow::bail!("tmux {} failed", argv.join(" "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        PathBuf::from("/tmp/proj")
    }

    #[test]
    fn a_fresh_session_is_created_and_attached_from_outside_tmux() {
        let plan = plan("proj", &dir(), false, false, "ws");
        let cmds = commands_for(&plan, false);
        assert_eq!(
            cmds[0],
            vec!["new-session", "-d", "-s", "ws", "-n", "proj", "-c", "/tmp/proj", "ws proj"]
        );
        assert_eq!(cmds[1], vec!["attach", "-t", "ws"], "outside tmux we attach");
    }

    #[test]
    fn an_existing_session_gets_a_new_window_instead_of_a_second_session() {
        let plan = plan("proj", &dir(), false, false, "ws");
        let cmds = commands_for(&plan, true);
        assert_eq!(
            cmds[0],
            vec!["new-window", "-t", "ws:", "-n", "proj", "-c", "/tmp/proj", "ws proj"]
        );
    }

    #[test]
    fn inside_tmux_we_select_the_window_and_never_attach() {
        let plan = plan("proj", &dir(), true, false, "ws");
        let cmds = commands_for(&plan, true);
        // Attaching from inside tmux is the classic nested-session error.
        assert!(!cmds.iter().any(|c| c[0] == "attach"), "must not attach from inside: {cmds:?}");
        assert_eq!(cmds.last().unwrap(), &vec!["select-window", "-t", "ws:proj"]);
    }

    #[test]
    fn the_task_variant_runs_a_drain_rather_than_an_interactive_launch() {
        let interactive = plan("proj", &dir(), false, false, "ws");
        assert_eq!(interactive.command, "ws proj");

        let drained = plan("proj", &dir(), false, true, "ws");
        assert_eq!(drained.command, "ws -queue drain proj");
        assert_ne!(drained.command, interactive.command);
    }

    #[test]
    fn the_ws_binary_path_is_used_verbatim_so_a_non_path_install_still_works() {
        let plan = plan("proj", &dir(), false, false, "/opt/bin/ws");
        assert_eq!(plan.command, "/opt/bin/ws proj");
    }

    /// I6. `-spawn --task "one thing"` runs a full `-queue drain`. When the
    /// queue holds more than the task just added, the user must be told the
    /// real blast radius before the window starts — the command's wording
    /// promises one thing and its behaviour is "everything pending".
    #[test]
    fn a_task_spawn_discloses_how_many_pending_tasks_will_actually_run() {
        // Only the task the user just typed: no surprise, no warning needed.
        let alone = drain_scope_notice(1);
        assert!(alone.contains('1'), "{alone}");
        assert!(!alone.contains("not just this one"), "nothing to warn about: {alone}");

        // A queue with older work in it: the count and the fact that it is
        // *more than the one task* must both be visible.
        let crowded = drain_scope_notice(7);
        assert!(crowded.contains('7'), "the real count is stated: {crowded}");
        assert!(
            crowded.contains("not just this one"),
            "the scope surprise is spelled out: {crowded}"
        );
        assert!(crowded.contains("unattended"), "and that nobody is watching: {crowded}");
    }

    #[test]
    fn the_scope_notice_counts_the_workspaces_real_pending_queue() {
        let td = tempfile::TempDir::new().unwrap();
        let ws = crate::workspace::Workspace {
            name: "proj".into(),
            root: td.path().to_path_buf(),
        };
        assert_eq!(pending_summary(&ws).unwrap(), 0, "an empty queue counts zero");
        crate::queue::add(&ws.queue_tasks(), "older work", "alice").unwrap();
        crate::queue::add(&ws.queue_tasks(), "the new task", "bob").unwrap();
        assert_eq!(pending_summary(&ws).unwrap(), 2);
    }
}
