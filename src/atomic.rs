//! The one way `ws` writes a shared file.
//!
//! Temp + rename, with a per-process temp name. The fixed-temp-name variant of
//! this code has been written and fixed five separate times in this codebase,
//! each time in a module modeled on a previously-fixed one: a shared `*.tmp`
//! path lets two live `ws` processes interleave their writes before either
//! renames, and the rename being atomic does not save you. Every writer goes
//! through here so the unsafe shape has nowhere left to be reintroduced.
//!
//! This handles the *write*. Refusing to overwrite a file that failed to parse
//! is the caller's job, because only the caller knows the format.
use anyhow::{Context, Result};
use std::path::Path;

pub fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    atomic_write_with_mode(path, contents, None)
}

/// Owner-only, for a directory `ws` creates and owns.
pub const PRIVATE_DIR: u32 = 0o700;

/// Owner-only, for a file `ws` creates and owns.
pub const PRIVATE_FILE: u32 = 0o600;

/// `create_dir_all`, then take the umask back out of the answer.
///
/// Every directory `ws` made was created with a bare `create_dir_all`, which
/// takes the caller's umask — so under the common `umask 022` a workspace's
/// `.ws/` tree (notebooks, handoffs, memory, machine-local state) and the
/// secrets directory were world-readable. The mode is applied to each component
/// this call is responsible for, not just the leaf, because
/// `.ws/notebook` being `0700` is worth nothing if `.ws` itself is `0755`.
pub fn create_private_dir_all(path: &Path) -> Result<()> {
    // Which components this call is responsible for is decided *before*
    // creating anything: afterwards every one of them exists and there is no
    // way to tell ours from a directory that was already there. Hardening only
    // the leaf — the first version, which the sentence above already claimed
    // otherwise — left `.ws/local/mail` at `0700` under a `0755` `local/`
    // wherever a caller created the whole chain at once.
    let mut missing = Vec::new();
    let mut cursor = Some(path);
    while let Some(c) = cursor {
        if c.exists() {
            break;
        }
        missing.push(c.to_path_buf());
        cursor = c.parent().filter(|p| !p.as_os_str().is_empty());
    }

    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create {}", path.display()))?;

    // Leaf first, so the tightest directory is private before its parent
    // becomes traversable.
    for dir in &missing {
        harden_dir(dir);
    }
    // The leaf itself is hardened whether or not this call created it: a `.ws/`
    // that arrived by clone exists already, and tightening it is the whole point
    // of calling this on open as well as on create.
    harden_dir(path);
    Ok(())
}

