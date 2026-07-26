# ws Phase 5 (Secrets) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Give `ws` a per-workspace secret store — a native OS keyring backend and an encrypted-file backend — with the `ws -secrets` CLI and a write-redaction hook, so agents can stash and use credentials without them ever landing in files, argv, or logs.

**Architecture:** A `SecretStore` trait with two implementations: `KeyringStore` (the `keyring` crate → macOS Keychain / Linux Secret Service / Windows Credential Manager; service = `ws:<workspace>`, account = secret name; a plaintext **names index** file provides `list` since the keyring can't enumerate) and `FileStore` (AES-256-GCM over a serde_json name→value map, key derived from a master password via Argon2id, random salt+nonce per write, atomic file at `~/.config/ws/secrets/<workspace>.enc`). Backend selection is `auto` (probe keyring, fall back to file) / `keyring` / `file`, overridable per-run by `WS_SECRETS_BACKEND` (the test seam). The `set` value is read from **stdin only** — never argv, never logged. A PostToolUse(Write) hook (`ws internal secret-redact`) scans a just-written file for secret patterns, stores the value, and rewrites the literal to a `{{ws:secret:NAME}}` placeholder.

**Tech Stack:** Rust 2021. New deps (verified to resolve): `keyring = "3"`, `aes-gcm = "0.10"`, `argon2 = "0.5"`, `rand = "0.8"`, `rpassword = "7"` (hidden prompt). Existing: serde/serde_json, anyhow, dirs. Dev: assert_cmd, predicates, tempfile.

## Global Constraints

- **Rust 2021, single crate, binary `ws`.** cargo NOT on PATH — prefix every cargo call with `. "$HOME/.cargo/env";` (Rust 1.97.1).
- **SECURITY — non-negotiable:** a secret VALUE must never appear in argv (it comes from stdin), never be written to disk except the encrypted `.enc` or the OS keyring, and never be logged. `set` reads stdin and prints nothing. Only `get`/`export` print values (their sole purpose — consumed inline). Tests must not echo real secrets into assertions beyond what's needed to prove round-trip; use obviously-fake values.
- **New deps (verified resolve, 2026-07-24):** keyring 3.6.3, aes-gcm 0.10.3, argon2 0.5, rand 0.8, rpassword 7. Add to `[dependencies]`. These compile into the one binary (the zero-external-deps rule is about external *binaries*, not Rust crates — consistent with spec §16 which already lists `keyring`).
- **Test determinism:** the keyring backend touches the real OS vault (may prompt / be unavailable in CI), so ALL automated tests use the **file** backend via `WS_SECRETS_BACKEND=file` + `WS_SECRETS_PASSWORD=<fixed>`. The keyring path gets one `#[ignore]`d smoke test (run manually on a machine with a vault) plus unit tests of the names-index logic. Never let a test depend on the real Keychain.
- **File-backend crypto (exact):** file bytes = `salt(16) || nonce(12) || ciphertext`. Key = Argon2id(password, salt) → 32 bytes. AEAD = AES-256-GCM, fresh random salt+nonce on every save. Decrypt failure → a clear "wrong password or corrupt secrets file" error (never a panic). Atomic write (temp + rename).
- **Workspace resolution for `ws -secrets`:** `WS_WORKSPACE` env → else the current dir if it is a workspace (`./.ws` exists or it's registered) → else error "not in a workspace (run inside one, or cd into it)". Secrets are per-workspace: keyring service `ws:<name>`, file `<config>/ws/secrets/<name>.enc`.
- **config:** add `secrets_backend = "auto"` (values auto|keyring|file) to `Config` (get/set/list surface).
- **Redaction hook contract (verified):** PostToolUse Write input has `tool_input.file_path`. The hook reads that file post-write, scans for secret patterns, and on a hit stores the value + rewrites the file + notes it in `.ws/artifacts/MANIFEST.json`. It NEVER blocks the agent (best-effort, exit 0, no stdout needed).
- **Full suite is source of truth:** `. "$HOME/.cargo/env"; cargo test` all-green before each commit (RUST_TEST_THREADS=1 pinned).

---

## File Structure

```
Cargo.toml               # + keyring, aes-gcm, argon2, rand, rpassword
src/secrets.rs           # SecretStore trait, FileStore (crypto), KeyringStore, open(), workspace resolution
src/config.rs            # + secrets_backend field
src/commands.rs          # + secrets(cmd) : set/get/list/rm/purge/export/backend
src/cli.rs               # + Cmd::Secrets(SecretsCmd) parsed from `-secrets <sub> [name]`
src/internal.rs          # + secret_redact handler (PostToolUse Write)
src/hooksetup.rs         # + the redact hook in HOOKS (PostToolUse matcher Write|Edit)
src/main.rs              # route Cmd::Secrets; mod secrets
src/assets/context-template.md   # + one line: store secrets via `ws -secrets`, never in files
tests/secrets.rs         # ws -secrets set/get/list/rm/purge/export/backend (file backend)
tests/redact.rs          # secret-redact hook rewrites a written file + stores the value
```

---

### Task 1: Secrets core — trait, FileStore (crypto), KeyringStore, backend selection

**Files:** `Cargo.toml`, `src/secrets.rs` (new), `src/config.rs`, `src/main.rs` (`mod secrets;`); unit tests in `secrets.rs`.

**Interfaces:**
```rust
pub trait SecretStore {
    fn set(&self, name: &str, value: &str) -> anyhow::Result<()>;
    fn get(&self, name: &str) -> anyhow::Result<Option<String>>;
    fn list(&self) -> anyhow::Result<Vec<String>>;   // sorted names
    fn remove(&self, name: &str) -> anyhow::Result<()>;
    fn purge(&self) -> anyhow::Result<()>;
    fn backend_name(&self) -> &'static str;          // "keyring" | "file"
}
/// Resolve the current workspace name for secrets (WS_WORKSPACE > cwd-workspace > error).
pub fn workspace_name() -> anyhow::Result<String>;
/// Open the store for `ws_name` per the configured/overridden backend.
pub fn open(ws_name: &str) -> anyhow::Result<Box<dyn SecretStore>>;
pub fn secrets_dir() -> std::path::PathBuf;          // <config>/ws/secrets
```

- [ ] **Step 1: Add deps + the failing FileStore unit tests**

Add to `Cargo.toml` `[dependencies]`: `keyring = "3"`, `aes-gcm = "0.10"`, `argon2 = "0.5"`, `rand = "0.8"`, `rpassword = "7"`.

In `src/secrets.rs`:
```rust
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
        file_store(d.path(), "pw").set("K", "SUPERSECRETVALUE").unwrap();
        let bytes = std::fs::read(d.path().join("w.enc")).unwrap();
        assert!(!bytes.windows(15).any(|w| w == b"SUPERSECRETVALUE"), "plaintext leaked into .enc");
    }

    #[test]
    fn purge_clears_all() {
        let d = TempDir::new().unwrap();
        let s = file_store(d.path(), "pw");
        s.set("A", "1").unwrap(); s.set("B", "2").unwrap();
        s.purge().unwrap();
        assert!(s.list().unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test secrets`
Expected: FAIL — module/types missing (and the new deps download on first build).

- [ ] **Step 3: Write secrets.rs**

`src/secrets.rs`:
```rust
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use argon2::Argon2;
use rand::RngCore;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
            Err(_) => return Ok(BTreeMap::new()),
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
        Ok(serde_json::from_slice(&pt).context("corrupt secrets file (bad json)")?)
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
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("enc.tmp");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, &self.path)?;
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
    fn read_index(&self) -> Vec<String> {
        std::fs::read_to_string(&self.index)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect())
            .unwrap_or_default()
    }
    fn write_index(&self, names: &[String]) -> Result<()> {
        if let Some(d) = self.index.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut v: Vec<&String> = names.iter().collect();
        v.sort();
        v.dedup();
        let body = v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
        let tmp = self.index.with_extension("index.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.index)?;
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
        let mut names = self.read_index();
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
        let mut v = self.read_index();
        v.sort();
        Ok(v)
    }
    fn remove(&self, name: &str) -> Result<()> {
        if let Ok(e) = self.entry(name) {
            let _ = e.delete_credential();
        }
        let names: Vec<String> = self.read_index().into_iter().filter(|n| n != name).collect();
        self.write_index(&names)
    }
    fn purge(&self) -> Result<()> {
        for name in self.read_index() {
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
```
Add `secrets_backend: String` to `Config` (default `"auto"`), to `Default`, to `list()`, and to `set()`'s match (see Task-5 config precedent). Add `mod secrets;` to `main.rs`.

**KEYRING API NOTE (verify at impl):** keyring 3's delete method is `delete_credential()` and the missing-entry error is `keyring::Error::NoEntry`. Confirm these names compile against the resolved keyring 3.6 (`cargo doc -p keyring` / compiler); adjust if the exact identifiers differ.

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test secrets`
Expected: PASS (the four FileStore unit tests; KeyringStore compiles but isn't exercised here).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/secrets.rs src/config.rs src/main.rs
git commit -m "feat: secrets core — FileStore (AES-256-GCM/Argon2id) + KeyringStore + backend selection"
```

---

### Task 2: `ws -secrets` CLI

**Files:** `src/cli.rs` (`Cmd::Secrets(SecretsCmd)`), `src/commands.rs` (`secrets`), `src/main.rs` (route); `tests/secrets.rs`.

**Interfaces:**
```rust
// cli.rs
pub enum SecretsCmd { Set(String), Get(String), List, Rm(String), Purge, Export, Backend }
// parsed from: ws -secrets set|get|rm <name> | list | purge | export | backend
pub fn secrets(cmd: SecretsCmd) -> anyhow::Result<()>;   // in commands.rs
```
- `set NAME` reads the value from **stdin** (trailing newline trimmed), stores it, prints nothing sensitive (a `stored NAME` confirmation only). `get NAME` prints the value (or errors if absent). `list` prints names (never values). `rm NAME`. `purge` prompts `y/N` unless not a TTY (then require nothing? no — purge is destructive: require a TTY confirm, or `--force`; for the CLI keep it simple: confirm on TTY, refuse without TTY unless forced — mirror `-rm`). `export` prints `export NAME=<value>` lines for all secrets. `backend` prints the active backend name.

- [ ] **Step 1: Write the failing tests**

`tests/secrets.rs` (all via the file backend so no real Keychain is touched):
```rust
mod common;
use common::Env;

fn sc<'a>(env: &Env) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "testpw")
        .env("WS_WORKSPACE", "sw");
    c
}

#[test]
fn set_from_stdin_get_list_rm() {
    let env = Env::new();
    sc(&env).args(["-secrets", "set", "API_KEY"]).write_stdin("s3cr3t\n").assert().success();
    // value never echoed by set
    sc(&env).args(["-secrets", "get", "API_KEY"]).assert().success()
        .stdout(predicates::str::diff("s3cr3t\n"));
    sc(&env).args(["-secrets", "list"]).assert().success()
        .stdout(predicates::str::contains("API_KEY"))
        .stdout(predicates::str::contains("s3cr3t").not());  // list never shows values
    sc(&env).args(["-secrets", "rm", "API_KEY"]).assert().success();
    sc(&env).args(["-secrets", "get", "API_KEY"]).assert().failure(); // absent
}

#[test]
fn export_and_backend() {
    let env = Env::new();
    sc(&env).args(["-secrets", "set", "TOKEN"]).write_stdin("abc").assert().success();
    sc(&env).args(["-secrets", "export"]).assert().success()
        .stdout(predicates::str::contains("export TOKEN=abc"));
    sc(&env).args(["-secrets", "backend"]).assert().success()
        .stdout(predicates::str::contains("file"));
}

#[test]
fn secrets_outside_workspace_errors() {
    let env = Env::new();
    // no WS_WORKSPACE and cwd isn't a workspace
    env.cmd().env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","x")
        .args(["-secrets","list"]).assert().failure()
        .stderr(predicates::str::contains("not in a workspace"));
}
```
Add `use predicates::prelude::*;` for `.not()`.

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test secrets`
Expected: FAIL — `-secrets` unhandled.

- [ ] **Step 3: Implement**

`src/cli.rs` — add `Secrets(SecretsCmd)` to `Cmd` and `SecretsCmd`; in the leading-dash section add `"-secrets" => parse_secrets(it.collect())`:
```rust
fn parse_secrets(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it.next().unwrap_or_default();
    let cmd = match sub.as_str() {
        "set" => SecretsCmd::Set(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets set <name>"))?),
        "get" => SecretsCmd::Get(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets get <name>"))?),
        "rm" => SecretsCmd::Rm(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets rm <name>"))?),
        "list" => SecretsCmd::List,
        "purge" => SecretsCmd::Purge,
        "export" => SecretsCmd::Export,
        "backend" => SecretsCmd::Backend,
        other => bail!("unknown -secrets subcommand: {other}"),
    };
    Ok(Cmd::Secrets(cmd))
}
```
`src/commands.rs`:
```rust
use crate::cli::SecretsCmd;
use crate::secrets;
use std::io::Read;

pub fn secrets(cmd: SecretsCmd) -> Result<()> {
    let ws = secrets::workspace_name()?;
    let store = secrets::open(&ws)?;
    match cmd {
        SecretsCmd::Set(name) => {
            let mut value = String::new();
            std::io::stdin().read_to_string(&mut value)?;
            let value = value.strip_suffix('\n').unwrap_or(&value);
            store.set(&name, value)?;
            println!("stored {name}");   // never echoes the value
        }
        SecretsCmd::Get(name) => match store.get(&name)? {
            Some(v) => println!("{v}"),
            None => anyhow::bail!("no such secret: {name}"),
        },
        SecretsCmd::List => {
            for n in store.list()? {
                println!("{n}");
            }
        }
        SecretsCmd::Rm(name) => {
            store.remove(&name)?;
            println!("removed {name}");
        }
        SecretsCmd::Purge => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                eprint!("Purge ALL secrets for workspace {ws}? [y/N] ");
                use std::io::Write;
                std::io::stderr().flush().ok();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    println!("cancelled");
                    return Ok(());
                }
            } else {
                anyhow::bail!("refusing to purge without a TTY to confirm");
            }
            store.purge()?;
            println!("purged all secrets for {ws}");
        }
        SecretsCmd::Export => {
            for n in store.list()? {
                if let Some(v) = store.get(&n)? {
                    println!("export {n}={v}");
                }
            }
        }
        SecretsCmd::Backend => println!("{}", store.backend_name()),
    }
    Ok(())
}
```
Route in `main.rs`: `Cmd::Secrets(c) => commands::secrets(c)?`.

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test secrets; cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/commands.rs src/main.rs tests/secrets.rs
git commit -m "feat: ws -secrets CLI (set-from-stdin/get/list/rm/purge/export/backend)"
```

---

### Task 3: Write-redaction hook

**Files:** `src/internal.rs` (`secret_redact` handler), `src/hooksetup.rs` (add the hook to `HOOKS`), `src/assets/context-template.md` (one line), `tests/redact.rs`.

**Interfaces / behavior:** `ws internal secret-redact` — a PostToolUse hook (matcher `Write|Edit`). No-op unless in a ws workspace. Reads `tool_input.file_path`; if that file exists, scans each line for a secret assignment matching a pattern (a `NAME=VALUE` where NAME matches `.*(KEY|TOKEN|SECRET|PASSWORD|PASSWD|API).*` case-insensitively, or the file is a `.env`, and VALUE is non-empty and not already a `{{ws:secret:...}}` placeholder). On a hit: store the value under NAME in the workspace secret store, replace the literal value with `{{ws:secret:NAME}}` in the file (atomic rewrite), and append a note to `.ws/artifacts/MANIFEST.json`. Best-effort, exit 0, no stdout.

- [ ] **Step 1: Write the failing test**

`tests/redact.rs`:
```rust
mod common;
use common::Env;

fn adopt(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    env.cmd().current_dir(&p).args(["-adopt", name]).assert().success();
    p
}

#[test]
fn redact_rewrites_secret_and_stores_it() {
    let env = Env::new();
    let proj = adopt(&env, "rp");
    // an agent just wrote a .env with a secret
    let envfile = proj.join(".env");
    std::fs::write(&envfile, "PORT=8080\nAPI_KEY=supersecret123\n").unwrap();

    let payload = format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, envfile.display());
    env.cmd()
        .env("WS_WORKSPACE", "rp").env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file").env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    // file rewritten: the literal is gone, a placeholder took its place; PORT untouched
    let after = std::fs::read_to_string(&envfile).unwrap();
    assert!(!after.contains("supersecret123"), "secret literal must be gone");
    assert!(after.contains("API_KEY={{ws:secret:API_KEY}}"), "placeholder must be present: {after}");
    assert!(after.contains("PORT=8080"), "non-secret untouched");

    // the value is retrievable from the store
    env.cmd().env("WS_WORKSPACE","rp").env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","pw")
        .args(["-secrets","get","API_KEY"]).assert().success()
        .stdout(predicates::str::diff("supersecret123\n"));
}

#[test]
fn redact_ignores_non_secret_files() {
    let env = Env::new();
    let proj = adopt(&env, "rp2");
    let f = proj.join("notes.txt");
    std::fs::write(&f, "just some text PORT=1\n").unwrap();
    let payload = format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, f.display());
    env.cmd().env("WS_WORKSPACE","rp2").env("WS_DIR",&proj)
        .env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","pw")
        .args(["internal","secret-redact"]).write_stdin(payload).assert().success();
    // unchanged (no secret-looking assignment)
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "just some text PORT=1\n");
}
```

- [ ] **Step 2: Run to verify fail**

Run: `. "$HOME/.cargo/env"; cargo test --test redact`
Expected: FAIL — handler not dispatched.

- [ ] **Step 3: Implement the handler + register the hook**

FIRST, add the `file_path` field to `hookio::ToolInput` (Phase-2 only had `command`), so `h.tool_input.file_path` compiles:
```rust
#[derive(Debug, Default, Deserialize)]
pub struct ToolInput {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub file_path: String,
}
```

In `src/internal.rs`, add the dispatch arm `"secret-redact" => secret_redact(),` and:
```rust
fn secret_redact() {
    let h = hookio::read_stdin();
    let ws = match current_ws() {
        Some(w) => w,
        None => return,
    };
    if h.tool_name != "Write" && h.tool_name != "Edit" {
        return;
    }
    let path = std::path::PathBuf::from(&h.tool_input.file_path);
    if h.tool_input.file_path.is_empty() || !path.is_file() {
        return;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return,
    };
    let is_env = path.file_name().and_then(|s| s.to_str()).map_or(false, |n| n == ".env" || n.ends_with(".env"));

    let store = match crate::secrets::open(&ws.name) {
        Ok(s) => s,
        Err(_) => return, // e.g. file backend without a password → skip silently
    };

    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    let mut redacted: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some((name, value)) = parse_assignment(line) {
            let looks_secret = is_env || name_looks_secret(name);
            let already = value.starts_with("{{ws:secret:");
            if looks_secret && !already && !value.is_empty() {
                if store.set(name, value).is_ok() {
                    out.push_str(name);
                    out.push_str("={{ws:secret:");
                    out.push_str(name);
                    out.push_str("}}");
                    out.push('\n');
                    redacted.push(name.to_string());
                    changed = true;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if changed {
        // preserve trailing-newline shape roughly; atomic rewrite
        let tmp = path.with_extension("redact.tmp");
        if std::fs::write(&tmp, &out).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        let _ = note_manifest(&ws, &redacted);
    }
}

/// A `NAME=VALUE` line (VALUE may be quoted). Returns (name, unquoted value) or None.
fn parse_assignment(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    if t.starts_with('#') {
        return None;
    }
    let (name, rest) = t.split_once('=')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let value = rest.trim().trim_matches('"').trim_matches('\'');
    Some((name, value))
}

fn name_looks_secret(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "API"].iter().any(|k| u.contains(k))
}

fn note_manifest(ws: &crate::workspace::Workspace, names: &[String]) -> std::io::Result<()> {
    let path = ws.ws_dir().join("artifacts").join("MANIFEST.json");
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut val: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !val.is_object() {
        val = serde_json::json!({});
    }
    let arr = val.as_object_mut().unwrap().entry("redacted_secrets").or_insert_with(|| serde_json::json!([]));
    if let Some(a) = arr.as_array_mut() {
        for n in names {
            a.push(serde_json::json!({ "name": n, "at": crate::now_iso() }));
        }
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&val)?)?;
    std::fs::rename(&tmp, &path)
}
```
Register the hook: in `src/hooksetup.rs` `HOOKS`, add
```rust
    HookSpec { event: "PostToolUse", matcher: Some("Write|Edit"), handler: "secret-redact", script: "secret-redact.sh" },
```
Add one line to `src/assets/context-template.md` (inside the block): `- Store secrets with `ws -secrets set NAME` (value on stdin); never paste credentials into files — the redaction hook will replace any it catches with `{{ws:secret:NAME}}`.`

- [ ] **Step 4: Run to verify pass**

Run: `. "$HOME/.cargo/env"; cargo test --test redact; cargo test`
Expected: PASS. (Note: adding a hook to `HOOKS` means the Phase-2/3/4 setup tests that count hooks may assert a specific count — update any exact-count assertion to the new number, or assert on presence not count.)

- [ ] **Step 5: Commit**

```bash
git add src/internal.rs src/hooksetup.rs src/assets/context-template.md tests/redact.rs
git commit -m "feat: write-redaction hook — store secrets, replace literals with {{ws:secret:NAME}}"
```

---

## Self-Review

**Spec coverage (§11 / §17.5):**
- keyring backend (Keychain/Secret Service, service `ws:<workspace>`) — Task 1 ✓ (+ names index for `list`)
- file backend (encrypted, `~/.config/ws/secrets/<workspace>.enc`, password from `WS_SECRETS_PASSWORD`/prompt) — Task 1 ✓ (AES-256-GCM + Argon2id)
- `auto` probe + fallback; `backend` reports active — Task 1 + Task 2 ✓
- CLI set (stdin, never argv) / get / list / rm / purge (confirm) / export — Task 2 ✓
- redaction hook (patterns → store → `{{ws:secret:NAME}}` → MANIFEST.json) — Task 3 ✓
- context files instruct proactive `ws -secrets` use — Task 3 ✓

**Security review:** `set` reads stdin only (never argv → never in the bash-audit log); `set`/`list` never print values; encryption uses a fresh salt+nonce per write and a clean decrypt-failure error (no panic); the redaction hook is best-effort and never blocks the agent; the plaintext-leak unit test proves the `.enc` doesn't contain the value.

**Deferred (correctly out of Phase 5):** cs's age-key export/import cross-machine sync (spec explicitly out of scope); per-secret metadata/rotation; secret sharing between workspaces; Windows-specific keyring testing.

**Verify-at-impl flags:** the exact keyring 3.6 identifiers (`delete_credential`, `Error::NoEntry`) — confirm they compile and adjust if renamed. rpassword's `prompt_password` signature — confirm (7.x returns `io::Result<String>`).

**Type consistency:** `SecretStore` is implemented by `FileStore` and `KeyringStore`; `secrets::{open, workspace_name, secrets_dir}` are consumed by `commands::secrets` and `internal::secret_redact`. `SecretsCmd` (cli.rs) is consumed by `commands::secrets`. `hookio::HookInput.tool_input.file_path` is used by `secret_redact` — CONFIRM `ToolInput` has a `file_path` field; the Phase-2 `ToolInput` only had `command`. **Task 3 must add `#[serde(default)] pub file_path: String` to `hookio::ToolInput`** (call this out in the Task 3 implementation — it's required for `h.tool_input.file_path` to compile).
