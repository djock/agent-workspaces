use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use argon2::Argon2;
use rand::RngCore;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A build with no platform credential store enabled falls back to keyring's
/// mock in-memory store, which loses every secret at process exit while still
/// reporting success — see the `keyring` note in `Cargo.toml`. The failure is
/// invisible in-process, so no unit test can catch it; refuse to build instead.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "windows", unix,)))]
compile_error!(
    "no keyring platform store is enabled for this target: `ws` would silently \
     store secrets in an in-memory mock and lose them at exit. Add a \
     target-specific `keyring` feature in Cargo.toml before building here."
);

pub trait SecretStore {
    fn set(&self, name: &str, value: &str) -> Result<()>;
    /// Store several values as ONE transaction.
    ///
    /// The redaction hook needs this: calling `set` per credential meant one full
    /// Argon2id derivation and whole-store re-encryption *each*, which a file with
    /// a few dozen secrets could not finish inside the hook timeout — and being
    /// killed part-way left values stored with the plaintext still on disk.
    fn set_many(&self, pairs: &[(String, String)]) -> Result<()>;
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn list(&self) -> Result<Vec<String>>;
    fn remove(&self, name: &str) -> Result<()>;
    fn purge(&self) -> Result<()>;
    fn backend_name(&self) -> &'static str;
}

pub fn secrets_dir() -> PathBuf {
    crate::config::ws_config_dir().join("secrets")
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("invalid secret name: {name:?} (use letters, digits, _ or -)");
    }
    Ok(())
}

// ---------- FileStore (AES-256-GCM + Argon2id) ----------

pub struct FileStore {
    pub path: PathBuf,
    pub password: String,
}

impl FileStore {
    fn derive_key(&self, salt: &[u8]) -> Result<[u8; 32]> {
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(self.password.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow!("key derivation failed: {e}"))?;
        Ok(key)
    }
    fn load(&self) -> Result<BTreeMap<String, String>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            // Absent is a legitimately empty store. Unreadable is not: mapping
            // it to empty means the next `set` writes a store containing only
            // the new secret over every existing one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", self.path.display()))
            }
        };
        if bytes.len() < 28 {
            bail!("corrupt secrets file (too short)");
        }
        let (salt, rest) = bytes.split_at(16);
        let (nonce, ct) = rest.split_at(12);
        let key = self.derive_key(salt)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let pt = cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| anyhow!("wrong password or corrupt secrets file"))?;
        serde_json::from_slice(&pt).context("corrupt secrets file (bad json)")
    }
    fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
        let mut salt = [0u8; 16];
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut nonce);
        let key = self.derive_key(&salt)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        let pt = serde_json::to_vec(map)?;
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), pt.as_ref())
            .map_err(|e| anyhow!("encryption failed: {e}"))?;
        let mut out = Vec::with_capacity(28 + ct.len());
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        // Owner-only, applied to the temp file at creation rather than to the
        // real path after the rename. The after-the-fact chmod this replaces
        // left the credential file world-readable at its real path between
        // the rename and the chmod, and could return Err from a chmod that
        // failed *after* the write succeeded — telling the user the operation
        // failed while a 0644 credential file sat on disk.
        crate::atomic::atomic_write_with_mode(&self.path, &out, Some(0o600))?;
        Ok(())
    }
}

