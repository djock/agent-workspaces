//! Choices that must come from this machine's user, never from a tracked file.
//!
//! `.ws/workspace.toml` is committed, so a `mode = "loco"` in it reaches every
//! clone. Bypassing an agent's permission checks is a decision for the person
//! running it, so it is recorded here, under ws's own config directory, per
//! workspace root, and only when that person types `-loco`.

use anyhow::Result;
use std::path::{Path, PathBuf};

fn file() -> PathBuf {
    crate::config::ws_config_dir().join("loco-consent")
}

fn key(root: &Path) -> String {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()).display().to_string()
}

fn entries() -> Vec<String> {
    std::fs::read_to_string(file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

pub fn has_loco(root: &Path) -> bool {
    let k = key(root);
    entries().contains(&k)
}

/// Record (`true`) or withdraw (`false`) this machine's consent for `root`.
pub fn set_loco(root: &Path, on: bool) -> Result<()> {
    let k = key(root);
    let mut all = entries();
    let had = all.contains(&k);
    if on == had {
        return Ok(());
    }
    if on {
        all.push(k);
    } else {
        all.retain(|e| *e != k);
    }
    let body: String = all.iter().map(|e| format!("{e}\n")).collect();
    crate::atomic::atomic_write_with_mode(&file(), body, Some(crate::atomic::PRIVATE_FILE))
}
