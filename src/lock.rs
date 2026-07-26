use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct LockGuard {
    path: PathBuf,
    released: bool,
}

impl LockGuard {
    /// Leave the lock file in place (do not remove on drop). Used before `exec`,
    /// where the launched agent inherits this PID and holds the lock until exit.
    pub fn keep(mut self) {
        self.released = true; // suppress Drop removal, but leave file on disk
        std::mem::forget(self);
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    // POSIX: `kill -0 <pid>` succeeds iff the process exists and is signalable.
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Read the pid recorded in a lock file. `Ok(None)` covers "file absent",
/// "file present but the pid field is missing or unparseable" (both
/// already-tolerated forms of staleness), and "a path component above the
/// lock file is not a directory" — that last one proves no lock could ever
/// have been acquired there (`acquire` needs `.ws/local/` to be a real
/// directory to write into), so it is absence, not an unknown. `Err` is
/// reserved for a read that could not be performed despite the path shape
/// being sound (permission error, other I/O error): the caller must not
/// treat that the same as a confirmed-stale lock.
fn read_pid(lock_file: &Path) -> Result<Option<u32>> {
    let s = match std::fs::read_to_string(lock_file) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
            return Ok(None)
        }
        Err(e) => return Err(e).with_context(|| format!("failed to read lock file {}", lock_file.display())),
    };
    let pid = toml::from_str::<toml::Table>(&s)
        .ok()
        .and_then(|t| t.get("pid").and_then(|v| v.as_integer()))
        .map(|n| n as u32);
    Ok(pid)
}

/// The pid currently holding `lock_file`, if the lock exists and that process
/// is still running. A stale lock (dead pid), a missing lock, and an
/// unreadable lock all read as `None` here.
///
/// **Display only.** This folds a read error into "not live" for callers that
/// only ever show the result (e.g. the TUI row's live marker) — degrading is
/// correct there, nothing gets written or deleted from it. Any caller that
/// gates a destructive action (delete, reclaim) on "is this live" must use
/// [`live_pid_checked`] instead: an unreadable lock is not proof of absence,
/// and folding it into `None` here previously let `remove_one` delete a live
/// workspace whenever its lock file happened to be unreadable.
pub fn live_pid(lock_file: &Path) -> Option<u32> {
    let pid = read_pid(lock_file).ok().flatten()?;
    pid_alive(pid).then_some(pid)
}

/// Like [`live_pid`], but surfaces a read failure instead of swallowing it.
/// Use this wherever the answer gates a destructive action: an unreadable
/// lock file must never be treated the same as "no one holds this lock".
pub fn live_pid_checked(lock_file: &Path) -> Result<Option<u32>> {
    let pid = match read_pid(lock_file)? {
        Some(p) => p,
        None => return Ok(None),
    };
    Ok(pid_alive(pid).then_some(pid))
}

pub fn acquire(lock_file: &Path, force: bool) -> Result<LockGuard> {
    if lock_file.exists() && !force {
        match read_pid(lock_file) {
            Ok(Some(pid)) if pid_alive(pid) => {
                bail!(
                    "workspace is in use by pid {pid} (another terminal). \
                     Close it or re-run with --force."
                );
            }
            Ok(_) => {} // missing pid field / dead pid → stale, fall through and reclaim
            Err(e) => {
                // Unreadable is not proof of staleness — reclaiming here could
                // steal the lock from a live holder we simply failed to read.
                // Refuse; --force still overrides.
                return Err(e).context("could not read existing lock file; refusing to reclaim it (use --force to override)");
            }
        }
    }
    if let Some(dir) = lock_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let host = hostname();
    let tty = std::env::var("TTY").unwrap_or_else(|_| "?".into());
    let body = format!(
        "pid = {}\nhost = \"{}\"\ntty = \"{}\"\nstarted = \"{}\"\n",
        std::process::id(),
        host,
        tty,
        crate::now_iso(),
    );
    std::fs::write(lock_file, body)?;
    Ok(LockGuard {
        path: lock_file.to_path_buf(),
        released: false,
    })
}

fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn live_pid_reports_only_running_holders() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        assert_eq!(live_pid(&lf), None, "no lock file → not live");

        std::fs::write(&lf, "pid = 999999\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n").unwrap();
        assert_eq!(live_pid(&lf), None, "dead pid → not live");

        let me = std::process::id();
        std::fs::write(&lf, format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();
        assert_eq!(live_pid(&lf), Some(me));
    }

    #[test]
    fn acquire_then_release_allows_reacquire() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        {
            let _g = acquire(&lf, false).unwrap();
            assert!(lf.exists());
        } // dropped → released
        assert!(!lf.exists());
        let _g2 = acquire(&lf, false).unwrap();
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        // PID 999999 is (essentially certainly) not running.
        std::fs::write(&lf, "pid = 999999\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n").unwrap();
        let _g = acquire(&lf, false).expect("stale lock should be reclaimed");
    }

    #[test]
    fn live_lock_blocks_without_force() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        let mypid = std::process::id();
        std::fs::write(&lf, format!("pid = {mypid}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();
        assert!(acquire(&lf, false).is_err());
        // force overrides
        let _g = acquire(&lf, true).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn an_unreadable_lock_file_is_never_reclaimed() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        let mypid = std::process::id();
        let original = format!("pid = {mypid}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n");
        std::fs::write(&lf, &original).unwrap();

        // Write-only, no read: isolates the *read* failure from a write
        // failure. `acquire` writes the lock file directly (not through
        // atomic_write), so a 0o000 file would already fail to reclaim for
        // the wrong reason (EACCES on the write) even without the read fix.
        let mut perms = std::fs::metadata(&lf).unwrap().permissions();
        perms.set_mode(0o200);
        std::fs::set_permissions(&lf, perms).unwrap();

        // Pre-fix: an unreadable lock file fell through to "stale" and was
        // silently reclaimed here, even though the recorded pid (this very
        // process) is alive — breaking the mutual exclusion the lock exists for.
        let result = acquire(&lf, false);

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&lf).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&lf, perms).unwrap();

        assert!(result.is_err(), "an unreadable lock file must not be treated as stale");
        assert_eq!(
            std::fs::read_to_string(&lf).unwrap(),
            original,
            "the original lock file must survive untouched"
        );
    }
}
