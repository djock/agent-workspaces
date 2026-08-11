use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_REPOSITORY: &str = "djock/agent-workspaces";

/// How long a recorded release lookup is trusted before `notify` asks GitHub
/// again. A launch must not pay for a network round trip every time.
const CHECK_INTERVAL_SECS: u64 = 3600;

/// Written into the cache when the lookup failed. The *timestamp* is what makes
/// a failure useful: it backs the retry off for the same hour a success would,
/// so a machine with no `gh` (or no network) does not re-pay the timeout on
/// every single launch.
const UNKNOWN: &str = "-";

/// How many releases the launch notice lists before collapsing the rest into
/// "… and N earlier versions". A launch card is a nudge, not a changelog.
const NOTES_CAP: usize = 5;

/// Widest a single headline may render. Fixed rather than terminal-derived: the
/// notes are cached, so a line written in a wide terminal would otherwise be
/// replayed into a narrow one for the rest of the hour.
const SUMMARY_WIDTH: usize = 68;

/// Stands in for a version on the "and N earlier" tail line.
const MORE: &str = "+";

pub fn run(check: bool, force: bool) -> Result<()> {
    let repository =
        std::env::var("WS_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    let latest = latest_tag(&repository)?;
    let current = env!("CARGO_PKG_VERSION");
    let latest_version = latest.strip_prefix('v').unwrap_or(&latest);

    validate_version(latest_version)?;
    // An explicit check is the freshest answer there is; record it so the next
    // launch reports the same thing rather than asking again.
    write_cache(latest_version);

    if check {
        if latest_version == current {
            println!("ws {current} is up to date");
        } else {
            println!("update available: ws {current} → {latest_version}");
            for (version, summary) in notes(latest_version, current) {
                if version == MORE {
                    println!("  {summary}");
                } else {
                    println!("  {version}  {summary}");
                }
            }
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

/// Print "there is a newer ws" on launch, the way `cs` does on session open.
///
/// Deliberately infallible: a launch is not the place to fail over a release
/// lookup, so every error path here degrades to silence. Set `WS_NO_UPDATE_CHECK`
/// to opt out of the network call and the cache write entirely (the test suite
/// does, so a launch never reaches GitHub).
pub fn notify() {
    if std::env::var_os("WS_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let current = env!("CARGO_PKG_VERSION");
    let latest = match cached() {
        Cached::Known(v) => v,
        // Fresh, but the last lookup failed: nothing to report until it ages out.
        Cached::Unknown => return,
        Cached::Stale => {
            let repository =
                std::env::var("WS_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
            let fetched = latest_tag(&repository)
                .ok()
                .map(|tag| tag.strip_prefix('v').unwrap_or(&tag).to_string())
                .filter(|v| validate_version(v).is_ok());
            write_cache(fetched.as_deref().unwrap_or(UNKNOWN));
            match fetched {
                Some(v) => v,
                None => return,
            }
        }
    };

    if !version_greater(&latest, current) {
        return;
    }
    let (y, dim, off) = if std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
    {
        ("\x1b[33m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    println!(
        "{y}▌{off} {y}Update available:{off} {current} {dim}→{off} {latest} {dim}(ws -update){off}"
    );
    for (version, summary) in notes(&latest, current) {
        if version == MORE {
            println!("{y}▌{off}   {dim}{summary}{off}");
        } else {
            println!("{y}▌{off}   {version}  {dim}{summary}{off}");
        }
    }
}

/// One headline per release you would be getting, newest first, read from the
/// published `CHANGELOG.md`.
///
/// Cached per pending version in `update-notes-<version>` beside the release
/// cache, because it changes only when `latest` does. An *empty* file is the
/// record of a failed fetch: without it a launch would re-ask GitHub for the
/// changelog every time the notice printed. Silent on every failure, like the
/// notice itself.
fn notes(latest: &str, installed: &str) -> Vec<(String, String)> {
    let path = notes_cache_path(latest);
    if !path.exists() {
        let repository =
            std::env::var("WS_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
        let body = fetch_changelog(&repository)
            .map(|md| {
                summarize(&md, installed, NOTES_CAP)
                    .into_iter()
                    .map(|(v, s)| format!("{v}\t{s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if crate::atomic::atomic_write(&path, body).is_err() {
            return Vec::new();
        }
        prune_stale_notes(&path);
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(v, s)| (v.to_string(), s.to_string()))
        .collect()
}

/// Only the pending version's notes are worth keeping; every other
/// `update-notes-*` file describes an update that is no longer the one on offer.
fn prune_stale_notes(keep: &Path) {
    let Some(dir) = keep.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_notes = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("update-notes-"));
        if is_notes && path != keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn fetch_changelog(repository: &str) -> Option<String> {
    let md = run_gh(
        repository,
        &[
            "api",
            &format!("repos/{repository}/contents/CHANGELOG.md"),
            "-H",
            "Accept: application/vnd.github.raw",
        ],
    )
    .ok()?;
    (!md.trim().is_empty()).then_some(md)
}

/// Reduce a keep-a-changelog file to one headline per release newer than
/// `installed`, newest first: the version, and the first sentence of its first
/// entry. Stops at the first heading that is not newer — the file is ordered, so
/// everything below it is already installed.
///
/// Pure, so the parsing is testable without a network or a cache. The `MORE`
/// marker (rather than a formatted line) keeps the "and N earlier" tail
/// something the caller renders, since only the caller knows about colour.
fn summarize(changelog: &str, installed: &str, cap: usize) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    let mut want: Option<String> = None;
    // A changelog bullet wraps across lines; the entry is the bullet plus every
    // continuation line under it, or the summary gets cut mid-clause.
    let mut entry = String::new();

    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if let (Some(version), Some(text)) = (want.take(), first_sentence(&entry)) {
                out.push((version, text));
            }
            entry.clear();
            let version = heading.split_whitespace().next().unwrap_or("").trim_matches(['[', ']']);
            // `[Unreleased]` heads the file and is not a release; skip it rather
            // than letting it parse as 0.0.0 and end the scan immediately.
            if version.eq_ignore_ascii_case("unreleased") {
                continue;
            }
            if !version_greater(version, installed) {
                break;
            }
            total += 1;
            if total <= cap {
                want = Some(version.to_string());
            }
            continue;
        }
        if want.is_none() || line.starts_with("### ") || line.trim().is_empty() {
            continue;
        }
        if line.starts_with("- ") || line.starts_with("* ") {
            if !entry.is_empty() {
                continue; // the first entry is the headline; later ones are not
            }
            entry.push_str(line[2..].trim());
        } else if !entry.is_empty() && line.starts_with(' ') {
            entry.push(' ');
            entry.push_str(line.trim());
        }
    }
    if let (Some(version), Some(text)) = (want.take(), first_sentence(&entry)) {
        out.push((version, text));
    }
    if total > cap {
        let n = total - cap;
        let plural = if n == 1 { "" } else { "s" };
        out.push((MORE.to_string(), format!("… and {n} earlier version{plural}")));
    }
    out
}

/// The first sentence of a markdown entry, as plain text. One sentence is the
/// unit that reliably says what changed without wrapping the terminal.
fn first_sentence(entry: &str) -> Option<String> {
    let text = strip_markdown(entry);
    if text.is_empty() {
        return None;
    }
    // ". " and not '.', so `0.4.0` and `~/.cache/ws` do not end the sentence.
    let end = text.find(". ").map(|i| i + 1).unwrap_or(text.len());
    let mut sentence = text[..end].trim().to_string();
    if sentence.chars().count() > SUMMARY_WIDTH {
        sentence =
            sentence.chars().take(SUMMARY_WIDTH - 1).collect::<String>().trim_end().to_string();
        sentence.push('…');
    }
    Some(sentence)
}

/// Inline markdown to plain text: `**bold**`, `` `code` ``, and `[text](url)`
/// all become their text. Enough for changelog prose — this is not a parser.
fn strip_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '`' | '_' => continue,
            '[' => {
                // `[text](url)` → `text`; a bare `[` is left alone.
                let text: String = chars.by_ref().take_while(|&c| c != ']').collect();
                out.push_str(&text);
                if chars.peek() == Some(&'(') {
                    for c in chars.by_ref() {
                        if c == ')' {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

enum Cached {
    /// Looked up recently enough to trust, and it named a release.
    Known(String),
    /// Looked up recently enough to trust, and the lookup had failed.
    Unknown,
    /// Missing, unreadable, malformed, or aged out — ask GitHub again.
    Stale,
}

fn cached() -> Cached {
    let Ok(raw) = std::fs::read_to_string(cache_path()) else {
        return Cached::Stale;
    };
    let Some((stamp, version)) = raw.trim().split_once(char::is_whitespace) else {
        return Cached::Stale;
    };
    let Ok(stamp) = stamp.parse::<u64>() else {
        return Cached::Stale;
    };
    // `checked_sub`, because a cache written under a clock that has since been
    // moved back would otherwise wrap to a huge age and look permanently fresh.
    match now_secs().checked_sub(stamp) {
        Some(age) if age < CHECK_INTERVAL_SECS => {}
        _ => return Cached::Stale,
    }
    // Validated on the way *out* as well as in: this value names a cache file
    // (`update-notes-<version>`), and a cache anyone can write must not be able
    // to decide a path.
    let version = version.trim();
    if validate_version(version).is_ok() {
        Cached::Known(version.to_string())
    } else {
        Cached::Unknown
    }
}

fn write_cache(version: &str) {
    if std::env::var_os("WS_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::atomic::atomic_write(&path, format!("{} {version}\n", now_secs()));
}

/// `$XDG_CACHE_HOME/ws/update-check`, else `~/.cache/ws/update-check` — the
/// same shape `cs` uses, and under `$HOME` either way so tests stay isolated.
fn cache_path() -> PathBuf {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cache"),
    };
    base.join("ws").join("update-check")
}

/// Changelog headlines for one pending version, beside the release cache. The
/// version is in the *filename* so a new release invalidates the notes by
/// construction rather than by anyone remembering to clear them.
fn notes_cache_path(latest: &str) -> PathBuf {
    let mut path = cache_path();
    path.set_file_name(format!("update-notes-{latest}"));
    path
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Is `a` a newer release than `b`? Numeric per component, so 0.10.0 beats
/// 0.9.0 — the string compare this replaced got that backwards. A pre-release
/// loses to the release it precedes (0.5.0-rc.1 < 0.5.0) and is otherwise
/// compared as text, which is enough to order rc.1 before rc.2.
fn version_greater(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, Option<String>) {
        let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p.to_string())));
        (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
    }
    let (an, ap) = parts(a);
    let (bn, bp) = parts(b);
    if an != bn {
        return an > bn;
    }
    match (ap, bp) {
        (None, None) => false,
        (None, Some(_)) => true, // release beats its own pre-release
        (Some(_), None) => false,
        (Some(x), Some(y)) => x > y,
    }
}

fn latest_tag(repository: &str) -> Result<String> {
    let output = run_gh(
        repository,
        &["release", "view", "--repo", repository, "--json", "tagName", "--jq", ".tagName"],
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
    let output =
        Command::new(&gh).args(args).output().with_context(|| format!("failed to run `{gh}`"))?;
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
        || parts.iter().any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
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
    use super::{strip_markdown, summarize, validate_version, version_greater, MORE};

    const CHANGELOG: &str = "\
# Changelog

## [Unreleased]

## [0.6.0] - 2026-08-05

### Added

- **Update notice.** Opening a workspace now says when a newer `ws` exists,
  reading the headline from `CHANGELOG.md`. More words that belong to the
  same bullet.
- A second entry, which is not the headline.

## [0.5.0] - 2026-08-01

### Fixed

- Something else changed here. And a second sentence.

## [0.4.0] - 2026-07-29

- The installed release; nothing at or below it may appear.

## [0.3.0] - 2026-07-01

- Older still.
";

    #[test]
    fn summarize_takes_one_headline_per_newer_release() {
        let notes = summarize(CHANGELOG, "0.4.0", 5);
        assert_eq!(notes.len(), 2, "only releases above 0.4.0: {notes:?}");
        assert_eq!(notes[0].0, "0.6.0");
        assert_eq!(notes[1].0, "0.5.0");
        assert_eq!(notes[1].1, "Something else changed here.");
    }

    /// The headline is one sentence of plain text, joined across the wrapped
    /// lines of its bullet — a raw first line would cut mid-clause.
    #[test]
    fn a_headline_is_one_plain_sentence() {
        let notes = summarize(CHANGELOG, "0.4.0", 5);
        assert_eq!(notes[0].1, "Update notice.");
        let notes =
            summarize("## [1.0.0]\n\n- One `wrapped`\n  bullet, no full stop\n", "0.1.0", 5);
        assert_eq!(notes[0].1, "One wrapped bullet, no full stop");
    }

    #[test]
    fn releases_past_the_cap_collapse_into_a_tail() {
        let notes = summarize(CHANGELOG, "0.3.0", 2);
        assert_eq!(notes.len(), 3, "two headlines plus the tail: {notes:?}");
        assert_eq!(notes[2].0, MORE);
        assert_eq!(notes[2].1, "… and 1 earlier version");
    }

    /// `[Unreleased]` heads the file and has no version. Parsed as one it would
    /// compare as older than anything installed and end the scan on line one,
    /// leaving every real release unreported.
    #[test]
    fn the_unreleased_heading_does_not_end_the_scan() {
        assert!(!summarize(CHANGELOG, "0.5.0", 5).is_empty());
    }

    #[test]
    fn summarize_reports_nothing_when_nothing_is_newer() {
        assert!(summarize(CHANGELOG, "0.6.0", 5).is_empty());
        assert!(summarize("", "0.1.0", 5).is_empty());
    }

    #[test]
    fn markdown_inlines_become_their_text() {
        assert_eq!(strip_markdown("**bold** and `code`"), "bold and code");
        assert_eq!(strip_markdown("see [the docs](https://x.dev/y) now"), "see the docs now");
    }

    #[test]
    fn newer_releases_compare_greater() {
        assert!(version_greater("0.5.0", "0.4.0"));
        assert!(version_greater("1.0.0", "0.9.9"));
        // The string compare this replaced called 0.10.0 older than 0.9.0.
        assert!(version_greater("0.10.0", "0.9.0"));
        assert!(!version_greater("0.4.0", "0.4.0"));
        assert!(!version_greater("0.4.0", "0.5.0"));
    }

    #[test]
    fn a_prerelease_ranks_below_its_release() {
        assert!(version_greater("0.5.0", "0.5.0-rc.1"));
        assert!(!version_greater("0.5.0-rc.1", "0.5.0"));
        assert!(version_greater("0.5.0-rc.2", "0.5.0-rc.1"));
        assert!(version_greater("0.5.0-rc.1", "0.4.0"));
    }

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
