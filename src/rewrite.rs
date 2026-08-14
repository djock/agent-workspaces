//! Prompt rewriting: type a rough prompt, press `ctrl+g`, get a precise one.
//!
//! **How this is possible at all.** No hook can replace prompt text: a
//! `UserPromptSubmit` hook can only append context, and a blocking one kills the
//! turn before the model is called. The route is Claude Code's own
//! `chat:externalEditor` (`ctrl+g`, verified present in 2.1.232): it writes the
//! composer buffer to a temp file, runs `$EDITOR` on it, and replaces the
//! composer with whatever comes back. Point `$EDITOR` at a shim and a supported
//! round-trip does the substitution — nothing is submitted on the user's behalf,
//! they review and send what comes back.
//!
//! **The shim must not break your editor.** It runs for *every* `$EDITOR`
//! invocation inside the session, including `/memory` opening a real file. The
//! rule is containment in the OS temp directory: a composer buffer is a temp
//! file, and `/memory` and friends edit real paths. Anything outside temp is
//! handed to the editor the user actually configured, unchanged. This is a
//! property ws can check, unlike a filename pattern, which would be a guess
//! about a private implementation detail.
//!
//! **Every failure leaves the text exactly as typed.** A rewrite that errors,
//! times out, produces nothing, or is not configured returns the buffer
//! untouched: the worst outcome of this feature must be that it did nothing.
use anyhow::Result;
use std::path::{Path, PathBuf};

/// How long the rewriter gets before the buffer is returned untouched.
const TIMEOUT_SECS: u64 = 30;

/// What to do with a file handed to the shim.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Rewrite this composer buffer in place.
    Rewrite,
    /// Not a composer buffer: run the user's own editor on it.
    Delegate,
    /// A composer buffer that must not be touched, and why.
    PassThrough(&'static str),
}

/// Decide what the shim does with `path`, given its `content`.
///
/// Kept separate from doing it so the whole decision is testable without an
/// editor, a model, or a terminal.
pub fn decide(path: &Path, content: &str, temp_dir: &Path) -> Action {
    // A real file the user asked to edit — `/memory`, a plugin file — is never
    // a composer buffer, whatever is in it.
    if !inside(path, temp_dir) {
        return Action::Delegate;
    }
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Action::PassThrough("nothing to rewrite");
    }
    // A slash command, a bang command, or a memory line is not a prompt to a
    // model: rewriting `/clear` into a polite request to clear the screen would
    // be actively wrong.
    if let Some(first) = trimmed.chars().next() {
        if matches!(first, '/' | '!' | '#') {
            return Action::PassThrough("a command, not a prompt");
        }
    }
    // The buffer holds *placeholders* for pastes and images, not their bodies.
    // Rewriting one destroys the attachment it stands for.
    if content.contains("[Pasted text") || content.contains("[Image") {
        return Action::PassThrough("the buffer holds a paste or image placeholder");
    }
    Action::Rewrite
}

/// Is `path` really inside `dir`?
///
/// Both sides are resolved first. `Path::starts_with` is purely lexical, so
/// `<temp>/../notes.md` "starts with" `<temp>` while living somewhere else
/// entirely — found by driving the shim with exactly that path, which was
/// rewritten as a composer buffer instead of being handed to the user's editor.
/// The same lesson as resolving both sides of a containment test anywhere else:
/// a symlinked or dotted path is the ordinary case, not the hostile one.
///
/// A path that cannot be resolved (it does not exist yet) falls back to the
/// lexical test on a normalized copy, which is stricter than nothing.
fn inside(path: &Path, dir: &Path) -> bool {
    // The *parent* is what gets resolved, not the file: a composer buffer may
    // not exist yet, and `canonicalize` on a missing path fails — which on macOS
    // would compare a `/var/...` fallback against a canonical `/private/var/...`
    // and answer "outside" for a file plainly inside.
    let resolved = |p: &Path| -> PathBuf {
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(name)) => match parent.canonicalize() {
                Ok(c) => c.join(name),
                Err(_) => normalize(p),
            },
            _ => p.canonicalize().unwrap_or_else(|_| normalize(p)),
        }
    };
    resolved(path).starts_with(resolved(dir))
}

