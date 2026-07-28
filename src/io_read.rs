//! One place for the read policy every state file in ws shares:
//! **absent → default, unreadable → refuse.**
//!
//! That policy was hand-written sixteen times (config, registry, meta ×2,
//! contract, secrets ×2, hooksetup ×8, internal, queue). Every copy was correct,
//! but a policy spread over sixteen sites cannot be checked — the interesting
//! question is "does anything treat an unreadable file as empty?", and answering
//! it meant reading sixteen matches. Now it means reading one function.
//!
//! Refusing on an unreadable file matters more than it looks: defaulting would
//! let a permission error or a partially-written file present as "no data", and
//! the very next write would persist that emptiness over the real contents.

use anyhow::{Context, Result};
use std::path::Path;

/// `Ok(None)` when the file does not exist, `Ok(Some(text))` when it reads, and
/// `Err` for every other failure — with the path in the message, because the
/// caller's own error text rarely says which file it was reading.
pub fn read_or_absent(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context(format!("failed to read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn an_absent_file_is_none_not_an_error() {
        let d = TempDir::new().unwrap();
        assert_eq!(read_or_absent(&d.path().join("nope.toml")).unwrap(), None);
    }

    #[test]
    fn a_present_file_returns_its_contents() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("x.toml");
        std::fs::write(&p, "a = 1\n").unwrap();
        assert_eq!(read_or_absent(&p).unwrap().as_deref(), Some("a = 1\n"));
    }

    /// The discriminating case: a file that exists but cannot be read must be an
    /// error, never `None`. If this ever returns `Ok(None)` the next write
    /// silently overwrites real data with a default.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_file_refuses_and_names_the_path() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc_geteuid() } == 0 {
            return; // root bypasses the mode bits; the assertion would be vacuous
        }
        let d = TempDir::new().unwrap();
        let p = d.path().join("locked.toml");
        std::fs::write(&p, "secret = 1\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();

        let err = read_or_absent(&p).unwrap_err();
        let msg = format!("{err:#}");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(msg.contains("locked.toml"), "error must name the file: {msg}");
    }

    /// A directory is not "absent" either — it is a real misconfiguration and
    /// must surface rather than read as no-data.
    #[test]
    fn a_directory_where_a_file_belongs_refuses() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("adir");
        std::fs::create_dir(&p).unwrap();
        assert!(read_or_absent(&p).is_err());
    }

    #[cfg(unix)]
    unsafe fn libc_geteuid() -> u32 {
        // Avoid a libc dependency for one call: `id -u` is already how the rest
        // of this crate's tests detect root.
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1)
    }
}