impl SecretStore for FileStore {
    // `set` and `remove` decrypt the whole store, change one entry and re-encrypt.
    // That is a read-modify-write, so it is transacted: the secret-redaction hook
    // can store several values in quick succession while the user runs
    // `ws -secrets set` in another terminal, and an unlocked version would lose
    // one of them. Losing a credential silently is the worst outcome this file
    // has, so it gets the lock even though the window is small.
    fn set(&self, name: &str, value: &str) -> Result<()> {
        validate_name(name)?;
        crate::txn::transaction(&self.path, || {
            let mut m = self.load()?;
            m.insert(name.to_string(), value.to_string());
            self.save(&m)
        })
    }
    fn set_many(&self, pairs: &[(String, String)]) -> Result<()> {
        for (n, _) in pairs {
            validate_name(n)?;
        }
        crate::txn::transaction(&self.path, || {
            let mut m = self.load()?;
            for (n, v) in pairs {
                m.insert(n.clone(), v.clone());
            }
            self.save(&m)
        })
    }
    fn get(&self, name: &str) -> Result<Option<String>> {
        validate_name(name)?;
        Ok(self.load()?.get(name).cloned())
    }
    fn list(&self) -> Result<Vec<String>> {
        Ok(self.load()?.keys().cloned().collect())
    }
    fn remove(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        crate::txn::transaction(&self.path, || {
            let mut m = self.load()?;
            m.remove(name);
            self.save(&m)
        })
    }
    // Transacted like `set`/`remove`: without the lock, a concurrent `set`
    // that loaded the store before this `remove_file` and saved after it
    // would resurrect the file with its own read-modify-write's copy of every
    // other secret — undoing the purge while both calls report success.
    fn purge(&self) -> Result<()> {
        crate::txn::transaction(&self.path, || match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            // Absent is success: there was nothing to purge.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("failed to remove {}", self.path.display())),
        })
    }
    fn backend_name(&self) -> &'static str {
        "file"
    }
}

// ---------- KeyringStore (OS vault + plaintext names index) ----------

/// The index is the only record of *which* names live in the OS vault — the
/// vault itself cannot be enumerated by service. So the same rule as
/// `FileStore::load` applies, and harder: absent is a legitimately empty
/// store, unreadable is not. Mapping a read error to empty made `set`
/// rewrite the index with only the new name, `remove` rewrite it empty, and
/// — worst — `purge` delete the index having deleted *nothing* from the
/// vault, reporting success while stranding every secret behind a name that
/// no longer exists anywhere.
///
/// Free function (takes a bare path rather than `&self`) so `transact_index`
/// below can call it without a `KeyringStore`, and so the read-modify-write it
/// powers can be unit-tested with no OS vault involved at all.
fn read_index_at(index: &Path) -> Result<Vec<String>> {
    match std::fs::read_to_string(index) {
        Ok(s) => Ok(s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", index.display())),
    }
}

fn write_index_at(index: &Path, names: &[String]) -> Result<()> {
    let mut v: Vec<&String> = names.iter().collect();
    v.sort();
    v.dedup();
    let body = v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    crate::atomic::atomic_write(index, body)
}

/// The keyring index's entire read-modify-write, holding the interprocess
/// lock on `index` from the first read through the durable write — same
/// pattern as `FileStore::set`/`remove`'s `txn::transaction` wrapping, and for
/// the same reason: two `ws -secrets set` invocations racing on one workspace
/// must not silently discard one of their updates to the index.
///
/// `set`/`remove` differ only in what `f` does to the list (and, for
/// `remove`, in the vault side effect it layers into `f`) — this function is
/// the whole RMW surface, so a test that races it directly with plain
/// threads and a bare path is racing exactly what production races, without
/// depending on a working OS vault in CI.
/// Like `transact_index`, but the closure may fail — and when it does, the index
/// is left exactly as it was.
fn transact_index_checked(
    index: &Path,
    f: impl FnOnce(Vec<String>) -> Result<Vec<String>>,
) -> Result<()> {
    crate::txn::transaction(index, || {
        let names = read_index_at(index)?;
        let next = f(names)?;
        write_index_at(index, &next)
    })
}

fn transact_index(index: &Path, f: impl FnOnce(Vec<String>) -> Vec<String>) -> Result<()> {
    crate::txn::transaction(index, || {
        let names = read_index_at(index)?;
        write_index_at(index, &f(names))
    })
}

pub struct KeyringStore {
    service: String, // "ws:<ws_name>"
    index: PathBuf,  // <secrets_dir>/<ws_name>.keyring-index
}

impl KeyringStore {
    pub fn new(ws_name: &str) -> Self {
        KeyringStore {
            service: format!("ws:{ws_name}"),
            index: secrets_dir().join(format!("{ws_name}.keyring-index")),
        }
    }
    fn read_index(&self) -> Result<Vec<String>> {
        read_index_at(&self.index)
    }
    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, name).map_err(|e| anyhow!("keyring: {e}"))
    }
    /// Probe whether a working OS vault is available (best-effort, non-fatal).
    pub fn available() -> bool {
        match keyring::Entry::new("ws:__probe__", "probe") {
            Ok(e) => {
                let ok = e.set_password("x").is_ok() && e.get_password().is_ok();
                let _ = e.delete_credential();
                ok
            }
            Err(_) => false,
        }
    }
}