/// Make an existing directory owner-only, best effort.
///
/// Called on open as well as on create, and deliberately silent when it cannot:
/// git records no directory modes, so a `.ws/` arriving by clone is created
/// under whatever umask the cloning machine had and needs tightening on a
/// machine that may not own it. A directory somebody else owns is not ours to
/// fix and not a reason to refuse to launch.
pub fn harden_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(path) {
            if md.is_dir() && md.permissions().mode() & 0o7777 != PRIVATE_DIR {
                let mut perms = md.permissions();
                perms.set_mode(PRIVATE_DIR);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Make an existing file owner-only, best effort. Same contract as [`harden_dir`].
pub fn harden_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(path) {
            if md.is_file() && md.permissions().mode() & 0o7777 != PRIVATE_FILE {
                let mut perms = md.permissions();
                perms.set_mode(PRIVATE_FILE);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Append one line to a JSON-lines file, repairing a torn tail first.
///
/// Every `.jsonl` writer here used a bare append. A process killed mid-write
/// leaves a final line with no newline, and the next append then splices two
/// records onto one line — so one interrupted write costs *two* records: the
/// torn one, which was never complete, and the intact one written after it,
/// which a per-line reader drops along with it. `.ws/timeline.jsonl` is what
/// `ws -who` and `ws -conversations` read, and `.ws/local/tasks.jsonl` is a
/// queue, where losing a record loses work the user asked for.
///
/// The repair is a newline, written before the record rather than after it.
/// Terminating each line as it is written would leave the same window one byte
/// later; what makes this safe is that the repair and the record go out in one
/// `write_all` to an `O_APPEND` descriptor, so a concurrent writer cannot land
/// between them.
///
/// The *decision* to repair is a separate read and is not part of that atom: two
/// processes appending at the same moment can both see the unterminated tail and
/// both prepend a newline. That costs a blank line, which every reader here
/// already skips — the failure this guards against is a spliced record, and a
/// blank line is not one.
pub fn append_line(path: &Path, line: &str) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;

    let mut needs_newline = false;
    let len = f.seek(SeekFrom::End(0))?;
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        if f.read_exact(&mut last).is_ok() {
            needs_newline = last[0] != b'\n';
        }
    }

    let mut out = String::with_capacity(line.len() + 2);
    if needs_newline {
        out.push('\n');
    }
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
    f.write_all(out.as_bytes()).with_context(|| format!("cannot append to {}", path.display()))?;
    Ok(())
}

/// `atomic_write`, but with the temp file created at an explicit unix mode
/// *before* any bytes are written to it.
///
/// The mode has to be applied at creation, not after the rename. A writer that
/// renames into place and then chmods leaves the file world-readable at its
/// real path for the width of that window, and — worse — can return `Err` from
/// a `set_permissions` that failed *after* the write already succeeded, so the
/// user is told the operation failed while a loosely-permissioned file sits on
/// disk. Creating the temp file restricted means there is no window and no
/// post-write chmod to fail.
///
/// `None` (what `atomic_write` passes) leaves permissions entirely alone: the
/// file is created under the process umask exactly as `fs::write` would, so
/// every existing caller is byte-for-byte and bit-for-bit unaffected.
pub fn atomic_write_with_mode(
    path: &Path,
    contents: impl AsRef<[u8]>,
    mode: Option<u32>,
) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => format!("{e}.tmp.{}", std::process::id()),
        None => format!("tmp.{}", std::process::id()),
    };
    let tmp = path.with_extension(ext);

    if let Err(e) = write_tmp(&tmp, contents.as_ref(), mode) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("failed to write {}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("failed to rename into {}", path.display()));
    }
    // Durability needs *both* fsyncs, and they cover different things.
    //
    // The fsync above (inside `write_tmp`) forces the temp file's *contents*
    // to stable storage before the rename. Without it a crash can leave the
    // rename durable while the data behind it is not — the classic
    // rename-before-data reorder, whose result is a correctly-named file full
    // of zeroes. For `registry.toml` that is an annoyance; for a credential
    // store it is unrecoverable.
    //
    // This fsync forces the *directory entry* — the rename itself. A file's
    // own fsync says nothing about whether the link naming it survived, so
    // without this the durable data can still be reachable only under the
    // temp name (or not at all) after a crash.
    //
    // Neither is redundant, and directory fsync is best-effort by design:
    // some filesystems and platforms refuse to open a directory for sync, and
    // failing a write that already landed would be worse than the weaker
    // durability guarantee.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// An existing file's unix mode, in the shape `atomic_write_with_mode` wants.
///
/// For rewriting a file *the user owns* rather than one this tool created:
/// passing `None` recreates it under the process umask, so a rewrite silently
/// loosens a `0600` `.env` to `0644`. That is exactly the file the redaction
/// path rewrites, and exactly the mode that matters on it. `None` here means
/// "no existing file to copy from" (or a platform without modes), which is the
/// only case where the umask is the right answer.
pub fn mode_of(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn write_tmp(tmp: &Path, contents: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(m);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut f = opts.open(tmp)?;
    // `OpenOptions::mode` applies only when the file is *created*. A leftover
    // temp file from a crashed run of this same pid would keep its old, looser
    // mode, so restrict the handle explicitly too. This chmod is still safe in
    // the way the post-rename one was not: it happens before the rename and
    // before any bytes are written, so if it fails we abort and clean up
    // rather than leaving a loose file at the real path.
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(m))?;
    }
    f.write_all(contents)?;
    // See the comment at the call site: contents before the rename.
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The defect: an interrupted write leaves a final line with no newline, a
    /// bare append splices the next record onto it, and a per-line reader drops
    /// the spliced line whole — so one bad write costs two records.
    #[test]
    fn appending_after_a_torn_write_does_not_splice_two_records() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("timeline.jsonl");
        std::fs::write(&p, "{\"kind\":\"first\"}\n{\"kind\":\"torn\"").unwrap();

        append_line(&p, "{\"kind\":\"next\"}").unwrap();

        let body = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "the repair adds a line, it does not merge one: {body:?}");
        assert_eq!(lines[2], "{\"kind\":\"next\"}", "the new record must stand alone");
        // The torn record stays torn — it was never complete — but the damage
        // stops there.
        assert_eq!(lines[1], "{\"kind\":\"torn\"");
    }

    #[test]
    fn appending_to_a_well_formed_file_adds_exactly_one_line() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("timeline.jsonl");
        append_line(&p, "{\"a\":1}").unwrap();
        append_line(&p, "{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":1}\n{\"a\":2}\n");
    }

    /// `.ws/notebook` at `0700` under a `0755` `.ws` protects nothing, and a
    /// caller that creates the whole chain in one call is the ordinary case —
    /// `.ws/local/mail/new` is created that way. Hardening only the leaf was the
    /// first version, and the doc comment claimed otherwise the whole time.
    #[test]
    #[cfg(unix)]
    fn every_directory_this_call_creates_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let leaf = d.path().join("outer/middle/inner");
        create_private_dir_all(&leaf).unwrap();

        for p in [&leaf, &d.path().join("outer/middle"), &d.path().join("outer")] {
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}", p.display());
        }
    }

    /// The other half of the rule: a directory that was already there is not
    /// this call's to re-permission, except the leaf, which is exactly what
    /// `create_private_dir_all` is called on open to tighten.
    #[test]
    #[cfg(unix)]
    fn an_existing_parent_is_left_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let parent = d.path().join("theirs");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();

        create_private_dir_all(&parent.join("ours")).unwrap();

        assert_eq!(std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(
            std::fs::metadata(parent.join("ours")).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn writes_and_replaces_atomically() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("f.toml");
        atomic_write(&p, "one").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one");
        atomic_write(&p, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
    }

    #[test]
    fn the_temp_name_is_per_process_and_leaves_nothing_behind() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("f.toml");
        atomic_write(&p, "x").unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files must not survive a successful write: {leftovers:?}"
        );

        // A fixed temp name is the bug this helper exists to prevent: two live
        // processes sharing one temp path interleave their writes.
        let fixed = p.with_extension("toml.tmp");
        std::fs::write(&fixed, "someone else's half-written file").unwrap();
        atomic_write(&p, "y").unwrap();
        assert_eq!(
            std::fs::read_to_string(&fixed).unwrap(),
            "someone else's half-written file",
            "atomic_write must not touch the fixed temp path"
        );
    }

    #[test]
    fn creates_the_parent_directory() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("nested/deeper/f.json");
        atomic_write(&p, "{}").unwrap();
        assert!(p.is_file());
    }

    #[test]
    fn a_failed_write_leaves_the_original_intact_and_no_temp() {
        let d = TempDir::new().unwrap();
        let target = d.path().join("dir-not-file");
        std::fs::create_dir(&target).unwrap();
        // Renaming a file over an existing directory fails on every platform we target.
        assert!(atomic_write(&target, "x").is_err());
        assert!(target.is_dir(), "the original is untouched");
        let leftovers: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "the temp file is cleaned up on failure: {leftovers:?}");
    }

    /// I4: the mode must be on the file from the moment it has bytes, so
    /// there is never an instant where a credential file is readable by
    /// anyone else at its real path.
    #[test]
    #[cfg(unix)]
    fn a_mode_is_applied_before_the_bytes_are_written() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let p = d.path().join("creds.enc");
        atomic_write_with_mode(&p, "ciphertext", Some(0o600)).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);

        // And a rewrite over an existing file does not loosen it back to the
        // umask default: the temp file carries the mode, so the renamed-in
        // replacement does too.
        atomic_write_with_mode(&p, "more ciphertext", Some(0o600)).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "more ciphertext");
    }

    /// The default-permission behaviour of every *existing* caller must be
    /// unchanged by I4: `atomic_write` passes `None`, which means the file is
    /// created under the umask exactly as `fs::write` would have made it.
    #[test]
    #[cfg(unix)]
    fn atomic_write_leaves_permissions_exactly_as_fs_write_would() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();

        let reference = d.path().join("by-fs-write.toml");
        std::fs::write(&reference, "x").unwrap();
        let expected = std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777;

        let p = d.path().join("by-atomic-write.toml");
        atomic_write(&p, "x").unwrap();
        let actual = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;

        assert_eq!(
            actual, expected,
            "atomic_write must not have acquired an opinion about permissions"
        );
    }
}
