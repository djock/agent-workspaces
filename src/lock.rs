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

fn lock_body() -> String {
    format!(
        "pid = {}\nhost = \"{}\"\ntty = \"{}\"\nstarted = \"{}\"\n",
        std::process::id(),
        hostname(),
        std::env::var("TTY").unwrap_or_else(|_| "?".into()),
        crate::now_iso(),
    )
}

/// Create the lock file only if it does not already exist, atomically, and
/// with its body already in it.
///
/// Two properties, and the second is as load-bearing as the first:
///
/// 1. **Exclusive.** The claim is a single syscall that fails if the path
///    exists, so when two processes race the kernel picks exactly one winner.
///    An `exists()`-then-`write` sequence has a window in which both racers see
///    "no lock" and both write.
/// 2. **Never observable empty.** `create_new` + `write_all` satisfies (1) but
///    not (2): between the two calls the lock file exists with zero bytes. An
///    empty file is valid TOML with no `pid`, which `acquire` step 2 reads as a
///    stale lock and reclaims — so a loser deleted the winner's lock and took
///    the workspace. Sixteen racing threads produced up to nine simultaneous
///    "holders" that way.
///
/// Writing the body into a private temp file and then `hard_link`ing it into
/// place gets both: `link` fails with `EEXIST` if the target exists, and the
/// name it publishes already has the content behind it. There is no moment at
/// which the lock file exists but is empty.
///
/// `rename` is the wrong primitive here despite being the codebase's usual
/// atomic-publish tool (`crate::atomic`) — it *replaces* the destination, which
/// is exactly the "steal a live lock" behaviour this function exists to
/// prevent.
fn create_exclusive(lock_file: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique per claim, not merely per process: `crate::atomic`'s pid-suffixed
    // temp name is enough for whole-file writers, but threads within one
    // process race here (the test does exactly that), and a shared temp name
    // would let two claimants scribble over each other's body before either
    // linked it.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = lock_file.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(body.as_bytes())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    let linked = std::fs::hard_link(&tmp, lock_file);
    // The temp name has served its purpose either way: on success the inode
    // lives on under the lock's name, on failure it must not be left behind.
    let _ = std::fs::remove_file(&tmp);
    linked
}