impl SecretStore for KeyringStore {
    fn set(&self, name: &str, value: &str) -> Result<()> {
        validate_name(name)?;
        self.entry(name)?.set_password(value).map_err(|e| anyhow!("keyring set: {e}"))?;
        transact_index(&self.index, |mut names| {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
            names
        })
    }
    /// One index transaction for the whole batch. The vault writes still happen
    /// per entry — the OS API has no batch form — but they are cheap here, unlike
    /// the file backend's whole-store re-encryption. Every vault write lands
    /// before the index names any of them, so a failure part-way cannot leave a
    /// name listed for a credential that is not in the vault.
    fn set_many(&self, pairs: &[(String, String)]) -> Result<()> {
        for (n, _) in pairs {
            validate_name(n)?;
        }
        for (n, v) in pairs {
            self.entry(n)?.set_password(v).map_err(|e| anyhow!("keyring set {n}: {e}"))?;
        }
        transact_index(&self.index, |mut names| {
            for (n, _) in pairs {
                if !names.iter().any(|x| x == n) {
                    names.push(n.clone());
                }
            }
            names
        })
    }
    fn get(&self, name: &str) -> Result<Option<String>> {
        validate_name(name)?;
        match self.entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("keyring get: {e}")),
        }
    }
    fn list(&self) -> Result<Vec<String>> {
        let mut v = self.read_index()?;
        v.sort();
        Ok(v)
    }
    fn remove(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        // Read the index *before* deleting from the vault: if the index is
        // unreadable we cannot rewrite it correctly, and dropping the vault
        // entry first would leave the name listed but unresolvable. Doing the
        // read, the vault delete and the write inside one `transact_index`
        // call also closes the race with a concurrent `set`/`remove`/`purge`
        // on the same index.
        // The vault delete must *succeed* before the name leaves the index.
        // Dropping the name regardless meant `ws -secrets rm X` printed
        // "removed X" while the value sat in the OS keychain with no name left
        // anywhere to reach it by — the precise failure this index exists to
        // prevent, per its own doc comment.
        transact_index_checked(&self.index, |names| {
            let e = self
                .entry(name)
                .with_context(|| format!("cannot open the keychain entry for {name}"))?;
            match e.delete_credential() {
                Ok(()) => {}
                // Already gone in the vault: the index entry is the leftover, so
                // removing it is exactly right.
                Err(keyring::Error::NoEntry) => {}
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "could not delete {name} from the keychain ({e}); \
                         leaving it listed so it stays reachable"
                    ))
                }
            }
            Ok(names.into_iter().filter(|n| n != name).collect())
        })
    }
    fn purge(&self) -> Result<()> {
        crate::txn::transaction(&self.index, || {
            // Enumerate first and propagate: deleting the index after failing
            // to read it would destroy the only list of what is still in the
            // vault, while telling the user their secrets are gone.
            let names = read_index_at(&self.index)?;
            // Collect failures rather than stopping at the first: a purge that
            // gives up halfway is worse than one that removes what it can and
            // then says exactly what it could not.
            let mut stuck: Vec<String> = Vec::new();
            for name in &names {
                let deleted = match self.entry(name) {
                    Ok(e) => matches!(e.delete_credential(), Ok(()) | Err(keyring::Error::NoEntry)),
                    Err(_) => false,
                };
                if !deleted {
                    stuck.push(name.clone());
                }
            }
            if !stuck.is_empty() {
                // The index is deliberately left in place: it is the only record
                // of what is still in the vault.
                anyhow::bail!(
                    "could not delete {} secret(s) from the keychain ({}); \
                     they are still stored and still listed",
                    stuck.len(),
                    stuck.join(", ")
                );
            }
            // Only a genuinely absent index counts as nothing to remove.
            match std::fs::remove_file(&self.index) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).with_context(|| {
                    format!("removed the secrets but could not delete {}", self.index.display())
                }),
            }
        })
    }
    fn backend_name(&self) -> &'static str {
        "keyring"
    }
}

