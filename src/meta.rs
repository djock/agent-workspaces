//! Typed, clobber-safe access to `.ws/workspace.toml`.
//!
//! Reads produce a `Meta`; writes go through a `toml::Table` round-trip so keys
//! this version of `ws` doesn't know about survive, and a file that fails to
//! parse is never overwritten.
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Meta {
    pub name: String,
    pub created: String,
    pub contract_version: u32,
    pub default_agent: Option<String>,
    pub archived: bool,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub color: Option<String>,
}

fn table(ws_toml: &Path) -> Option<toml::Table> {
    toml::from_str(&std::fs::read_to_string(ws_toml).ok()?).ok()
}

fn from_table(t: &toml::Table) -> Meta {
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(String::from);
    Meta {
        name: s("name").unwrap_or_default(),
        created: s("created").unwrap_or_default(),
        contract_version: t.get("contract_version").and_then(|v| v.as_integer()).unwrap_or(0) as u32,
        default_agent: s("default_agent"),
        archived: t.get("archived").and_then(|v| v.as_bool()).unwrap_or(false),
        tags: t
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        status: s("status"),
        color: s("color"),
    }
}

/// Read workspace metadata, surfacing failure instead of hiding it.
/// `Ok(None)` = the file does not exist; `Err` = it exists but could not be
/// read or parsed. Callers that walk many workspaces and want to tolerate a
/// half-built one keep using `read()`.
pub fn read_checked(ws_toml: &Path) -> Result<Option<Meta>> {
    let body = match std::fs::read_to_string(ws_toml) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", ws_toml.display())),
    };
    let t: toml::Table = toml::from_str(&body)
        .with_context(|| format!("{} is corrupt", ws_toml.display()))?;
    Ok(Some(from_table(&t)))
}

/// Read workspace metadata. A missing or unparseable file reads as defaults —
/// listing commands walk many workspaces and must tolerate a half-built one.
pub fn read(ws_toml: &Path) -> Meta {
    match table(ws_toml) {
        Some(t) => from_table(&t),
        None => Meta::default(),
    }
}

