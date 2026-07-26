use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use argon2::Argon2;
use rand::RngCore;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub trait SecretStore {
    fn set(&self, name: &str, value: &str) -> Result<()>;
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
            Err(e) => return Err(e).with_context(|| format!("failed to read {}", self.path.display())),
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
    fn set(&self, name: &str, value: &str) -> Result<()> {
        validate_name(name)?;
        let mut m = self.load()?;
        m.insert(name.to_string(), value.to_string());
        self.save(&m)
    }
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.load()?.get(name).cloned())
    }
    fn list(&self) -> Result<Vec<String>> {
        Ok(self.load()?.keys().cloned().collect())
    }
    fn remove(&self, name: &str) -> Result<()> {
        let mut m = self.load()?;
        m.remove(name);
        self.save(&m)
    }
    fn purge(&self) -> Result<()> {
        std::fs::remove_file(&self.path).ok();
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "file"
    }
}

// ---------- KeyringStore (OS vault + plaintext names index) ----------

pub struct KeyringStore {
    service: String,      // "ws:<ws_name>"
    index: PathBuf,       // <secrets_dir>/<ws_name>.keyring-index
}

impl KeyringStore {
    pub fn new(ws_name: &str) -> Self {
        KeyringStore {
            service: format!("ws:{ws_name}"),
            index: secrets_dir().join(format!("{ws_name}.keyring-index")),
        }
    }
    /// The index is the only record of *which* names live in the OS vault —
    /// the vault itself cannot be enumerated by service. So the same rule as
    /// `FileStore::load` applies, and harder: absent is a legitimately empty
    /// store, unreadable is not. Mapping a read error to empty made `set`
    /// rewrite the index with only the new name, `remove` rewrite it empty,
    /// and — worst — `purge` delete the index having deleted *nothing* from
    /// the vault, reporting success while stranding every secret behind a
    /// name that no longer exists anywhere.
    fn read_index(&self) -> Result<Vec<String>> {
        match std::fs::read_to_string(&self.index) {
            Ok(s) => Ok(s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", self.index.display())),
        }
    }
    fn write_index(&self, names: &[String]) -> Result<()> {
        let mut v: Vec<&String> = names.iter().collect();
        v.sort();
        v.dedup();
        let body = v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
        crate::atomic::atomic_write(&self.index, body)?;
        Ok(())
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
        let mut names = self.read_index()?;
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
            self.write_index(&names)?;
        }
        Ok(())
    }
    fn get(&self, name: &str) -> Result<Option<String>> {
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
        // Read the index *before* deleting from the vault: if the index is
        // unreadable we cannot rewrite it correctly, and dropping the vault
        // entry first would leave the name listed but unresolvable.
        let names: Vec<String> = self.read_index()?.into_iter().filter(|n| n != name).collect();
        if let Ok(e) = self.entry(name) {
            let _ = e.delete_credential();
        }
        self.write_index(&names)
    }
    fn purge(&self) -> Result<()> {
        // Enumerate first and propagate: deleting the index after failing to
        // read it would destroy the only list of what is still in the vault,
        // while telling the user their secrets are gone.
        let names = self.read_index()?;
        for name in names {
            if let Ok(e) = self.entry(&name) {
                let _ = e.delete_credential();
            }
        }
        std::fs::remove_file(&self.index).ok();
        Ok(())
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
            return Ok(w);
        }
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".ws").is_dir() {
        if let Some(n) = cwd.file_name().and_then(|s| s.to_str()) {
            return Ok(n.to_string());
        }
    }
    bail!("not in a workspace (run inside one, or cd into a workspace dir)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn file_store(dir: &std::path::Path, pw: &str) -> FileStore {
        FileStore { path: dir.join("w.enc"), password: pw.to_string() }
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
        assert!(err.to_string().to_lowercase().contains("password") || err.to_string().to_lowercase().contains("corrupt"));
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
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "plaintext leaked into .enc"
        );
    }

    #[test]
    fn purge_clears_all() {
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "pw");
        s.set("A", "1").unwrap(); s.set("B", "2").unwrap();
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
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

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
        assert_eq!(std::fs::read(&path).unwrap(), before, "and the existing secrets must survive byte-for-byte");
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
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

        let d = TempDir::new().unwrap();
        let store = KeyringStore {
            service: "ws:__test__".into(),
            index: d.path().join("w.keyring-index"),
        };
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
        assert!(purged.is_err(), "purge must not report success when it could not enumerate what to purge");
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
        assert!(store.purge().is_ok(), "purging a store that was never used is a no-op, not an error");
    }
}
