//! Full-text search across workspaces, using ripgrep's own libraries.
//!
//! Scope is deliberately narrow: the committed documents under `.ws/` only.
//! `.ws/local/` (bash audit log, limits, state, lock) and `*.enc` (encrypted
//! secrets) are never opened — that is a security boundary, not an optimization.
use anyhow::Result;
use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Cap on lines reported per workspace, so one noisy notebook can't bury the rest.
pub const MAX_HITS_PER_WORKSPACE: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub workspace: String,
    pub file: PathBuf,
    pub line: u64,
    pub text: String,
}

fn is_searchable(path: &Path) -> bool {
    // Never read the local/ subtree or encrypted secrets.
    let mut in_ws_local = false;
    let comps: Vec<_> =
        path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    for (i, c) in comps.iter().enumerate() {
        if c == "local" && i > 0 && comps[i - 1] == ".ws" {
            in_ws_local = true;
        }
    }
    if in_ws_local {
        return false;
    }
    !path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("enc"))
}

/// Search one workspace root's `.ws/` documents. Returns (file, 1-based line, text).
/// May return up to `MAX_HITS_PER_WORKSPACE + 1` hits: callers use the extra one to
/// detect truncation without it being counted as an actual match.
pub fn search_dir(root: &Path, query: &str) -> Result<Vec<(PathBuf, u64, String)>> {
    let ws_dir = root.join(".ws");
    if !ws_dir.is_dir() {
        return Ok(Vec::new());
    }
    // Literal, case-insensitive: users type words, not regexes.
    // NOTE: grep::regex has no `escape` — that's why the `regex` crate is a dep.
    let matcher = RegexMatcher::new_line_matcher(&format!("(?i){}", regex::escape(query)))?;
    let mut searcher: Searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .build();

    // Collect one past the cap so the caller can tell "exactly MAX_HITS_PER_WORKSPACE
    // matches" apart from "more than MAX_HITS_PER_WORKSPACE matches" and say so.
    let collect_cap = MAX_HITS_PER_WORKSPACE + 1;
    let mut out = Vec::new();
    // Every `.ws/` document is a real document, not source code to filter —
    // workspaces are git repos, so a stray `*.md` line in .gitignore (or a
    // global core.excludesFile) must not make search silently miss text the
    // user knows they wrote. is_searchable() below is the actual security
    // boundary (.ws/local/, *.enc) and is independent of any ignore file.
    for entry in WalkBuilder::new(&ws_dir)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .build()
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable entry — skip, never abort the search
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !is_searchable(&path) {
            continue;
        }
        let mut file_hits: Vec<(PathBuf, u64, String)> = Vec::new();
        let res = searcher.search_path(
            &matcher,
            &path,
            UTF8(|lnum, line| {
                // A hit is a line out of a document ws did not write, printed
                // straight to the terminal. `.ws/` is git-synced, and search
                // reaches every workspace at once, so one hostile notebook line
                // would otherwise reach the screen of anyone who searched.
                file_hits.push((path.clone(), lnum, crate::term::display_safe(line.trim_end())));
                Ok(true)
            }),
        );
        if res.is_ok() {
            out.extend(file_hits);
        }
        if out.len() >= collect_cap {
            out.truncate(collect_cap);
            break;
        }
    }
    Ok(out)
}

