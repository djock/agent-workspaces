//! Messages between workspaces.
//!
//! Two agents working on related things — a service and its client, a refactor
//! and the tests for it — have no way to tell each other anything. `ws -msg`
//! gives them one: a line delivered into another workspace's mailbox, surfaced
//! to that workspace's agent on its next prompt.
//!
//! **A maildir, not a log file.** Every message is its own file, staged in
//! `tmp/` and renamed into `new/`. This is the whole design, and it is the shape
//! cs arrived at only after shipping the other one: a shared append-only file
//! looks atomic and is not, because a large body is flushed in chunks and two
//! simultaneous senders splice each other's lines. Measured there, four
//! concurrent senders left 112 of 200 lines intact — and a torn line was
//! silently dropped by the tolerant reader. A rename cannot interleave, so
//! delivery here is atomic by construction rather than by hoping the write is
//! small enough.
//!
//! Unread is `new/*.json` — one definition, used by the reader, the prompt hook
//! and any other surface, so they cannot disagree about whether you have mail.
//!
//! Mail is **machine-local** (`.ws/local/mail/`, which is gitignored). A message
//! is addressed to a running agent on this machine, not to whoever clones the
//! repository next month.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The biggest body that may be sent, in bytes.
///
/// Bodies are rendered into another agent's prompt, so this bounds what a sender
/// can inject into a reader's context rather than what a file can hold. Large
/// enough for a handoff, small enough that five of them do not crowd out the
/// conversation they arrive in.
pub const MAX_BODY_BYTES: usize = 65536;

/// How many unread messages the prompt digest will render in full.
pub const DIGEST_CAP: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Something to read.
    Text,
    /// Something to do — queued as a task in the recipient, as well as delivered.
    Task,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind> {
        match s {
            "text" => Ok(Kind::Text),
            "task" => Ok(Kind::Task),
            other => anyhow::bail!("unknown message kind: {other} (want text|task)"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub ts: String,
    /// The workspace that sent it, empty when sent from a plain terminal.
    #[serde(default)]
    pub from: String,
    /// Who sent it, as an actor slug. Always present, so a message sent from a
    /// terminal outside any workspace still says who wrote it.
    pub actor: String,
    pub to: String,
    pub kind: Kind,
    pub body: String,
    /// The exchange this belongs to: a reply carries its parent's thread, a new
    /// message starts one with its own id.
    pub thread: String,
}

fn mail_dir(root: &Path) -> PathBuf {
    root.join(".ws").join("local").join("mail")
}

/// Unread. A message stays here until it is read.
fn new_dir(root: &Path) -> PathBuf {
    mail_dir(root).join("new")
}

/// Read. Kept rather than deleted, so `-msg log` can show the exchange.
fn cur_dir(root: &Path) -> PathBuf {
    mail_dir(root).join("cur")
}

/// Being written. Never scanned by a reader, which is what makes delivery atomic.
fn tmp_dir(root: &Path) -> PathBuf {
    mail_dir(root).join("tmp")
}

/// Deliver `msg` into the workspace rooted at `to_root`.
///
/// Written into `tmp/` and renamed into `new/`: a reader scanning `new/` sees
/// either nothing or the whole message, never a half-written one. The rename is
/// within one filesystem by construction, both paths being under the same
/// mailbox.
pub fn deliver(to_root: &Path, msg: &Message) -> Result<()> {
    if msg.body.len() > MAX_BODY_BYTES {
        anyhow::bail!(
            "message body is {} bytes, over the {MAX_BODY_BYTES}-byte limit — \
             send a path to the detail instead of the detail",
            msg.body.len()
        );
    }
    for d in [tmp_dir(to_root), new_dir(to_root), cur_dir(to_root)] {
        crate::atomic::create_private_dir_all(&d)?;
    }
    let name = format!("{}.json", msg.id);
    let tmp = tmp_dir(to_root).join(&name);
    let body = serde_json::to_string(msg)?;
    // Owner-only, like everything else ws writes. The mailbox directories are
    // already `0700`, so this is defence in depth — the same argument that put
    // the keyring index at `0600` next to a `0600` store: when the two disagree,
    // the weaker one is the real floor, and directory modes are the half that
    // does not survive a copy, a restore, or a `chmod -R` somebody meant for
    // something else.
    crate::atomic::atomic_write_with_mode(&tmp, &body, Some(crate::atomic::PRIVATE_FILE))
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, new_dir(to_root).join(&name))
        .with_context(|| format!("cannot deliver to {}", new_dir(to_root).display()))?;
    Ok(())
}

fn read_dir_messages(dir: &Path) -> Vec<Message> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Message> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            // A message that will not parse is skipped rather than fatal: one
            // bad file must not hide the mailbox. It stays on disk, where it can
            // be looked at, instead of being deleted to make the error go away.
            let raw = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str::<Message>(&raw).ok()
        })
        .collect();
    // By timestamp, then id, so two messages inside the same second still have a
    // stable order — ids are random, but the same random order every time.
    out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Messages that have not been read yet.
