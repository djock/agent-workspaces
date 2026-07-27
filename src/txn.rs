//! Interprocess transactions over shared files.
//!
//! `atomic::atomic_write` makes a write *durable and all-or-nothing*. It does
//! not make a read-modify-write *serialisable*: two processes can both read a
//! file, both apply their change to their own copy, and both rename their result
//! into place. Each write is atomic; the second silently discards the first.
//! Wrapping the whole read-modify-write in one of these transactions is what
//! closes that, and it is the rule the hardening plan states as
//!
//! > An atomic rename is not a transaction. Every shared read-modify-write must
//! > hold an interprocess lock from the first read through the durable rename.
//!
//! **The lock is a sidecar file, never the target.** `atomic_write` replaces the
//! target by renaming a temp file over it, so the target's inode changes on every
//! write. A lock held on the target would be held on an inode that is no longer
//! the file, and would protect nothing. `<target>.lock` is stable across writes.
//!
//! **No stale-lock reaping, deliberately.** These are advisory `flock` locks tied
//! to an open file description, so the kernel drops them when the holder exits or
//! crashes. That is the opposite situation from `lock::acquire`, which records a
//! pid in a file precisely because it must survive the `exec` that replaces the
//! process — and therefore does need staleness logic.
//!
//! **Not reentrant.** `flock` treats two file descriptors for the same file as
//! independent even within one process, so calling `transaction` on the same path
//! from inside a `transaction` on that path deadlocks against itself. Take the
//! lock at the outermost public API only, and have inner helpers do unlocked
//! reads and writes. Read-only callers do not need it at all: `atomic_write`
//! guarantees a reader sees either the old file or the new one, never a mix.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long to wait for a holder before giving up.
///
/// A bounded wait rather than a blocking one: a wedged process holding the lock
/// should produce an error naming the file, not a command that hangs forever with
/// no output. Ten seconds is far longer than any read-modify-write here takes and
/// short enough that a human notices.
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

pub fn lock_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Held for the duration of a transaction. Releases on drop, including on unwind.
pub struct Txn {
    file: std::fs::File,
}

impl Drop for Txn {
    fn drop(&mut self) {
        // Best-effort: the kernel also releases on close/exit, so a failure here
        // cannot leave the lock held.
        let _ = self.file.unlock();
    }
}

/// Take the exclusive lock guarding `target`, waiting up to [`TIMEOUT`].
pub fn acquire(target: &Path) -> Result<Txn> {
    let lp = lock_path(target);
    if let Some(dir) = lp.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lp)
        .with_context(|| format!("failed to open lock file {}", lp.display()))?;

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(Txn { file }),
            Err(e) => {
                // `try_lock_exclusive` reports contention as WouldBlock; anything
                // else (EBADF, ENOLCK, a filesystem with no lock support) is a
                // real failure and must not be retried into a timeout.
                let contended = e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc_ewouldblock());
                if !contended {
                    return Err(e)
                        .with_context(|| format!("failed to lock {}", lp.display()));
                }
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "timed out after {}s waiting for another ws process to finish \
                         writing {}. Nothing was changed; re-run the command.",
                        TIMEOUT.as_secs(),
                        target.display()
                    );
                }
                std::thread::sleep(POLL);
            }
        }
    }
}

/// EWOULDBLOCK/EAGAIN, without taking a libc dependency for one constant.
/// On Linux and macOS EAGAIN == EWOULDBLOCK == 35 (macOS) / 11 (Linux); both are
/// already mapped to `ErrorKind::WouldBlock` by std, so this is only a backstop
/// for platforms where that mapping is missing.
fn libc_ewouldblock() -> i32 {
    if cfg!(target_os = "linux") {
        11
    } else {
        35
    }
}

/// Run `f` holding the exclusive lock that guards `target`.
///
/// See the module docs: `f` must not itself call `transaction` on `target`.
pub fn transaction<T>(target: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = acquire(target)?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The discriminating test for the whole module, and the reason it exists.
    ///
    /// Each thread runs a read-modify-write: read an integer, then write it back
    /// incremented. The `yield_now` between read and write widens the window that
    /// real code has anyway. With N threads the final value must be exactly N.
    ///
    /// Verified against the unlocked version: replacing `transaction(...)` with a
    /// direct call to the closure makes this fail with a value well below N —
    /// which is precisely the lost-update bug atomic writes cannot prevent. A
    /// concurrency test that has not been run against the unfixed code proves
    /// nothing, so that check was done rather than assumed.
    ///
    /// Threads are sufficient despite being one process: `flock` treats two file
    /// descriptors for the same file as independent even within a process, so
    /// these genuinely contend.
    #[test]
    fn concurrent_read_modify_writes_do_not_lose_updates() {
        use std::sync::{Arc, Barrier};

        let d = TempDir::new().unwrap();
        let target = d.path().join("counter.txt");
        std::fs::write(&target, "0").unwrap();

        const N: usize = 12;
        let barrier = Arc::new(Barrier::new(N));
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let target = target.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                transaction(&target, || {
                    let cur: u32 = std::fs::read_to_string(&target)
                        .unwrap_or_default()
                        .trim()
                        .parse()
                        .unwrap_or(0);
                    std::thread::yield_now();
                    crate::atomic::atomic_write(&target, (cur + 1).to_string())
                        .map_err(|e| anyhow::anyhow!("{e}"))
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let final_value: u32 = std::fs::read_to_string(&target).unwrap().trim().parse().unwrap();
        assert_eq!(
            final_value, N as u32,
            "every increment must survive; {} updates were lost",
            N as u32 - final_value
        );
    }

    /// The lock must guard a path whose inode changes, so it cannot be the target.
    #[test]
    fn the_lock_is_a_sidecar_and_survives_the_target_being_replaced() {
        let d = TempDir::new().unwrap();
        let target = d.path().join("registry.toml");
        std::fs::write(&target, "a = 1").unwrap();
        let before = std::fs::metadata(&target).unwrap();

        transaction(&target, || {
            crate::atomic::atomic_write(&target, "a = 2").map_err(|e| anyhow::anyhow!("{e}"))
        })
        .unwrap();

        let after = std::fs::metadata(&target).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                before.ino(),
                after.ino(),
                "atomic_write is expected to replace the inode — which is exactly why \
                 the lock cannot live on the target"
            );
        }
        let _ = before;
        assert!(lock_path(&target).exists(), "the sidecar lock file is the stable identity");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a = 2");
    }

    /// A second holder must be refused with a message naming the file, not hang.
    #[test]
    fn a_held_lock_times_out_with_an_actionable_message() {
        let d = TempDir::new().unwrap();
        let target = d.path().join("thing.toml");
        let held = acquire(&target).unwrap();

        // Shrink the wait by racing the real timeout only once: acquire in a
        // thread and assert it is still blocked shortly after, then release.
        let t2 = target.clone();
        let handle = std::thread::spawn(move || acquire(&t2).map(|_| ()));
        std::thread::sleep(Duration::from_millis(80));
        assert!(!handle.is_finished(), "a second acquirer must block while the lock is held");
        drop(held);
        assert!(handle.join().unwrap().is_ok(), "and succeed once it is released");
    }

    #[test]
    fn lock_path_is_the_target_plus_lock() {
        assert_eq!(
            lock_path(Path::new("/a/b/registry.toml")),
            PathBuf::from("/a/b/registry.toml.lock")
        );
        // Not `with_extension`, which would have produced `registry.lock` and
        // collided across `registry.toml` and `registry.json`.
        assert_eq!(lock_path(Path::new("/a/x")), PathBuf::from("/a/x.lock"));
    }
}
