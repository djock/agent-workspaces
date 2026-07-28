use anyhow::{Context, Result};
use std::path::Path;

pub const BEGIN: &str = "<!-- ws:begin -->";
pub const END: &str = "<!-- ws:end -->";

const TEMPLATE: &str = include_str!("assets/context-template.md");

fn render(workspace_name: &str, handoff_hint: Option<&Path>) -> String {
    let body = TEMPLATE.replace("{{name}}", workspace_name);
    match handoff_hint {
        Some(p) => {
            let file = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("handoff.md");
            format!(
                "{BEGIN}\nSTART HERE: read the handoff .ws/handoffs/{file} first, then continue.\n\n{body}\n{END}\n"
            )
        }
        None => format!("{BEGIN}\n{body}\n{END}\n"),
    }
}

/// Like `regenerate`, but when `handoff_hint` is `Some`, prepends a
/// "START HERE" line pointing at the given handoff file inside the managed block.
/// Rewrite the managed block in the agent's context file, leaving everything
/// around it alone.
///
/// Transacted: the splice is a read-modify-write, and everything outside the
/// managed block is the user's own prose. Two launches racing here would each
/// read the same file and write back only their own version, dropping whatever
/// the other had spliced — and the atomic write, which only makes each write
/// all-or-nothing, cannot prevent that.
pub fn regenerate_with_handoff(
    path: &Path,
    workspace_name: &str,
    handoff_hint: Option<&Path>,
) -> Result<()> {
    crate::txn::transaction(path, || regenerate_locked(path, workspace_name, handoff_hint))
}

fn regenerate_locked(
    path: &Path,
    workspace_name: &str,
    handoff_hint: Option<&Path>,
) -> Result<()> {
    let block = render(workspace_name, handoff_hint);
    let new_contents = match std::fs::read_to_string(path) {
        Ok(existing) => splice(&existing, &block),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => block,
        Err(e) => return Err(e).context("failed to read existing context file"),
    };
    // Atomic, not `fs::write`: everything outside the managed block is the
    // user's own prose, and `fs::write` truncates before it writes. A crash, a
    // full disk or a signal in that gap destroys the prose with nothing left
    // to recover from. `atomic_write` also creates the parent directory.
    crate::atomic::atomic_write(path, new_contents)?;
    Ok(())
}

/// Replace the region between BEGIN..END (inclusive) with `block`, or append
/// `block` if no managed region exists.
fn splice(existing: &str, block: &str) -> String {
    if let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        if e >= b {
            let end_idx = e + END.len();
            let mut out = String::new();
            out.push_str(&existing[..b]);
            out.push_str(block.trim_end());
            out.push_str(&existing[end_idx..]);
            return out;
        }
    }
    // No block: append, separated by a blank line.
    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_file_with_managed_block() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("CLAUDE.local.md");
        regenerate_with_handoff(&f, "proj", None).unwrap();
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains(BEGIN));
        assert!(s.contains(END));
        assert!(s.contains("proj"));
    }

    #[test]
    fn preserves_user_content_and_replaces_block() {
        let d = TempDir::new().unwrap();
        let f = d.path().join("CLAUDE.local.md");
        std::fs::write(
            &f,
            format!("# my notes\nkeep me\n{BEGIN}\nOLD MANAGED\n{END}\ntrailing user text\n"),
        )
        .unwrap();
        regenerate_with_handoff(&f, "proj", None).unwrap();
        let s = std::fs::read_to_string(&f).unwrap();
        assert!(s.contains("keep me"));
        assert!(s.contains("trailing user text"));
        assert!(!s.contains("OLD MANAGED"));
        // exactly one managed block
        assert_eq!(s.matches(BEGIN).count(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn regenerate_refuses_when_an_existing_file_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let d = TempDir::new().unwrap();
        let f = d.path().join("CLAUDE.local.md");
        let original = "# my notes\nkeep me — this is a live project's context file\n";
        std::fs::write(&f, original).unwrap();
        let mut perms = std::fs::metadata(&f).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&f, perms).unwrap();

        // Unreadable ≠ absent: with the pre-fix code the read error was
        // mapped to "no existing content" and the file was overwritten with
        // a fresh managed block, silently destroying the original — which
        // for an in-place-migrated workspace is a live project's context file.
        let result = regenerate_with_handoff(&f, "proj", None);

        let mut perms = std::fs::metadata(&f).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&f, perms).unwrap();

        assert!(result.is_err(), "regenerate must not treat an unreadable file as absent");
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            original,
            "the original file must survive untouched"
        );
    }

    /// I3: everything outside the managed block is the user's prose, and the
    /// old `fs::write` truncated the file in place before rewriting it.
    ///
    /// The inode is the discriminator, and it is not incidental: `fs::write`
    /// truncates and refills the *same* inode, so there is a window in which
    /// the user's prose is gone from disk and the replacement is not yet
    /// there. A temp-file-plus-rename always lands on a new inode, because
    /// the prose is only ever replaced by an already-complete file. A test on
    /// content alone passes under both implementations; this one does not.
    #[test]
    #[cfg(unix)]
    fn the_rewrite_never_truncates_the_users_prose_in_place() {
        use std::os::unix::fs::MetadataExt;
        let d = TempDir::new().unwrap();
        let f = d.path().join("CLAUDE.local.md");
        let prose = "# my notes\nkeep me\n";
        std::fs::write(&f, format!("{prose}{BEGIN}\nOLD\n{END}\n")).unwrap();
        let before = std::fs::metadata(&f).unwrap().ino();

        regenerate_with_handoff(&f, "proj", None).unwrap();

        let after = std::fs::metadata(&f).unwrap().ino();
        assert_ne!(
            before, after,
            "the managed block must be spliced in by rename, not by truncating the user's file"
        );
        assert!(std::fs::read_to_string(&f).unwrap().contains("keep me"), "and the prose is there");
    }
}