pub fn unread(root: &Path) -> Vec<Message> {
    read_dir_messages(&new_dir(root))
}

/// How many messages `unread` would return.
///
/// Counted *through* `unread`, not by counting files. Counting files was
/// cheaper and wrong: `read_dir_messages` deliberately skips a message it
/// cannot parse, so one unparseable file made the status line say "1 unread"
/// with nothing behind it — and since `mark_read` moved only what it could
/// parse, the file stayed in `new/` and the badge stayed lit forever. A mailbox
/// holds a handful of small files and `new/` is drained on every read, so the
/// parse is not worth a second definition of "unread".
pub fn unread_count(root: &Path) -> usize {
    unread(root).len()
}

/// Files in `new/` that no reader can turn into a message.
///
/// They are not silently deleted and not left to accumulate either: `mark_read`
/// moves them to `cur/` with everything else, which takes them out of the
/// unread set while leaving them on disk to be looked at.
fn unreadable(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|raw| serde_json::from_str::<Message>(&raw).ok())
                .is_none()
        })
        .collect()
}

/// Everything this workspace has received, read or not, oldest first.
pub fn history(root: &Path) -> Vec<Message> {
    let mut all = read_dir_messages(&cur_dir(root));
    all.extend(read_dir_messages(&new_dir(root)));
    all.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
    all
}

/// Mark everything currently unread as read, by moving it to `cur/`.
///
/// Moving rather than deleting: the exchange is still readable with `-msg log`,
/// and a reply needs its parent's thread id. Returns what was moved.
///
/// A file that will not parse is moved too, and counted in the second return
/// value. Leaving it behind is what kept `new/` from ever emptying — every
/// read re-scanned a file no reader could use, and (when the count still came
/// from counting files) the badge never went out.
pub fn mark_read(root: &Path) -> (Vec<Message>, usize) {
    let msgs = unread(root);
    let _ = crate::atomic::create_private_dir_all(&cur_dir(root));
    for m in &msgs {
        let name = format!("{}.json", m.id);
        let _ = std::fs::rename(new_dir(root).join(&name), cur_dir(root).join(&name));
    }
    let mut set_aside = 0;
    for p in unreadable(&new_dir(root)) {
        let Some(name) = p.file_name() else { continue };
        if std::fs::rename(&p, cur_dir(root).join(name)).is_ok() {
            set_aside += 1;
        }
    }
    (msgs, set_aside)
}

/// Compose a message. `thread` is the parent's thread for a reply, `None` to
/// start a new exchange.
pub fn compose(
    from: &str,
    actor: &str,
    to: &str,
    kind: Kind,
    body: &str,
    thread: Option<String>,
) -> Message {
    let id = uuid::Uuid::new_v4().to_string();
    Message {
        thread: thread.unwrap_or_else(|| id.clone()),
        id,
        ts: crate::now_iso(),
        from: from.to_string(),
        actor: actor.to_string(),
        to: to.to_string(),
        kind,
        body: body.to_string(),
    }
}

