//! Notebook rollover: keep the notebook an agent rereads at every start small.
//!
//! Past [`ROLL_OVER_BYTES`], the oldest whole `## ` entries move to
//! `.ws/notebook-archive/notebook.<actor>.md`, which the start protocol does
//! not list, until the notebook's entries fit in [`KEEP_BYTES`].

use anyhow::{Context, Result};
use std::io::Write;

use crate::workspace::Workspace;

/// A notebook larger than this rolls over at session start.
pub const ROLL_OVER_BYTES: usize = 32 * 1024;
/// What a rolled notebook's entries are cut back to. Half the trigger, so a
/// roll is not due again at the very next start.
pub const KEEP_BYTES: usize = 16 * 1024;

/// Split a notebook into its preamble (title and anything before the first
/// entry) and its entries. An entry starts at a line beginning `## ` outside a
/// fenced code block. The slices concatenate back to `text` exactly.
fn split_entries(text: &str) -> (&str, Vec<&str>) {
    let mut starts = Vec::new();
    let mut in_fence = false;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence && line.starts_with("## ") {
            starts.push(at);
        }
        at += line.len();
    }
    let Some(&first) = starts.first() else {
        return (text, Vec::new());
    };
    let mut entries = Vec::with_capacity(starts.len());
    for (i, &s) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(text.len());
        entries.push(&text[s..end]);
    }
    (&text[..first], entries)
}

/// Index of the oldest entry to keep: walk newest to oldest while the kept
/// bytes stay within `budget`. The newest entry is always kept.
fn first_kept(entries: &[&str], budget: usize) -> usize {
    let mut i = entries.len().saturating_sub(1);
    let mut total = entries.get(i).map_or(0, |e| e.len());
    while i > 0 && total + entries[i - 1].len() <= budget {
        i -= 1;
        total += entries[i].len();
    }
    i
}

fn pointer_line(actor: &str) -> String {
    format!(
        "Older entries live in .ws/notebook-archive/notebook.{actor}.md. \
         Read them only when you need that history."
    )
}

/// The preamble with the archive pointer under its title, added once.
fn with_pointer(preamble: &str, actor: &str) -> String {
    let pointer = pointer_line(actor);
    if preamble.contains(&pointer) {
        return preamble.to_string();
    }
    let nl = if preamble.contains("\r\n") { "\r\n" } else { "\n" };
    match preamble.find('\n') {
        Some(i) => {
            let (title, rest) = preamble.split_at(i + 1);
            format!("{title}{nl}{pointer}{nl}{rest}")
        }
        None if preamble.is_empty() => format!("{pointer}{nl}{nl}"),
        None => format!("{preamble}{nl}{nl}{pointer}{nl}{nl}"),
    }
}