pub fn acquire(lock_file: &Path, force: bool) -> Result<LockGuard> {
    if let Some(dir) = lock_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let guard = || LockGuard { path: lock_file.to_path_buf(), released: false };
    let body = lock_body();

    // 1. Uncontended case, and the race decided correctly: if no lock file
    //    exists, exactly one caller creates it.
    match create_exclusive(lock_file, &body) {
        Ok(()) => return Ok(guard()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to create lock file {}", lock_file.display()))
        }
    }

    // 2. A lock file exists. Decide whether it may be taken over at all.
    if !force {
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

    // 3. Reclaim a stale (or force-overridden) lock. Remove then create
    //    exclusively rather than overwriting in place: two callers can reach
    //    this point having both judged the *same* lock stale, and an
    //    unconditional write would let both succeed. Going back through
    //    `create_exclusive` means the kernel still picks one.
    match std::fs::remove_file(lock_file) {
        Ok(()) => {}
        // The holder released it between step 2 and here — nothing to remove.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to clear stale lock {}", lock_file.display()))
        }
    }
    match create_exclusive(lock_file, &body) {
        Ok(()) => Ok(guard()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => bail!(
            "another ws process claimed this workspace while a stale lock was being \
             reclaimed; nothing was changed, re-run the command."
        ),
        Err(e) => Err(e)
            .with_context(|| format!("failed to create lock file {}", lock_file.display())),
    }
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

    /// `create_exclusive` is the mechanism the TOCTOU fix rests on, and this is
    /// the test that actually **discriminates** against the old implementation.
    ///
    /// The old `acquire` ended in `fs::write`, which creates *or truncates*. This
    /// asserts the opposite semantics: on an existing path the call must fail
    /// with `AlreadyExists` and leave the bytes untouched. Swap `create_exclusive`
    /// back to `fs::write` and this fails immediately — which is more than can be
    /// said for the contention test below, see its note.
    #[test]
    fn create_exclusive_refuses_an_existing_file_and_leaves_it_intact() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        let original = "pid = 4242\nhost = \"other\"\ntty = \"?\"\nstarted = \"earlier\"\n";
        std::fs::write(&lf, original).unwrap();

        let err = create_exclusive(&lf, "pid = 1\n").expect_err("must not create over an existing file");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::AlreadyExists,
            "the O_EXCL semantics are the whole point; got {err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&lf).unwrap(),
            original,
            "a losing racer must not have truncated the winner's lock"
        );

        // And it does create when the path is free.
        let free = d.path().join("free");
        create_exclusive(&free, "pid = 7\n").unwrap();
        assert_eq!(std::fs::read_to_string(&free).unwrap(), "pid = 7\n");
    }

    /// Mutual exclusion under real contention: N threads race for one lock and
    /// exactly one may hold it.
    ///
    /// **Honest limitation, verified by experiment:** this test is *not* a
    /// discriminator for the TOCTOU fix. It was run against the old
    /// `exists()`-then-`fs::write` implementation and still passed, because every
    /// thread shares this process's pid — so a losing thread that observes the
    /// winner's file reads back its own live pid and is rejected by the liveness
    /// check regardless of whether the create was atomic. The genuine window is a
    /// few instructions wide and separate processes are needed to open it
    /// reliably.
    ///
    /// It is kept as a guard against the *other* way this can break — a future
    /// change that lets two callers through under contention for some reason
    /// unrelated to atomicity — but the proof of the fix itself is
    /// `create_exclusive_refuses_an_existing_file_and_leaves_it_intact` above.
    ///
    /// **It did catch one**, at roughly one run in three: when `create_exclusive`
    /// created the file and wrote the body as two steps, the winner's lock was
    /// observable *empty*. An empty file parses as a valid TOML table with no
    /// `pid`, which step 2 classifies as stale — so a loser deleted the winner's
    /// lock and took it. The pid-sharing argument above is what makes this the
    /// one window threads can still expose: it does not depend on comparing
    /// pids, only on reading the file mid-claim.
    ///
    /// Hence the repetition: one round caught it ~37% of the time, which reads
    /// as flakiness. Rounds make a regression a certainty rather than a rumour.
    #[test]
    fn exactly_one_of_many_simultaneous_acquirers_wins() {
        use std::sync::{Arc, Barrier};

        const N: usize = 16;
        const ROUNDS: usize = 24;

        let d = TempDir::new().unwrap();
        // Ensure the parent exists up front so the race is over the lock file
        // itself and not over create_dir_all.
        std::fs::create_dir_all(d.path()).unwrap();

        for round in 0..ROUNDS {
            // A fresh path per round: a surviving guard from the previous round
            // would otherwise decide the next one.
            let lf = d.path().join(format!("lock{round}"));
            let barrier = Arc::new(Barrier::new(N));
            let mut handles = Vec::with_capacity(N);
            for _ in 0..N {
                let lf = lf.clone();
                let barrier = Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    // Release all threads at the same instant to widen the window.
                    barrier.wait();
                    acquire(&lf, false)
                }));
            }

            // Hold every guard until after counting: dropping a winner's guard
            // deletes the file and would let a later caller win too, masking a
            // double-acquire.
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners = results.iter().filter(|r| r.is_ok()).count();
            assert_eq!(
                winners, 1,
                "exactly one acquirer may hold the lock, got {winners} in round {round}"
            );

            for r in &results {
                if let Err(e) = r {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("in use") || msg.contains("claimed this workspace"),
                        "a loser must say why it lost, got: {msg}"
                    );
                }
            }
        }
    }

    /// `--force` must still be able to take a live lock — the race fix must not
    /// have turned the reclaim path into an unconditional refusal.
    #[test]
    fn force_still_reclaims_a_live_lock_after_the_race_fix() {
        let d = TempDir::new().unwrap();
        let lf = d.path().join("lock");
        let mypid = std::process::id();
        std::fs::write(&lf, format!("pid = {mypid}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();
        assert!(acquire(&lf, false).is_err(), "live lock blocks without force");
        let _g = acquire(&lf, true).expect("force must reclaim");
        // The reclaimed file records *this* acquisition, not the old contents.
        let body = std::fs::read_to_string(&lf).unwrap();
        assert!(body.contains(&format!("pid = {mypid}")));
        assert!(!body.contains("started = \"t\""), "stale body must be replaced: {body}");
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
