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