/// Roll `actor`'s notebook over if it has grown past [`ROLL_OVER_BYTES`].
/// Returns how many entries moved to the archive (0 when nothing was due).
///
/// The archive is appended to, and synced, before the notebook is rewritten:
/// a crash between the two leaves an entry in both files, never in neither.
pub fn roll_over(ws: &Workspace, actor: &str) -> Result<usize> {
    let path = ws.notebook_dir().join(format!("notebook.{actor}.md"));
    match std::fs::metadata(&path) {
        Ok(m) if m.len() as usize > ROLL_OVER_BYTES => {}
        Ok(_) => return Ok(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    }
    crate::txn::transaction(&path, || {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if text.len() <= ROLL_OVER_BYTES {
            return Ok(0);
        }
        let (preamble, entries) = split_entries(&text);
        let keep = first_kept(&entries, KEEP_BYTES);
        if keep == 0 {
            return Ok(0);
        }

        let dir = ws.notebook_archive_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let archive = dir.join(format!("notebook.{actor}.md"));
        let existing = match std::fs::read(&archive) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", archive.display())),
        };
        let mut out = String::new();
        if existing.is_empty() {
            out.push_str(&format!("# Notebook archive ({actor})\n\n"));
        } else if !existing.ends_with(b"\n") {
            out.push('\n');
        }
        entries[..keep].iter().for_each(|e| out.push_str(e));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&archive)
            .with_context(|| format!("opening {}", archive.display()))?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;

        let mut kept = with_pointer(preamble, actor);
        entries[keep..].iter().for_each(|e| kept.push_str(e));
        crate::atomic::atomic_write(&path, kept)?;
        Ok(keep)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use tempfile::TempDir;

    const ACTOR: &str = "me";

    fn ws_in(d: &TempDir) -> Workspace {
        std::fs::create_dir_all(d.path().join(".ws/notebook")).unwrap();
        Workspace { name: "t".into(), root: d.path().to_path_buf() }
    }
    fn nb(ws: &Workspace) -> std::path::PathBuf {
        ws.notebook_dir().join(format!("notebook.{ACTOR}.md"))
    }
    fn archive(ws: &Workspace) -> std::path::PathBuf {
        ws.notebook_archive_dir().join(format!("notebook.{ACTOR}.md"))
    }
    fn big(nl: &str) -> String {
        let mut s = format!("# Notebook ({ACTOR}){nl}{nl}");
        for i in 0..40 {
            s.push_str(&format!("## e{i}{nl}{}{nl}", "x".repeat(1000)));
        }
        s
    }
    fn pointer() -> String {
        format!(
            "Older entries live in .ws/notebook-archive/notebook.{ACTOR}.md. \
             Read them only when you need that history."
        )
    }

    #[test]
    fn splits_preamble_and_entries() {
        let (pre, entries) = split_entries("# T\n\nintro\n## a\nx\n## b\ny\n");
        assert_eq!(pre, "# T\n\nintro\n");
        assert_eq!(entries, vec!["## a\nx\n", "## b\ny\n"]);
    }

    #[test]
    fn fenced_heading_is_not_an_entry() {
        let (_, entries) = split_entries("# T\n## a\n```\n## not\n```\n## b\n");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("## not"));
    }

    #[test]
    fn first_kept_respects_budget() {
        let e = ["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc"];
        assert_eq!(first_kept(&e, 25), 1);
        assert_eq!(first_kept(&e, 5), 2);
    }

    #[test]
    fn under_trigger_is_untouched() {
        let d = TempDir::new().unwrap();
        let ws = ws_in(&d);
        let text = format!("# N\n\n## a\n{}\n", "x".repeat(1000));
        std::fs::write(nb(&ws), &text).unwrap();
        assert_eq!(roll_over(&ws, ACTOR).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(nb(&ws)).unwrap(), text);
        assert!(!ws.notebook_archive_dir().exists());
    }

    #[test]
    fn no_entries_means_no_roll() {
        let d = TempDir::new().unwrap();
        let ws = ws_in(&d);
        let text = format!("# N\n\n{}\n", "x".repeat(40 * 1024));
        std::fs::write(nb(&ws), &text).unwrap();
        assert_eq!(roll_over(&ws, ACTOR).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(nb(&ws)).unwrap(), text);
    }

    fn check_rolled(ws: &Workspace, n: usize, nl: &str) {
        assert!(n > 0);
        let kept = std::fs::read_to_string(nb(ws)).unwrap();
        assert!(kept.starts_with(&format!("# Notebook ({ACTOR}){nl}")));
        assert_eq!(kept.matches(&pointer()).count(), 1);
        assert!(kept.len() <= KEEP_BYTES + 300, "kept {} bytes", kept.len());
        assert!(kept.trim_end().ends_with(&"x".repeat(1000)));
        assert!(kept.contains(&format!("## e39{nl}")));
        assert!(!kept.contains(&format!("## e{}{nl}", n - 1)));
        let arch = std::fs::read_to_string(archive(ws)).unwrap();
        assert!(arch.starts_with(&format!("# Notebook archive ({ACTOR})\n\n")));
        let mut at = 0;
        for i in 0..n {
            let h = format!("## e{i}{nl}");
            let p = arch[at..].find(&h).unwrap_or_else(|| panic!("missing {h}"));
            at += p + h.len();
        }
        assert!(!arch.contains(&format!("## e{n}{nl}")));
    }

    #[test]
    fn rolls_oldest_into_archive() {
        let d = TempDir::new().unwrap();
        let ws = ws_in(&d);
        std::fs::write(nb(&ws), big("\n")).unwrap();
        let n = roll_over(&ws, ACTOR).unwrap();
        check_rolled(&ws, n, "\n");
    }

    #[test]
    fn second_roll_is_a_noop() {
        let d = TempDir::new().unwrap();
        let ws = ws_in(&d);
        std::fs::write(nb(&ws), big("\n")).unwrap();
        roll_over(&ws, ACTOR).unwrap();
        let (a, b) = (std::fs::read(nb(&ws)).unwrap(), std::fs::read(archive(&ws)).unwrap());
        assert_eq!(roll_over(&ws, ACTOR).unwrap(), 0);
        assert_eq!(std::fs::read(nb(&ws)).unwrap(), a);
        assert_eq!(std::fs::read(archive(&ws)).unwrap(), b);
    }

    #[test]
    fn archive_is_appended_in_order() {
        let d = TempDir::new().unwrap();
        let ws = ws_in(&d);
        std::fs::create_dir_all(ws.notebook_archive_dir()).unwrap();
        let old = format!("# Notebook archive ({ACTOR})\n\n## old\n");
        std::fs::write(archive(&ws), &old).unwrap();
        std::fs::write(nb(&ws), big("\n")).unwrap();
        roll_over(&ws, ACTOR).unwrap();
        let arch = std::fs::read_to_string(archive(&ws)).unwrap();
        assert!(arch.starts_with(&old));
        assert!(arch[old.len()..].starts_with("## e0\n"));
        assert_eq!(arch.matches("# Notebook archive").count(), 1);
    }

    #[test]
    fn crlf_notebook_round_trips() {
        let d = TempDir::new().unwrap();
        let ws = ws_in(&d);
        let text = big("\r\n");
        std::fs::write(nb(&ws), &text).unwrap();
        let n = roll_over(&ws, ACTOR).unwrap();
        check_rolled(&ws, n, "\r\n");
        let (_, orig) = split_entries(&text);
        let kept = std::fs::read_to_string(nb(&ws)).unwrap();
        let (_, now) = split_entries(&kept);
        assert_eq!(now, orig[n..].to_vec());
    }
}