// ---------- selection + resolution ----------

fn selected_backend() -> String {
    if let Ok(b) = std::env::var("WS_SECRETS_BACKEND") {
        if !b.is_empty() {
            return b;
        }
    }
    crate::config::load().secrets_backend
}

fn file_password() -> Result<String> {
    if let Ok(p) = std::env::var("WS_SECRETS_PASSWORD") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    rpassword::prompt_password("ws secrets master password: ")
        .context("could not read master password")
}

/// True when `open` would have to ask a human for the master password.
///
/// Callers that have no human attached must check this first. A hook handler is
/// the case that matters: its stdin is the payload JSON, so
/// `rpassword::prompt_password` opens `/dev/tty` and either fails or — with a
/// tty inherited from the terminal the agent was launched in — blocks the agent
/// mid-turn waiting for a password nobody is going to type. Deciding up front
/// beats discovering it by hanging.
///
/// Kept beside `selected_backend` and `file_password` deliberately: it answers a
/// question about *their* behaviour, and a copy of this reasoning in another
/// module would go stale the first time the backend logic changed.
pub fn would_prompt_for_password() -> bool {
    // Supplied out of band → no prompt, whatever the backend.
    if std::env::var("WS_SECRETS_PASSWORD").map(|p| !p.is_empty()).unwrap_or(false) {
        return false;
    }
    match selected_backend().as_str() {
        "keyring" => false,
        "file" => true,
        // auto: only the file fallback prompts.
        _ => !KeyringStore::available(),
    }
}

/// Which backend `open` would select, without opening anything.
///
/// `backend_name` on a live store cannot answer this: building the store is
/// exactly the step that demands the master password, so reporting the
/// configured backend used to authenticate for a question that decrypts
/// nothing. Resolving `auto` still probes the vault — that probe is what the
/// answer *means* — but it never prompts.
pub fn selected_backend_name() -> &'static str {
    match selected_backend().as_str() {
        "keyring" => "keyring",
        "file" => "file",
        _ if KeyringStore::available() => "keyring",
        _ => "file",
    }
}

/// Can `file_password` actually reach a human?
///
/// This mirrors what rpassword is about to do rather than guessing from stdin:
/// on unix it reads `/dev/tty`, so `ws -secrets set K < value` piped from a
/// terminal must still be allowed to prompt even though stdin is not a tty.
/// With no controlling terminal that open fails with ENXIO — the error users
/// were seeing raw — so asking first turns an errno into a sentence.
pub fn can_prompt() -> bool {
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new().read(true).open("/dev/tty").is_ok()
    }
    #[cfg(not(unix))]
    {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    }
}

/// Why the CLI could not obtain the master password, and the way out.
///
/// The redaction hook (`internal.rs`) deliberately keeps its own wording: there
/// the refusal is a policy — a hook must not block a turn on a prompt even when
/// it inherited a terminal — while here it is an observation about this
/// process. Both name `$WS_SECRETS_PASSWORD`, which is the part that matters.
pub const NO_PASSWORD_HELP: &str =
    "$WS_SECRETS_PASSWORD is unset and there is no terminal to prompt on";

pub fn open(ws_name: &str) -> Result<Box<dyn SecretStore>> {
    match selected_backend().as_str() {
        "keyring" => Ok(Box::new(KeyringStore::new(ws_name))),
        "file" => Ok(Box::new(FileStore {
            path: secrets_dir().join(format!("{ws_name}.enc")),
            password: file_password()?,
        })),
        _ => {
            // auto: keyring if a vault is available, else file
            if KeyringStore::available() {
                Ok(Box::new(KeyringStore::new(ws_name)))
            } else {
                Ok(Box::new(FileStore {
                    path: secrets_dir().join(format!("{ws_name}.enc")),
                    password: file_password()?,
                }))
            }
        }
    }
}