/// Search every registered workspace, skipping archived ones unless asked.
pub fn search_all(query: &str, include_archived: bool) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for (name, path) in crate::registry::all() {
        let m = crate::meta::read(&path.join(".ws/workspace.toml"));
        if m.archived && !include_archived {
            continue;
        }
        for (file, line, text) in search_dir(&path, query)? {
            hits.push(Hit { workspace: name.clone(), file, line, text });
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a `.ws/` tree with a doc, a secret-ish local log, and an .enc file.
    /// Includes a `.git` dir at the workspace root: `ignore::WalkBuilder`'s
    /// `require_git` defaults to true, so a `.gitignore` is only consulted at
    /// all when a `.git` directory is present in the walk root or an ancestor.
    /// Without this, a fixture's `.gitignore` is inert regardless of whether
    /// the walker is configured to honor it — locking in nothing.
    fn fixture() -> TempDir {
        let d = TempDir::new().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        let ws = d.path().join(".ws");
        std::fs::create_dir_all(ws.join("notebook")).unwrap();
        std::fs::create_dir_all(ws.join("local/log")).unwrap();
        std::fs::write(ws.join("README.md"), "# proj\n\nObjective: ship the Kraken parser\n")
            .unwrap();
        std::fs::write(
            ws.join("notebook/notebook.me.md"),
            "day 1\nthe kraken retries on 429\nday 2\n",
        )
        .unwrap();
        std::fs::write(ws.join("local/log/session.log"), "curl kraken --key hunter2\n").unwrap();
        std::fs::write(ws.join("secrets.enc"), "kraken-ciphertext").unwrap();
        // Uppercase/mixed-case extension: the exclusion must not depend on
        // filenames actually being lowercased elsewhere in the codebase.
        std::fs::write(ws.join("legacy.ENC"), "kraken-legacy-ciphertext").unwrap();
        d
    }

    #[test]
    fn finds_matches_case_insensitively_with_line_numbers() {
        let d = fixture();
        let hits = search_dir(d.path(), "kraken").unwrap();
        let files: Vec<String> = hits
            .iter()
            .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(files.contains(&"README.md".to_string()), "{files:?}");
        assert!(files.contains(&"notebook.me.md".to_string()), "{files:?}");
        let nb = hits.iter().find(|(p, _, _)| p.ends_with("notebook.me.md")).unwrap();
        assert_eq!(nb.1, 2, "line numbers are 1-based");
        assert!(nb.2.contains("429"));
    }

    /// A hit is a line from a document ws did not write, printed straight to the
    /// terminal — and `ws -search` reaches every registered workspace at once, so
    /// one hostile notebook line is enough to reach anyone who searches.
    #[test]
    fn a_matched_line_cannot_carry_a_control_sequence_to_the_terminal() {
        let d = fixture();
        std::fs::write(
            d.path().join(".ws/notebook/notebook.them.md"),
            "kraken \x1b[2J\x1b]0;pwned\x07 notes\n",
        )
        .unwrap();
        let hits = search_dir(d.path(), "kraken").unwrap();
        let hit = hits.iter().find(|(p, _, _)| p.ends_with("notebook.them.md")).unwrap();
        assert!(!hit.2.contains('\u{1b}'), "an escape byte survived into a hit: {:?}", hit.2);
        assert!(hit.2.contains("kraken"), "the match itself must survive: {:?}", hit.2);
    }

    #[test]
    fn never_searches_local_or_encrypted_files() {
        let d = fixture();
        let hits = search_dir(d.path(), "kraken").unwrap();
        for (p, _, text) in &hits {
            let s = p.to_string_lossy();
            assert!(!s.contains("/local/"), "search must never read .ws/local: {s}");
            assert!(
                !s.to_lowercase().ends_with(".enc"),
                "search must never read encrypted secrets: {s}"
            );
            assert!(!text.contains("hunter2"), "a secret leaked into search output");
        }
        assert!(
            !hits.iter().any(|(p, _, _)| p.ends_with("legacy.ENC")),
            "uppercase .ENC extension must be excluded too: {hits:?}"
        );
    }

    #[test]
    fn query_is_a_literal_not_a_regex() {
        let d = fixture();
        // `.*` must match nothing here — if it were treated as a regex it would
        // match every line in the fixture.
        assert!(search_dir(d.path(), ".*").unwrap().is_empty());
        assert!(!search_dir(d.path(), "429").unwrap().is_empty());
    }

    #[test]
    fn missing_ws_dir_yields_no_hits_not_an_error() {
        let d = TempDir::new().unwrap();
        assert!(search_dir(d.path(), "anything").unwrap().is_empty());
    }

    // I3: workspaces are git repos, and every .ws/ document is markdown — a
    // `*.md` line in .gitignore must not make search silently miss text the
    // user knows they wrote.
    #[test]
    fn finds_matches_even_when_gitignore_excludes_markdown() {
        let d = fixture();
        std::fs::write(d.path().join(".ws/.gitignore"), "*.md\n").unwrap();
        let hits = search_dir(d.path(), "kraken").unwrap();
        let files: Vec<String> = hits
            .iter()
            .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(files.contains(&"README.md".to_string()), "{files:?}");
        assert!(files.contains(&"notebook.me.md".to_string()), "{files:?}");
    }

    // The .ws/local/ and *.enc exclusions are a security boundary enforced in
    // code by is_searchable(), not a side effect of an ignore file. Prove
    // that: write a .gitignore that does NOT mention local/ or *.enc (and
    // would do nothing for them even if honored), with a real .git dir
    // present so the ignore file is eligible to be consulted at all. If the
    // exclusion were coming from gitignore filtering rather than code, these
    // files would still show up here; they must not.
    #[test]
    fn local_and_enc_exclusion_holds_independent_of_ignore_files() {
        let d = fixture();
        std::fs::write(d.path().join(".ws/.gitignore"), "*.tmp\n").unwrap();
        let hits = search_dir(d.path(), "kraken").unwrap();
        for (p, _, text) in &hits {
            let s = p.to_string_lossy();
            assert!(!s.contains("/local/"), "must never read .ws/local: {s}");
            assert!(!s.to_lowercase().ends_with(".enc"), "must never read encrypted secrets: {s}");
            assert!(!text.contains("hunter2"), "a secret leaked into search output");
        }
        // And normal docs still come through — the .gitignore's presence
        // doesn't accidentally suppress everything.
        assert!(
            hits.iter().any(|(p, _, _)| p.ends_with("README.md")),
            "unrelated .gitignore must not affect ordinary doc hits: {hits:?}"
        );
    }
}