/// Apply `f` to the parsed table and write it back atomically.
/// Bails if the file exists but cannot be parsed — we never clobber a file we
/// don't understand.
pub fn update(ws_toml: &Path, f: impl FnOnce(&mut toml::Table)) -> Result<()> {
    let mut t = match std::fs::read_to_string(ws_toml) {
        Ok(s) => toml::from_str(&s).with_context(|| {
            format!(
                "{} is corrupt (refusing to overwrite)",
                ws_toml.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).context("failed to read workspace.toml"),
    };
    f(&mut t);
    crate::atomic::atomic_write(ws_toml, toml::to_string_pretty(&t)?)?;
    Ok(())
}

fn write_tags(t: &mut toml::Table, tags: &[String]) {
    t.insert(
        "tags".into(),
        toml::Value::Array(tags.iter().map(|s| toml::Value::String(s.clone())).collect()),
    );
}

/// Add tags (deduped, sorted). Returns the resulting full tag list.
pub fn add_tags(ws_toml: &Path, tags: &[String]) -> Result<Vec<String>> {
    let mut all = read(ws_toml).tags;
    for tag in tags {
        if !all.iter().any(|t| t == tag) {
            all.push(tag.clone());
        }
    }
    all.sort();
    all.dedup();
    let out = all.clone();
    update(ws_toml, |t| write_tags(t, &all))?;
    Ok(out)
}

/// Remove tags. Removing a tag that isn't there is not an error.
pub fn remove_tags(ws_toml: &Path, tags: &[String]) -> Result<Vec<String>> {
    let mut all = read(ws_toml).tags;
    all.retain(|t| !tags.iter().any(|r| r == t));
    let out = all.clone();
    update(ws_toml, |t| write_tags(t, &all))?;
    Ok(out)
}

/// Set the one-line status text; `None` clears it (removes the key).
pub fn set_status(ws_toml: &Path, text: Option<&str>) -> Result<()> {
    let text = text.map(str::to_string);
    update(ws_toml, |t| match text {
        Some(s) => {
            t.insert("status".into(), toml::Value::String(s));
        }
        None => {
            t.remove("status");
        }
    })
}

pub fn set_archived(ws_toml: &Path, archived: bool) -> Result<()> {
    update(ws_toml, |t| {
        t.insert("archived".into(), toml::Value::Boolean(archived));
    })
}

pub fn set_default_agent(ws_toml: &Path, agent: &str) -> Result<()> {
    let agent = agent.to_string();
    update(ws_toml, |t| {
        t.insert("default_agent".into(), toml::Value::String(agent));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_checked_distinguishes_missing_from_corrupt() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("workspace.toml");

        // missing → Ok(None)
        assert!(read_checked(&p).unwrap().is_none());

        // valid → Ok(Some)
        std::fs::write(&p, "name = \"alpha\"\narchived = true\n").unwrap();
        let m = read_checked(&p).unwrap().expect("parsed");
        assert_eq!(m.name, "alpha");
        assert!(m.archived);

        // corrupt → Err, and read() still degrades to defaults for existing callers
        std::fs::write(&p, "this is not toml {{{").unwrap();
        assert!(read_checked(&p).is_err(), "corrupt workspace.toml must not read as missing");
        assert_eq!(read(&p), Meta::default());
    }

    fn wt(contents: &str) -> (TempDir, std::path::PathBuf) {
        let d = TempDir::new().unwrap();
        let p = d.path().join("workspace.toml");
        std::fs::write(&p, contents).unwrap();
        (d, p)
    }

    #[test]
    fn read_full_and_missing() {
        let (_d, p) = wt(
            "name = \"proj\"\ncreated = \"2026-07-24T10:00:00Z\"\ncontract_version = 1\n\
             default_agent = \"codex\"\narchived = true\ntags = [\"rust\", \"cli\"]\n\
             status = \"waiting on review\"\ncolor = \"blue\"\n",
        );
        let m = read(&p);
        assert_eq!(m.name, "proj");
        assert_eq!(m.contract_version, 1);
        assert_eq!(m.default_agent.as_deref(), Some("codex"));
        assert!(m.archived);
        assert_eq!(m.tags, vec!["rust".to_string(), "cli".to_string()]);
        assert_eq!(m.status.as_deref(), Some("waiting on review"));
        assert_eq!(m.color.as_deref(), Some("blue"));

        // A missing file reads as defaults, not an error — callers list many
        // workspaces and must tolerate a half-built one.
        let missing = read(std::path::Path::new("/nope/workspace.toml"));
        assert_eq!(missing.name, "");
        assert!(!missing.archived);
        assert!(missing.tags.is_empty());
    }

    #[test]
    fn update_preserves_unknown_keys() {
        let (_d, p) = wt("name = \"proj\"\nfuture_key = \"keep me\"\ntags = [\"a\"]\n");
        set_status(&p, Some("shipping")).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("future_key"), "unknown keys must survive a write: {s}");
        assert!(s.contains("shipping"));
        assert_eq!(read(&p).tags, vec!["a".to_string()]);
    }

    #[test]
    fn update_refuses_to_clobber_corrupt_file() {
        let (_d, p) = wt("this is not toml {{{");
        assert!(set_archived(&p, true).is_err());
        // The corrupt file is still there, byte for byte.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "this is not toml {{{");
    }

    #[test]
    fn tags_add_dedupes_and_sorts_remove_is_idempotent() {
        let (_d, p) = wt("name = \"proj\"\n");
        let after = add_tags(&p, &["rust".into(), "cli".into(), "rust".into()]).unwrap();
        assert_eq!(after, vec!["cli".to_string(), "rust".to_string()]);
        // adding an existing tag is a no-op, not a duplicate
        let after = add_tags(&p, &["cli".into()]).unwrap();
        assert_eq!(after, vec!["cli".to_string(), "rust".to_string()]);
        let after = remove_tags(&p, &["cli".into(), "never-there".into()]).unwrap();
        assert_eq!(after, vec!["rust".to_string()]);
    }

    #[test]
    fn status_clear_removes_the_key_entirely() {
        let (_d, p) = wt("name = \"proj\"\nstatus = \"busy\"\n");
        set_status(&p, None).unwrap();
        assert_eq!(read(&p).status, None);
        assert!(!std::fs::read_to_string(&p).unwrap().contains("status"));
    }

    #[test]
    fn archived_and_default_agent_roundtrip() {
        let (_d, p) = wt("name = \"proj\"\narchived = false\n");
        set_archived(&p, true).unwrap();
        assert!(read(&p).archived);
        set_archived(&p, false).unwrap();
        assert!(!read(&p).archived);
        set_default_agent(&p, "codex").unwrap();
        assert_eq!(read(&p).default_agent.as_deref(), Some("codex"));
    }

    #[test]
    fn update_creates_the_file_when_absent() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("workspace.toml");
        set_status(&p, Some("new")).unwrap();
        assert_eq!(read(&p).status.as_deref(), Some("new"));
    }

    #[test]
    #[cfg(unix)]
    fn update_refuses_when_an_existing_file_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let (_d, p) = wt("name = \"proj\"\nstatus = \"keep me\"\n");
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&p, perms).unwrap();

        // Unreadable ≠ absent: with the pre-fix code the read error was
        // swallowed, an empty table was written to a temp file, and the rename
        // succeeded — silently destroying the contents. update() must refuse.
        let result = set_archived(&p, true);

        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&p, perms).unwrap();

        assert!(result.is_err(), "update must not treat an unreadable file as absent");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "name = \"proj\"\nstatus = \"keep me\"\n",
            "the original file must survive untouched"
        );
    }
}
