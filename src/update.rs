use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_REPOSITORY: &str = "djock/agent-workspaces";

pub fn run(check: bool, force: bool) -> Result<()> {
    let repository =
        std::env::var("WS_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    let latest = latest_tag(&repository)?;
    let current = env!("CARGO_PKG_VERSION");
    let latest_version = latest.strip_prefix('v').unwrap_or(&latest);

    validate_version(latest_version)?;

    if check {
        if latest_version == current {
            println!("ws {current} is up to date");
        } else {
            println!("update available: ws {current} → {latest_version}");
        }
        return Ok(());
    }

    if latest_version == current && !force {
        println!("ws {current} is already up to date");
        return Ok(());
    }

    let current_exe = std::env::current_exe().context("cannot locate the current ws binary")?;
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the ws install directory"))?;
    let temp = TempDir::new()?;
    let installer = temp.path().join("install.sh");

    run_gh(
        &repository,
        &[
            "release",
            "download",
            &latest,
            "--repo",
            &repository,
            "--pattern",
            "install.sh",
            "--dir",
            temp.path()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("temporary path is not valid UTF-8"))?,
        ],
    )
    .context("failed to download the release installer")?;

    let status = Command::new("sh")
        .arg(&installer)
        .arg("--version")
        .arg(&latest)
        .arg("--install-dir")
        .arg(install_dir)
        .arg("--no-setup")
        .env("WS_REPOSITORY", &repository)
        .status()
        .context("failed to run the release installer")?;
    if !status.success() {
        bail!("release installer exited with {status}");
    }

    let status = Command::new(&current_exe)
        .arg("setup")
        .status()
        .context("updated ws, but failed to refresh hooks and prompts")?;
    if !status.success() {
        bail!("updated ws, but `ws setup` exited with {status}");
    }

    println!("Updated ws {current} → {latest_version}");
    Ok(())
}

fn latest_tag(repository: &str) -> Result<String> {
    let output = run_gh(
        repository,
        &[
            "release", "view", "--repo", repository, "--json", "tagName", "--jq", ".tagName",
        ],
    )
    .context("cannot read the latest GitHub release; run `gh auth login` and try again")?;
    let tag = output.trim();
    if tag.is_empty() {
        bail!("GitHub returned an empty release tag");
    }
    Ok(tag.to_string())
}

fn run_gh(_repository: &str, args: &[&str]) -> Result<String> {
    let gh = std::env::var("WS_GH_BIN").unwrap_or_else(|_| "gh".to_string());
    let output = Command::new(&gh)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `{gh}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`gh {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn validate_version(version: &str) -> Result<()> {
    let core = version.split_once('-').map_or(version, |(v, _)| v);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        bail!("release tag has an unsupported version: {version}");
    }
    Ok(())
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "ws-update-{}-{}",
            std::process::id(),
            crate::now_iso().replace([':', '-'], "")
        ));
        std::fs::create_dir(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::validate_version;

    #[test]
    fn accepts_semantic_release_versions() {
        assert!(validate_version("0.1.0").is_ok());
        assert!(validate_version("1.2.3-beta.1").is_ok());
    }

    #[test]
    fn rejects_malformed_release_versions() {
        for bad in ["", "1", "1.2", "1.2.x", "v1.2.3", "1.2.3.4"] {
            assert!(validate_version(bad).is_err(), "{bad} should be rejected");
        }
    }
}
