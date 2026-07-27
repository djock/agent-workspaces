use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    workspaces: BTreeMap<String, String>,
}

pub fn registry_path() -> PathBuf {
    // Shares config's base dir so registry.toml and config.toml always sit
    // together (and both honor XDG_CONFIG_HOME, which dirs::config_dir()
    // ignores on macOS — see config::ws_config_dir).
    crate::config::ws_config_dir().join("registry.toml")
}

fn load() -> Result<Registry> {
    match std::fs::read_to_string(registry_path()) {
        Ok(s) => toml::from_str(&s)
            .map_err(|e| anyhow::anyhow!("registry.toml is corrupt (refusing to overwrite): {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e).context("failed to read registry.toml"),
    }
}

fn save(r: &Registry) -> Result<()> {
    let path = registry_path();
    crate::atomic::atomic_write(&path, toml::to_string_pretty(r)?)?;
    Ok(())
}

pub fn register(name: &str, path: &Path) -> Result<()> {
    // The second chokepoint. `contract::init` validates before writing files;
    // this one guards the path that makes a name *visible* to `-list`, `-spawn`
    // and the TUI — `-adopt` on a directory that already has `.ws/` re-registers
    // without going through `init` at all.
    crate::workspace::validate_name(name)?;
    let mut r = load()?;
    r.workspaces
        .insert(name.to_string(), path.to_string_lossy().to_string());
    save(&r)
}

pub fn unregister(name: &str) -> Result<()> {
    let mut r = load()?;
    r.workspaces.remove(name);
    save(&r)
}

/// A corrupt or unreadable registry must not read as quietly empty — that
/// makes "-list" say "no workspaces yet" when the most plausible read is "my
/// data is gone". Warn on stderr, then fall back to empty so listing
/// commands still degrade gracefully instead of aborting.
fn load_or_warn() -> Registry {
    match load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ws: warning: could not read registry.toml, treating it as empty: {e:#}");
            Registry::default()
        }
    }
}

pub fn lookup(name: &str) -> Option<PathBuf> {
    load_or_warn().workspaces.get(name).map(PathBuf::from)
}

/// Look a workspace up, surfacing a registry that could not be read.
/// `Ok(None)` means "not registered"; `Err` means "I could not tell" — the
/// difference matters to any caller that would otherwise *create* something.
pub fn lookup_checked(name: &str) -> Result<Option<PathBuf>> {
    Ok(load()?.workspaces.get(name).map(PathBuf::from))
}

/// The registry, with read/parse failure surfaced. The TUI has no stderr the
/// user is watching, so it must be able to tell "no workspaces" from "I could
/// not read the file that lists them".
pub fn all_checked() -> Result<Vec<(String, PathBuf)>> {
    Ok(load()?
        .workspaces
        .into_iter()
        .map(|(n, p)| (n, PathBuf::from(p)))
        .collect())
}

pub fn all() -> Vec<(String, PathBuf)> {
    load_or_warn()
        .workspaces
        .into_iter()
        .map(|(n, p)| (n, PathBuf::from(p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Every test in this module resolves `registry_path()` through the
    // process-global XDG_CONFIG_HOME env var. `.cargo/config.toml` pins
    // RUST_TEST_THREADS=1 today, which happens to serialize these tests, but
    // that pin is a project-wide default this module shouldn't depend on —
    // `cargo test -- --test-threads=4` would otherwise let one test's env var
    // (or, worse, its 0o000-chmod'd registry.toml from the "unreadable
    // registry" test) leak into another test's run. Serialize explicitly.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    // Isolate the config dir for each test via XDG_CONFIG_HOME. Also takes
    // TEST_LOCK for the caller's whole test body: the guard returned here
    // must be bound (e.g. `let _guard = ...`) before `iso()` is called so it
    // is dropped after the TempDir, once the test is fully done touching
    // XDG_CONFIG_HOME / the registry file.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn iso() -> TempDir {
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path());
        d
    }

    #[test]
    fn all_checked_surfaces_a_corrupt_registry() {
        let _guard = lock();
        let _d = iso();
        let path = registry_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        assert!(all_checked().is_err(), "a corrupt registry must not read as zero workspaces");
        assert!(all().is_empty(), "lenient all() still degrades for existing callers");
    }

    #[test]
    fn lookup_checked_surfaces_a_corrupt_registry() {
        let _guard = lock();
        let _d = iso();
        let path = registry_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        assert!(lookup_checked("anything").is_err(), "a corrupt registry must not read as 'no such workspace'");
        assert!(lookup("anything").is_none(), "the lenient reader still degrades for its existing callers");
    }

    #[test]
    fn register_lookup_unregister() {
        let _guard = lock();
        let _d = iso();
        register("alpha", std::path::Path::new("/x/alpha")).unwrap();
        assert_eq!(lookup("alpha"), Some(std::path::PathBuf::from("/x/alpha")));
        assert!(all().iter().any(|(n, _)| n == "alpha"));
        unregister("alpha").unwrap();
        assert_eq!(lookup("alpha"), None);
    }

    #[test]
    fn corrupt_registry_refuses_to_overwrite() {
        let _guard = lock();
        let _d = iso();
        let path = registry_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let result = register("x", std::path::Path::new("/x/x"));
        assert!(
            result.is_err(),
            "register() must refuse to save over a corrupt registry.toml"
        );

        // The corrupt file must still be there untouched (not silently wiped).
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "this is not valid toml {{{");
    }

    #[test]
    #[cfg(unix)]
    fn register_refuses_when_an_existing_registry_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let _guard = lock();
        let _d = iso();
        let path = registry_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let original = "[workspaces]\nkeep-me = \"/x/keep-me\"\n";
        std::fs::write(&path, original).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        // Unreadable ≠ absent: with the pre-fix code the read error was
        // swallowed, an empty registry was written to a temp file, and the
        // rename succeeded — silently destroying every other registration.
        let result = register("new-entry", std::path::Path::new("/x/new-entry"));

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();

        assert!(result.is_err(), "register must not treat an unreadable registry as empty");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "the original registry must survive untouched"
        );
    }
}