pub fn workspace_name() -> Result<String> {
    if let Ok(w) = std::env::var("WS_WORKSPACE") {
        if !w.is_empty() {
            // The result is used as `format!("{ws_name}.enc")` / a keyring
            // service suffix, both of which treat it as a bare path segment.
            // Without this, `WS_WORKSPACE=../../foo` escapes the secrets dir.
            crate::workspace::validate_name(&w)
                .with_context(|| format!("$WS_WORKSPACE={w:?} is not a valid workspace name"))?;
            return Ok(w);
        }
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".ws").is_dir() {
        // The *recorded* name, not the directory's. These differ for a directory
        // adopted under another name, and using the directory name here meant
        // `ws -secrets` read and wrote a different store than every other command
        // named — so a secret stored from inside a session could not be found by
        // `-secrets get` run anywhere else. `workspace.toml` is the single source
        // of truth for what a workspace is called.
        let recorded = crate::meta::read(&cwd.join(".ws/workspace.toml")).name;
        let name = if recorded.is_empty() {
            cwd.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string()
        } else {
            recorded
        };
        if !name.is_empty() {
            crate::workspace::validate_name(&name).with_context(|| {
                format!("{} records an invalid workspace name {name:?}", cwd.display())
            })?;
            return Ok(name);
        }
    }
    bail!("not in a workspace (run inside one, or cd into a workspace dir)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn file_store(dir: &std::path::Path, pw: &str) -> FileStore {
        FileStore { path: dir.join("w.enc"), password: pw.to_string() }
    }

    // `workspace_name_*` tests below resolve through the process-global
    // WS_WORKSPACE env var. `.cargo/config.toml` pins RUST_TEST_THREADS=1
    // today, but this module must not depend on that project-wide default
    // (see registry.rs's TEST_LOCK for the same rationale). Serialize
    // explicitly against other tests in *this* module that touch the var.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn file_roundtrip_and_list_and_remove() {
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "hunter2");
        s.set("API_KEY", "abc123").unwrap();
        s.set("TOKEN", "zzz").unwrap();
        assert_eq!(s.get("API_KEY").unwrap(), Some("abc123".into()));
        assert_eq!(s.get("MISSING").unwrap(), None);
        assert_eq!(s.list().unwrap(), vec!["API_KEY".to_string(), "TOKEN".to_string()]);
        s.remove("API_KEY").unwrap();
        assert_eq!(s.get("API_KEY").unwrap(), None);
        assert_eq!(s.backend_name(), "file");
    }

    #[test]
    fn wrong_password_fails_cleanly() {
        let d = TempDir::new().unwrap();
        file_store(d.path(), "right").set("K", "v").unwrap();
        let err = file_store(d.path(), "wrong").get("K").unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("password")
                || err.to_string().to_lowercase().contains("corrupt")
        );
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let d = TempDir::new().unwrap();
        let needle: &[u8] = b"SUPERSECRETVALUE";
        file_store(d.path(), "pw").set("K", "SUPERSECRETVALUE").unwrap();
        let bytes = std::fs::read(d.path().join("w.enc")).unwrap();
        // Scan windows of exactly the needle length (a length mismatch would make
        // this vacuously pass — the value name and the window width must agree).
        assert_eq!(needle.len(), 16);
        assert!(!bytes.windows(needle.len()).any(|w| w == needle), "plaintext leaked into .enc");
    }

    #[test]
    fn purge_clears_all() {
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "pw");
        s.set("A", "1").unwrap();
        s.set("B", "2").unwrap();
        s.purge().unwrap();
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn set_leaves_the_store_file_owner_only_readable() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "hunter2");
        let path = s.path.clone();

        s.set("FIRST", "one").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the credential file must be owner-only after the first write");

        // A second `set` over an existing file must not loosen the mode back
        // to the temp file's umask-derived default (typically 0644).
        s.set("SECOND", "two").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the credential file must stay owner-only after a second write");
    }

    #[test]
    #[cfg(unix)]
    fn an_unreadable_store_is_never_replaced_by_a_store_with_one_secret() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }

        let d = TempDir::new().unwrap();
        let store = file_store(d.path(), "hunter2");
        let path = store.path.clone();
        store.set("FIRST", "one").unwrap();
        store.set("SECOND", "two").unwrap();
        let before = std::fs::read(&path).unwrap();

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        let result = store.set("THIRD", "three");

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();

        assert!(result.is_err(), "an unreadable store must not be treated as empty");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "and the existing secrets must survive byte-for-byte"
        );
        // and they are still retrievable
        assert_eq!(store.get("FIRST").unwrap().as_deref(), Some("one"));
    }

    /// I1: the keyring index is the *only* record of which names exist in the
    /// OS vault. `remove` is a read-modify-write of it, so an unreadable index
    /// mapped to "empty" made `remove` rewrite the index to hold nothing —
    /// stranding every other secret in the vault with no name left to reach it
    /// by. `remove` reaches this path without touching a real vault (the
    /// `delete_credential` is best-effort), so this is testable everywhere.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_keyring_index_is_never_rewritten_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }

        let d = TempDir::new().unwrap();
        let store =
            KeyringStore { service: "ws:__test__".into(), index: d.path().join("w.keyring-index") };
        let original = "ALPHA\nBETA\nGAMMA\n";
        std::fs::write(&store.index, original).unwrap();

        let mut perms = std::fs::metadata(&store.index).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&store.index, perms).unwrap();

        let removed = store.remove("BETA");
        let listed = store.list();
        let purged = store.purge();

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&store.index).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&store.index, perms).unwrap();

        assert!(removed.is_err(), "remove must not treat an unreadable index as empty");
        assert!(listed.is_err(), "list must not silently under-report");
        assert!(
            purged.is_err(),
            "purge must not report success when it could not enumerate what to purge"
        );
        assert_eq!(
            std::fs::read_to_string(&store.index).unwrap(),
            original,
            "every other secret's name must survive byte-for-byte"
        );
    }

    /// The absent case still has to work: a workspace that has never had a
    /// secret set has no index file, and that is an empty store, not an error.
    #[test]
    fn an_absent_keyring_index_is_an_empty_store() {
        let d = TempDir::new().unwrap();
        let store = KeyringStore {
            service: "ws:__test__".into(),
            index: d.path().join("never-written.keyring-index"),
        };
        assert!(store.list().unwrap().is_empty());
        assert!(
            store.purge().is_ok(),
            "purging a store that was never used is a no-op, not an error"
        );
    }

    // ---------- Task 1 hardening ----------

    /// Task 1 item 2: `purge` must propagate a real removal failure rather
    /// than swallowing it with `.ok()`. Directory write permission is revoked
    /// (not file permission) because unlink requires write access on the
    /// *containing directory*, not the file itself.
    #[test]
    #[cfg(unix)]
    fn purge_propagates_a_removal_failure_instead_of_reporting_success() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats directory permissions — the unlink would succeed.
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }

        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "hunter2");
        s.set("A", "1").unwrap();

        let mut perms = std::fs::metadata(d.path()).unwrap().permissions();
        perms.set_mode(0o500); // r-x: no write on the dir, so remove_file cannot unlink
        std::fs::set_permissions(d.path(), perms).unwrap();

        let result = s.purge();

        // Restore permissions before further assertions so TempDir teardown works.
        let mut perms = std::fs::metadata(d.path()).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(d.path(), perms).unwrap();

        assert!(
            result.is_err(),
            "purge must not report success when the file could not be removed"
        );
        let msg = result.unwrap_err().to_string();
        assert!(!msg.to_lowercase().contains("purged"), "the error must not claim success: {msg}");
        // The strongest evidence purge did not lie: the secret is still there.
        assert_eq!(
            s.get("A").unwrap().as_deref(),
            Some("1"),
            "the secret must still be recoverable since purge did not actually remove the file"
        );
    }

    #[test]
    fn purge_on_absent_file_is_ok() {
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "pw");
        assert!(
            s.purge().is_ok(),
            "purging a store that was never written is a no-op, not an error"
        );
    }

    #[test]
    fn file_store_get_and_remove_reject_invalid_names() {
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "pw");
        s.set("GOOD", "v").unwrap();
        assert!(s.get("bad name!").is_err(), "get must validate the name, same as set");
        assert!(s.remove("bad name!").is_err(), "remove must validate the name, same as set");
        // and a valid name is unaffected
        assert_eq!(s.get("GOOD").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn keyring_store_get_and_remove_reject_invalid_names() {
        let d = TempDir::new().unwrap();
        let store =
            KeyringStore { service: "ws:__test__".into(), index: d.path().join("w.keyring-index") };
        assert!(store.get("bad name!").is_err(), "get must validate the name, same as set");
        assert!(store.remove("bad name!").is_err(), "remove must validate the name, same as set");
    }

    #[test]
    fn workspace_name_rejects_traversal_in_ws_workspace() {
        let _guard = lock();
        std::env::set_var("WS_WORKSPACE", "../../foo");
        let result = workspace_name();
        std::env::remove_var("WS_WORKSPACE");

        let err = result.expect_err("a traversal name must not escape the secrets dir via .enc");
        let msg = err.to_string();
        assert!(msg.contains("WS_WORKSPACE"), "error should name the env var: {msg}");
    }

    #[test]
    fn workspace_name_accepts_a_valid_ws_workspace() {
        let _guard = lock();
        std::env::set_var("WS_WORKSPACE", "my-proj");
        let result = workspace_name();
        std::env::remove_var("WS_WORKSPACE");
        assert_eq!(result.unwrap(), "my-proj");
    }

    /// Task 4 item 1: hook handlers ask this instead of finding out by hanging.
    /// The file backend with no `$WS_SECRETS_PASSWORD` is the one case that
    /// must answer true — it is the default on a headless box, and the whole
    /// reason redaction used to skip silently.
    ///
    /// `auto` is not exercised: it probes the real OS vault, so its answer
    /// depends on the machine the test runs on.
    #[test]
    fn would_prompt_only_when_the_file_backend_has_no_password() {
        let _guard = lock();
        let restore = std::env::var("WS_SECRETS_PASSWORD").ok();

        std::env::set_var("WS_SECRETS_BACKEND", "file");
        std::env::remove_var("WS_SECRETS_PASSWORD");
        let file_no_pw = would_prompt_for_password();

        std::env::set_var("WS_SECRETS_PASSWORD", "hunter2");
        let file_with_pw = would_prompt_for_password();

        // An empty password is not a password.
        std::env::set_var("WS_SECRETS_PASSWORD", "");
        let file_empty_pw = would_prompt_for_password();

        std::env::remove_var("WS_SECRETS_PASSWORD");
        std::env::set_var("WS_SECRETS_BACKEND", "keyring");
        let keyring = would_prompt_for_password();

        std::env::remove_var("WS_SECRETS_BACKEND");
        match restore {
            Some(v) => std::env::set_var("WS_SECRETS_PASSWORD", v),
            None => std::env::remove_var("WS_SECRETS_PASSWORD"),
        }

        assert!(file_no_pw, "file backend with no password must be reported as prompting");
        assert!(!file_with_pw, "a password in the environment means no prompt");
        assert!(file_empty_pw, "an empty WS_SECRETS_PASSWORD is not a password");
        assert!(!keyring, "the OS vault never prompts for a master password");
    }

    /// Task 1 item 1: the keyring index read-modify-write, raced directly
    /// with plain threads and a bare path — no `KeyringStore`, no OS vault, so
    /// this runs the same everywhere `cargo test` does, unlike a test that
    /// depended on `set_password` succeeding against a real (possibly
    /// headless) keychain. Modeled on txn.rs's
    /// `concurrent_read_modify_writes_do_not_lose_updates`: each thread adds
    /// its own unique name; with N threads the index must end up with exactly
    /// N names, or the RMW lost an update.
    #[test]
    fn keyring_index_transaction_does_not_lose_concurrent_adds() {
        use std::sync::{Arc, Barrier};

        let d = TempDir::new().unwrap();
        let index = d.path().join("w.keyring-index");

        const N: usize = 12;
        let barrier = Arc::new(Barrier::new(N));
        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let index = index.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                transact_index(&index, |mut names| {
                    std::thread::yield_now();
                    names.push(format!("NAME{i}"));
                    names
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let final_names = read_index_at(&index).unwrap();
        assert_eq!(
            final_names.len(),
            N,
            "every add must survive; {} were lost to a racing read-modify-write",
            N - final_names.len()
        );
    }
}