/// One rendered line per message, for a terminal.
///
/// Sanitized: a message is text another workspace's agent wrote, and it reaches
/// this terminal without anyone reviewing it. See `term::display_safe`.
pub fn render(m: &Message) -> String {
    let who = if m.from.is_empty() { m.actor.clone() } else { format!("{} ({})", m.from, m.actor) };
    format!(
        "{} from {}{}\n  {}",
        m.ts,
        crate::term::display_safe(&who),
        if m.kind == Kind::Task { " [task]" } else { "" },
        crate::term::display_safe(&m.body)
    )
}

/// What to put in front of the agent when it has unread mail.
///
/// Rendered from the unread set, capped at [`DIGEST_CAP`] messages with the
/// remainder counted. The cap is on *rendered messages*, not on files scanned:
/// counting files instead let one crafted document inline unbounded text into
/// every prompt.
pub fn digest(root: &Path) -> Option<String> {
    let msgs = unread(root);
    if msgs.is_empty() {
        return None;
    }
    let mut out = format!("You have {} unread message(s) from other workspaces:\n", msgs.len());
    for m in msgs.iter().take(DIGEST_CAP) {
        out.push_str(&render(m));
        out.push('\n');
    }
    if msgs.len() > DIGEST_CAP {
        out.push_str(&format!("… and {} more\n", msgs.len() - DIGEST_CAP));
    }
    out.push_str("Read and clear them with `ws -msg`; reply with `ws -msg <workspace> \"…\"`.");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(d: &TempDir, name: &str) -> PathBuf {
        let root = d.path().join(name);
        std::fs::create_dir_all(root.join(".ws/local")).unwrap();
        root
    }

    /// A message is text one workspace wrote for another to read, and it sits in
    /// a directory whose mode is the half that does not survive a copy or a
    /// restore. The file carries its own.
    #[test]
    #[cfg(unix)]
    fn a_delivered_message_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        let m = send(&root, "private");

        let p = new_dir(&root).join(format!("{}.json", m.id));
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    fn send(to: &Path, body: &str) -> Message {
        let m = compose("sender", "alice", "target", Kind::Text, body, None);
        deliver(to, &m).unwrap();
        m
    }

    #[test]
    fn a_delivered_message_is_unread_until_it_is_read() {
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        send(&root, "the parser is ready");

        assert_eq!(unread_count(&root), 1);
        assert_eq!(unread(&root)[0].body, "the parser is ready");

        let (read, _) = mark_read(&root);
        assert_eq!(read.len(), 1);
        assert_eq!(unread_count(&root), 0, "reading clears it");
        assert_eq!(history(&root).len(), 1, "but the exchange is still there");
    }

    /// The count and the list are one definition. Two definitions is how a
    /// status line says "2 unread" while the digest shows none.
    ///
    /// The well-formed half of this passed while the count was a file count —
    /// which is the whole lesson: an invariant asserted only over the inputs
    /// someone thought of is not the invariant. The unparseable case below is
    /// the one that mattered, and `read_dir_messages` skipping such a file is
    /// deliberate two functions up, so it was never hypothetical.
    #[test]
    fn the_count_and_the_list_always_agree() {
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        for i in 0..3 {
            send(&root, &format!("message {i}"));
        }
        assert_eq!(unread_count(&root), unread(&root).len());
        mark_read(&root);
        assert_eq!(unread_count(&root), unread(&root).len());
    }

    /// A message no reader can parse used to be counted but never listed, and
    /// `mark_read` left it where it was — so the badge said "1 unread" forever
    /// while `ws -msg` said there was none.
    #[test]
    fn a_message_that_will_not_parse_is_not_counted_and_does_not_stay_unread() {
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        send(&root, "a real message");
        std::fs::write(new_dir(&root).join("broken.json"), "{\"id\":\"broken\",\"ts\":").unwrap();

        assert_eq!(unread_count(&root), 1, "the badge counts what a reader can show");
        assert_eq!(unread_count(&root), unread(&root).len());

        let (msgs, set_aside) = mark_read(&root);
        assert_eq!(msgs.len(), 1);
        assert_eq!(set_aside, 1, "the unreadable one is reported, not swallowed");
        assert_eq!(unread_count(&root), 0);
        assert!(
            std::fs::read_dir(new_dir(&root)).unwrap().next().is_none(),
            "nothing may be left behind in new/ to be re-scanned forever"
        );
        assert!(
            cur_dir(&root).join("broken.json").exists(),
            "moved aside, not deleted — it is still evidence"
        );
    }

    /// The property the maildir exists for. A shared append-only file loses
    /// messages when senders overlap; every message being its own file, renamed
    /// into place, cannot.
    #[test]
    fn concurrent_senders_do_not_lose_or_splice_messages() {
        use std::sync::{Arc, Barrier};
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");

        const N: usize = 16;
        // Bodies large enough that a single write() would not be atomic, which
        // is exactly the case the old shape got wrong.
        let barrier = Arc::new(Barrier::new(N));
        let mut handles = Vec::new();
        for i in 0..N {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let body = format!("{i}:{}", "x".repeat(4096));
                let m = compose("sender", "alice", "target", Kind::Text, &body, None);
                barrier.wait();
                deliver(&root, &m).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let got = unread(&root);
        assert_eq!(got.len(), N, "every concurrent send must survive");
        let mut seen: Vec<usize> =
            got.iter().map(|m| m.body.split(':').next().unwrap().parse().unwrap()).collect();
        seen.sort();
        assert_eq!(seen, (0..N).collect::<Vec<_>>(), "and each must be its own whole message");
    }

    #[test]
    fn an_oversized_body_is_refused_rather_than_truncated() {
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        let m = compose("s", "a", "t", Kind::Text, &"x".repeat(MAX_BODY_BYTES + 1), None);
        let err = deliver(&root, &m).unwrap_err();
        assert!(err.to_string().contains("over the"), "{err}");
        assert_eq!(unread_count(&root), 0, "nothing may be delivered");
    }

    /// One unparseable file must not hide the rest of the mailbox — the failure
    /// mode cs hit when a single torn document took down every message after it.
    #[test]
    fn one_unreadable_message_does_not_hide_the_others() {
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        send(&root, "first");
        send(&root, "second");
        std::fs::write(new_dir(&root).join("garbage.json"), "{not json").unwrap();

        let got = unread(&root);
        assert_eq!(got.len(), 2, "the intact messages must still read");
        assert!(new_dir(&root).join("garbage.json").exists(), "and the bad one is not deleted");
    }

    #[test]
    fn a_reply_stays_in_its_parents_thread() {
        let d = TempDir::new().unwrap();
        let a = ws(&d, "a");
        let first = compose("a", "alice", "b", Kind::Text, "question", None);
        let reply = compose("b", "bob", "a", Kind::Text, "answer", Some(first.thread.clone()));
        deliver(&a, &reply).unwrap();

        assert_eq!(reply.thread, first.thread);
        assert_ne!(reply.id, first.id, "a reply is its own message");
    }

    #[test]
    fn a_new_message_starts_a_thread_named_after_itself() {
        let m = compose("a", "alice", "b", Kind::Text, "hello", None);
        assert_eq!(m.thread, m.id);
    }

    /// The digest is what reaches another agent's context. The cap bounds what a
    /// sender can inject there, and the count must describe what was shown.
    #[test]
    fn the_digest_caps_what_it_renders_and_counts_the_rest() {
        let d = TempDir::new().unwrap();
        let root = ws(&d, "target");
        for i in 0..(DIGEST_CAP + 3) {
            send(&root, &format!("body-{i}"));
        }
        let digest = digest(&root).unwrap();
        assert_eq!(digest.matches("body-").count(), DIGEST_CAP, "only the cap is rendered");
        assert!(digest.contains("and 3 more"), "the rest are counted: {digest}");
        assert!(digest.contains(&format!("{} unread", DIGEST_CAP + 3)));
    }

    #[test]
    fn no_mail_is_no_digest() {
        let d = TempDir::new().unwrap();
        assert_eq!(digest(&ws(&d, "target")), None);
    }

    /// A message is text another agent wrote, arriving on this terminal with
    /// nobody reviewing it in between.
    #[test]
    fn a_rendered_message_cannot_carry_a_control_sequence() {
        let m = compose("s\x1b[2J", "a", "t", Kind::Text, "body\x1b]0;pwned\x07", None);
        let r = render(&m);
        assert!(!r.contains('\u{1b}'), "{r:?}");
    }
}