/// Drop `.` and resolve `..` lexically, for a path that does not exist.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The instruction the rewriter is given, ahead of the user's text.
///
/// Deliberately narrow: this turns a rough request into a precise one, and must
/// never answer it. A rewriter that starts doing the work returns an essay where
/// the user expected their own sentence back, sharpened.
pub const INSTRUCTION: &str = "\
Rewrite the following into a clear, specific engineering request. Keep the \
author's intent and voice. Do not answer it, do not add requirements they did \
not state, and do not add preamble. Output only the rewritten request.";

/// How the buffer gets rewritten.
enum Rewriter {
    /// `$WS_REWRITE_CMD`: stdin to stdout, the oldest and widest contract.
    Command(String),
    /// Claude Code in one-shot mode.
    Claude(PathBuf),
}

fn rewriter() -> Option<Rewriter> {
    if let Ok(c) = std::env::var("WS_REWRITE_CMD") {
        if !c.trim().is_empty() {
            return Some(Rewriter::Command(c));
        }
    }
    // The agent's own CLI, which authenticates through the user's existing
    // login and spends no API credit.
    let bin = std::env::var("WS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
    which(&bin).map(Rewriter::Claude)
}

fn which(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(bin)).find(|c| c.is_file()))
}

/// Rewrite `prompt`, or return `None` to leave it alone.
///
/// Runs hermetically: a neutral working directory and the session's own context
/// variables stripped. Without that, a nested agent inherits the project's
/// `CLAUDE.md`, its memory and its conventions — cs measured a request to add a
/// flag coming back demanding TDD, bash 3.2 compatibility and a README update
/// nobody asked for.
pub fn rewrite(prompt: &str) -> Option<String> {
    let r = rewriter()?;
    let mut cmd = match &r {
        Rewriter::Command(c) => {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg(c);
            cmd
        }
        Rewriter::Claude(bin) => {
            let mut cmd = std::process::Command::new(bin);
            cmd.arg("-p").arg(format!("{INSTRUCTION}\n\n---\n{prompt}"));
            cmd
        }
    };
    // Neutral cwd: an agentic CLI reads instructions from the directory it runs
    // in, and this must not become a second opinion about the project.
    cmd.current_dir(std::env::temp_dir());
    for leaked in ["WS_WORKSPACE", "WS_DIR", "WS_AGENT", "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"] {
        cmd.env_remove(leaked);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let mut child = cmd.spawn().ok()?;
    if let Rewriter::Command(_) = r {
        use std::io::Write;
        child.stdin.take()?.write_all(prompt.as_bytes()).ok()?;
    } else {
        // Closed rather than left open: Claude Code hands the shim the real tty,
        // and an agentic CLI that decides it is interactive paints over the
        // screen the user is waiting on.
        drop(child.stdin.take());
    }

    let out = wait_with_timeout(child, std::time::Duration::from_secs(TIMEOUT_SECS))?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Judge the output, not only the status. A CLI that prints an error and
    // exits 0 would otherwise become the user's next message.
    if text.is_empty() || text.len() > 100 * prompt.len().max(200) {
        return None;
    }
    Some(text)
}

/// `wait_with_output` with a deadline, since a hung rewriter must not hold the
/// composer open forever. Polled rather than threaded: the child is short-lived
/// and this keeps the failure path a plain kill.
fn wait_with_timeout(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> Option<std::process::Output> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() > limit {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// The `$EDITOR` shim: `ws internal rewrite <file>`.
pub fn run(path: &str) -> Result<()> {
    let path = PathBuf::from(path);
    let content = std::fs::read_to_string(&path).unwrap_or_default();

    match decide(&path, &content, &std::env::temp_dir()) {
        Action::Delegate => {
            // The editor the user actually configured, captured at launch before
            // ws took the variable over. With none recorded there is nothing to
            // delegate to, and refusing is better than opening something they
            // did not choose.
            let editor = std::env::var("WS_REAL_EDITOR").unwrap_or_default();
            if editor.trim().is_empty() {
                anyhow::bail!(
                    "no editor configured: set $EDITOR before launching, \
                     or edit {} directly",
                    path.display()
                );
            }
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{editor} \"$1\"", editor = editor))
                .arg("sh")
                .arg(&path)
                .status()?;
            std::process::exit(status.code().unwrap_or(0));
        }
        Action::PassThrough(_) => Ok(()),
        Action::Rewrite => {
            // Written back through the same path Claude Code will read. A
            // `None` leaves the file exactly as typed — the contract here is
            // that the worst this feature does is nothing.
            if let Some(better) = rewrite(&content) {
                std::fs::write(&path, format!("{}\n", better.trim_end()))?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp() -> TempDir {
        TempDir::new().unwrap()
    }

    /// The rule that keeps `/memory` working: a real file the user asked to edit
    /// goes to their own editor, whatever is in it.
    #[test]
    fn a_file_outside_the_temp_directory_goes_to_the_users_editor() {
        let d = temp();
        let real = d.path().join("CLAUDE.md");
        assert_eq!(
            decide(&real, "# project notes", Path::new("/var/folders/nowhere")),
            Action::Delegate
        );
    }

    /// Found by driving the shim with a real path: `Path::starts_with` is
    /// lexical, so `<temp>/../notes.md` passed the containment test and a file
    /// outside temp entirely was rewritten as though it were a composer buffer.
    #[test]
    fn a_dotted_path_cannot_pretend_to_be_inside_the_temp_directory() {
        let d = temp();
        let tmp = d.path().join("tmp");
        let elsewhere = d.path().join("elsewhere");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let real = elsewhere.join("notes.md");
        std::fs::write(&real, "user notes").unwrap();

        let escaped = tmp.join("..").join("elsewhere").join("notes.md");
        assert_eq!(
            decide(&escaped, "user notes", &tmp),
            Action::Delegate,
            "a path that merely starts with the temp directory textually is not inside it"
        );
    }

    #[test]
    fn a_composer_buffer_is_rewritten() {
        let d = temp();
        let buf = d.path().join("buffer.md");
        assert_eq!(decide(&buf, "make the parser faster", d.path()), Action::Rewrite);
    }

    /// Rewriting `/clear` into a polite request to clear the screen would be
    /// actively wrong, and an empty buffer has nothing to rewrite.
    #[test]
    fn commands_and_empty_buffers_pass_through_untouched() {
        let d = temp();
        let buf = d.path().join("buffer.md");
        for content in ["", "   \n", "/clear", "!ls -la", "#remember this"] {
            assert!(
                matches!(decide(&buf, content, d.path()), Action::PassThrough(_)),
                "{content:?} must pass through"
            );
        }
    }

    /// The buffer holds placeholders, not the pasted bodies — rewriting one
    /// destroys the attachment it stands for.
    #[test]
    fn a_buffer_holding_an_attachment_placeholder_is_never_rewritten() {
        let d = temp();
        let buf = d.path().join("buffer.md");
        for content in ["look at this [Pasted text #1 +40 lines]", "what is [Image #2]?"] {
            assert!(
                matches!(decide(&buf, content, d.path()), Action::PassThrough(_)),
                "{content:?} must pass through"
            );
        }
    }

    /// The contract: every failure path leaves the text exactly as typed.
    #[test]
    fn a_failing_rewriter_leaves_the_buffer_alone() {
        let d = temp();
        let buf = d.path().join("buffer.md");
        std::fs::write(&buf, "the original text\n").unwrap();

        // A rewriter that exits non-zero, and one that prints nothing.
        for cmd in ["exit 3", "true"] {
            std::env::set_var("WS_REWRITE_CMD", cmd);
            std::env::set_var("TMPDIR", d.path());
            let before = std::fs::read_to_string(&buf).unwrap();
            let _ = run(buf.to_str().unwrap());
            assert_eq!(
                std::fs::read_to_string(&buf).unwrap(),
                before,
                "a failing rewriter ({cmd}) must not touch the buffer"
            );
        }
        std::env::remove_var("WS_REWRITE_CMD");
        std::env::remove_var("TMPDIR");
    }

    #[test]
    fn a_working_rewriter_replaces_the_buffer() {
        let d = temp();
        let buf = d.path().join("buffer.md");
        std::fs::write(&buf, "make it faster\n").unwrap();
        std::env::set_var("WS_REWRITE_CMD", "sed 's/faster/measurably faster/'");
        std::env::set_var("TMPDIR", d.path());

        run(buf.to_str().unwrap()).unwrap();

        assert_eq!(std::fs::read_to_string(&buf).unwrap(), "make it measurably faster\n");
        std::env::remove_var("WS_REWRITE_CMD");
        std::env::remove_var("TMPDIR");
    }
}
