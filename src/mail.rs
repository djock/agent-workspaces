use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::atomic::atomic_write;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Sortable id: "<epoch-millis>-<uuid>". Sorting ids sorts by send time,
    /// which is what the unread marker relies on.
    pub id: String,
    pub from: String,
    pub ts: String,
    pub body: String,
}

fn new_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Zero-padded so lexicographic order matches numeric order past the year 5138.
    format!("{millis:015}-{}", uuid::Uuid::new_v4())
}

/// Write one message into `mail_dir`. Returns its id.
pub fn send(mail_dir: &Path, from: &str, body: &str) -> Result<String> {
    std::fs::create_dir_all(mail_dir)
        .with_context(|| format!("cannot create {}", mail_dir.display()))?;
    let msg = Message {
        id: new_id(),
        from: from.to_string(),
        ts: crate::now_iso(),
        body: body.to_string(),
    };
    let path = mail_dir.join(format!("{}.json", msg.id));
    atomic_write(&path, serde_json::to_vec_pretty(&msg)?)?;
    Ok(msg.id)
}

/// All messages, ascending by id. A missing mailbox is empty; a corrupt message
/// is an error — the two must never collapse into each other.
pub fn all(mail_dir: &Path) -> Result<Vec<Message>> {
    let rd = match std::fs::read_dir(mail_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", mail_dir.display())),
    };
    let mut msgs = Vec::new();
    for entry in rd {
        let entry = entry.with_context(|| format!("cannot read {}", mail_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let msg: Message = serde_json::from_str(&raw)
            .with_context(|| format!("corrupt message {}", path.display()))?;
        msgs.push(msg);
    }
    msgs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(msgs)
}

/// Messages sent after the marked one. A missing marker means nothing has been
/// read yet; a marker that exists but cannot be read is an error.
pub fn unread(mail_dir: &Path, seen_marker: &Path) -> Result<Vec<Message>> {
    let seen = match std::fs::read_to_string(seen_marker) {
        Ok(s) => Some(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {}", seen_marker.display()))
        }
    };
    let msgs = all(mail_dir)?;
    Ok(match seen {
        None => msgs,
        Some(upto) => msgs.into_iter().filter(|m| m.id > upto).collect(),
    })
}

pub fn mark_seen(seen_marker: &Path, upto_id: &str) -> Result<()> {
    atomic_write(seen_marker, upto_id.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn send_then_all_round_trips_in_order() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("mail");
        let a = send(&dir, "alice", "first").unwrap();
        let b = send(&dir, "bob", "second").unwrap();
        assert_ne!(a, b, "each message gets its own id");

        let msgs = all(&dir).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body, "first");
        assert_eq!(msgs[0].from, "alice");
        assert_eq!(msgs[1].body, "second");
        assert!(msgs[0].id < msgs[1].id, "ids sort in send order");
    }

    #[test]
    fn all_on_a_missing_dir_is_empty_but_all_on_a_corrupt_file_is_an_error() {
        let td = TempDir::new().unwrap();
        // Never-written mailbox: genuinely empty.
        assert!(all(&td.path().join("nope")).unwrap().is_empty());

        // Corrupt message: must not be silently dropped, because "you have no
        // mail" and "one of your messages is unreadable" are different answers.
        let dir = td.path().join("mail");
        send(&dir, "alice", "first").unwrap();
        std::fs::write(dir.join("99999-garbage.json"), "{not json").unwrap();
        assert!(all(&dir).is_err());
    }

    #[test]
    fn unread_is_everything_after_the_marker() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("mail");
        let marker = td.path().join("mail-seen");

        let a = send(&dir, "alice", "first").unwrap();
        send(&dir, "bob", "second").unwrap();
        assert_eq!(unread(&dir, &marker).unwrap().len(), 2, "no marker: all unread");

        mark_seen(&marker, &a).unwrap();
        let u = unread(&dir, &marker).unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].body, "second", "the marked message is excluded, the later one is not");

        let c = send(&dir, "carol", "third").unwrap();
        assert_eq!(unread(&dir, &marker).unwrap().len(), 2, "new mail after the marker is unread again");
        mark_seen(&marker, &c).unwrap();
        assert!(unread(&dir, &marker).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_marker_means_error_not_all_unread() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("mail");
        send(&dir, "alice", "first").unwrap();
        let marker = td.path().join("marker-dir");
        std::fs::create_dir_all(&marker).unwrap(); // a directory: read fails
        assert!(unread(&dir, &marker).is_err());
    }
}
