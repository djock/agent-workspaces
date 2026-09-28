//! `ws -dispatch`: one agent session that works tasks for several workspaces
//! in order. See docs/superpowers/specs/2026-09-28-ws-dispatch-design.md.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub name: String,
    /// 1-based line of the `@name` marker (or `## N.` heading), for messages.
    pub line: usize,
    pub tasks: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    /// Text outside every section: before the first marker, or a generated
    /// plan's Unassigned section.
    pub preamble: String,
    pub sections: Vec<Section>,
    /// The input is a plan `-dispatch` generated earlier.
    pub rerun: bool,
}

const PLAN_TITLE: &str = "# Dispatch ";
const UNASSIGNED_HEADING: &str = "## Unassigned";

pub fn parse(text: &str) -> Parsed {
    let text = text.replace("\r\n", "\n");
    if text.starts_with(PLAN_TITLE) {
        parse_plan(&text)
    } else {
        parse_free(&text)
    }
}

fn parse_free(text: &str) -> Parsed {
    let mut out = Parsed::default();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix('@') {
            let (name, tail) = match rest.find(char::is_whitespace) {
                Some(at) => (&rest[..at], rest[at..].trim()),
                None => (rest, ""),
            };
            let name = name.trim_end_matches(':').to_string();
            let mut tasks = Vec::new();
            if !tail.is_empty() {
                tasks.push(item(tail));
            }
            out.sections.push(Section { name, line: i + 1, tasks });
        } else if let Some(sec) = out.sections.last_mut() {
            if !line.is_empty() {
                sec.tasks.push(item(line));
            }
        } else {
            out.preamble.push_str(raw);
            out.preamble.push('\n');
        }
    }
    out
}

fn parse_plan(text: &str) -> Parsed {
    // Where lines currently go: nowhere (title, protocol), a section, or the
    // unassigned text.
    enum At {
        Skip,
        Section,
        Unassigned,
    }
    let mut out = Parsed { rerun: true, ..Parsed::default() };
    let mut at = At::Skip;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with(UNASSIGNED_HEADING) {
            at = At::Unassigned;
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            // `## N. name — path`
            let numbered = h
                .split_once(". ")
                .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            match numbered {
                Some((_, rest)) => {
                    let name = rest.split(" — ").next().unwrap_or(rest).trim().to_string();
                    out.sections.push(Section { name, line: i + 1, tasks: Vec::new() });
                    at = At::Section;
                }
                None => at = At::Skip,
            }
            continue;
        }
        match at {
            At::Skip => {}
            At::Unassigned => {
                out.preamble.push_str(raw);
                out.preamble.push('\n');
            }
            At::Section => {
                let Some(sec) = out.sections.last_mut() else { continue };
                if line.is_empty() {
                    continue;
                }
                if line.starts_with("- ") || line.starts_with("* ") {
                    sec.tasks.push(item(line));
                } else if let Some(last) = sec.tasks.last_mut() {
                    // A note the agent wrote under a task (`? question`): keep it
                    // with that task so a rerun still carries it.
                    last.text.push_str("\n  ");
                    last.text.push_str(line);
                }
            }
        }
    }
    let trimmed = out.preamble.trim();
    out.preamble = if trimmed.is_empty() { String::new() } else { format!("{trimmed}\n") };
    out
}

/// One task line, with its list marker, number and checkbox stripped.
fn item(line: &str) -> Item {
    let mut s = line.trim();
    for bullet in ["- ", "* "] {
        if let Some(r) = s.strip_prefix(bullet) {
            s = r.trim_start();
            break;
        }
    }
    if let Some((n, r)) = s.split_once(". ") {
        if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            s = r.trim_start();
        }
    }
    let mut done = false;
    for (mark, d) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
        if let Some(r) = s.strip_prefix(mark) {
            s = r;
            done = d;
            break;
        }
    }
    Item { text: s.trim().to_string(), done }
}

/// What the registry and the workspace itself say about a name.
pub struct Known {
    pub path: PathBuf,
    pub archived: bool,
    /// A live process holding the workspace's lock.
    pub held_by: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    pub path: PathBuf,
    pub tasks: Vec<Item>,
}

/// Every reason the run cannot start, all at once — a list that fails on its
/// first error costs one rerun per typo.
///
/// A rerun (`p.rerun`) is empty when nothing is left, which the caller reports
/// as "nothing left to do" rather than as an error.
pub fn check(
    p: &Parsed,
    known: &dyn Fn(&str) -> Option<Known>,
    all: &[String],
    force: bool,
) -> Result<Vec<Target>, Vec<String>> {
    let sections: Vec<Section> = if p.rerun {
        p.sections
            .iter()
            .map(|s| Section {
                tasks: s.tasks.iter().filter(|t| !t.done).cloned().collect(),
                ..s.clone()
            })
            .filter(|s| !s.tasks.is_empty())
            .collect()
    } else {
        p.sections.clone()
    };
    if sections.is_empty() {
        return if p.rerun { Ok(Vec::new()) } else { Err(vec!["no @name sections found".into()]) };
    }

    let mut errs = Vec::new();
    let mut targets = Vec::new();
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for s in &sections {
        if let Some((_, first)) = seen.iter().find(|(n, _)| *n == s.name) {
            errs.push(format!(
                "'{}' appears twice (lines {first} and {}); merge the sections",
                s.name, s.line
            ));
            continue;
        }
        seen.push((&s.name, s.line));
        if s.tasks.is_empty() {
            errs.push(format!("'@{}' has no tasks", s.name));
            continue;
        }
        if let Err(e) = crate::workspace::validate_name(&s.name) {
            errs.push(format!("line {}: {e}", s.line));
            continue;
        }
        let Some(k) = known(&s.name) else {
            errs.push(match suggest(&s.name, all) {
                Some(m) => format!("no workspace '{}' — did you mean '{m}'?", s.name),
                None => format!("no workspace '{}'", s.name),
            });
            continue;
        };
        if k.archived {
            errs.push(format!("'{0}' is archived; ws -unarchive {0} first", s.name));
            continue;
        }
        if let (Some(pid), false) = (k.held_by, force) {
            errs.push(format!(
                "'{}' is open in another session (pid {pid}); close it or pass --force",
                s.name
            ));
            continue;
        }
        targets.push(Target { name: s.name.clone(), path: k.path, tasks: s.tasks.clone() });
    }
    if errs.is_empty() {
        Ok(targets)
    } else {
        Err(errs)
    }
}

/// The one registered name within edit distance 2, compared case-insensitively
/// so `@API` finds `api`. None when zero or several are that close — a guess
/// between two is worse than no guess.
fn suggest(name: &str, all: &[String]) -> Option<String> {
    let lower = name.to_lowercase();
    let close: Vec<&String> =
        all.iter().filter(|c| distance(&lower, &c.to_lowercase()) <= 2).collect();
    match close.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// Levenshtein distance over chars.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The first prompt the agent is launched with. The plan itself stays in the
/// file, so it survives a `/compact`.
pub const PROMPT: &str = "Work through tasks.md in this directory.";

const PROTOCOL: &str = "\
1. Work the projects below in the order listed. Before starting one, read its
   `.ws/README.md`, `.ws/conventions.md` if present, its `.ws/notebook/` files,
   and its root `CLAUDE.md` / `AGENTS.md` if present, and follow them. They do
   not load on their own because you were started outside that project.
2. Change files only inside the current project's directory. Do not commit, push, or switch branches.
3. Tick `[x]` in this file as each task is finished.
4. If a task is blocked or needs the user's decision, write the question on the
   line under that task, starting with `?`, leave it unticked, and continue.
   Do not stop to ask.
5. Before moving on, append an entry to your notebook in that workspace
   (`.ws/notebook/notebook.<actor>.md`; `ws -whoami` prints the actor): what was
   done, what was not, and why.
6. When every project is done, print one report block per project:
       api  (2/2 done)
         ✓ retry 429s with backoff — src/client.rs
         changed: 3 files (uncommitted)
         ? a question you left under a task
   Then list the questions again, then show the Unassigned text (if any) and ask
   the user what to do with it. Never act on the Unassigned text before that.
";

/// The plan file the agent works from. `parse` reads it back for a rerun.
pub fn render(targets: &[Target], unassigned: &str, stamp: &str) -> String {
    let mut s = format!("{PLAN_TITLE}{stamp}\n\n## Protocol\n\n{PROTOCOL}\n");
    for (i, t) in targets.iter().enumerate() {
        s.push_str(&format!("## {}. {} — {}\n", i + 1, t.name, t.path.display()));
        for task in &t.tasks {
            // A rerun's task can carry a `? question` note on its next line;
            // `text` already holds it indented, so it comes back out that way.
            s.push_str(&format!("- [ ] {}\n", task.text));
        }
        s.push('\n');
    }
    let unassigned = unassigned.trim();
    if !unassigned.is_empty() {
        s.push_str(&format!(
            "{UNASSIGNED_HEADING} (do not act on; ask the user at the end)\n\n{unassigned}\n"
        ));
    }
    s
}

const TEMPLATE: &str = "\
# One section per workspace, worked in this order. A line starting with @name
# begins a section; every non-blank line under it is one task. Text above the
# first @name is shown to you at the end, not acted on. Save and close to start;
# leave this unchanged to cancel.

";

/// Dispatch directories untouched this long are removed on the next run — the
/// same fortnight crash snapshots are kept.
const SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 3600);

/// `$XDG_CACHE_HOME/ws/dispatch`, else `~/.cache/ws/dispatch` — beside the
/// update check's cache.
fn dispatch_root() -> PathBuf {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cache"),
    };
    base.join("ws").join("dispatch")
}

/// `ws -dispatch [<file>]`.
pub fn run(
    file: Option<PathBuf>,
    agent: Option<String>,
    mode: Option<crate::agents::LaunchMode>,
    force: bool,
) -> Result<()> {
    // Before anything else: an editor session that ends in "claude is not
    // installed" wastes the list, and a run that never started must leave no
    // lock or timeline row behind.
    let cfg = crate::config::load();
    let agent = crate::agents::for_id(agent.as_deref().unwrap_or(&cfg.default_agent))?;
    if !agent.is_installed() {
        bail!(
            "{} is not installed or not on PATH (looked for `{}`). Install it, or set WS_{}_BIN.",
            agent.id(),
            agent.binary(),
            agent.id().to_uppercase()
        );
    }
    let root = dispatch_root();

    // A rerun of a generated plan reuses its own directory; anything else gets
    // a new one.
    let (text, scratch, input) = match &file {
        Some(f) => {
            let text = std::fs::read_to_string(f)
                .with_context(|| format!("cannot read {}", f.display()))?;
            let scratch = if text.starts_with(PLAN_TITLE) {
                f.canonicalize()?
                    .parent()
                    .map(Path::to_path_buf)
                    .context("the plan has no parent directory")?
            } else {
                new_scratch(&root)?
            };
            (text, scratch, None)
        }
        None => {
            let scratch = new_scratch(&root)?;
            let input = scratch.join("input.md");
            std::fs::write(&input, TEMPLATE)?;
            edit(&input)?;
            let text = std::fs::read_to_string(&input)?;
            let text = strip_template(&text);
            if text.trim().is_empty() {
                let _ = std::fs::remove_dir_all(&scratch);
                println!("nothing to dispatch");
                return Ok(());
            }
            (text, scratch, Some(input))
        }
    };

    // Swept only now, and never the directory in use: a rerun of a plan older
    // than the sweep would otherwise delete the file it is about to read.
    sweep(&root, &scratch);

    let parsed = parse(&text);
    let all: Vec<String> = crate::registry::all().into_iter().map(|(n, _)| n).collect();
    let targets = match check(&parsed, &known, &all, force) {
        Ok(t) => t,
        Err(errs) => {
            for e in &errs {
                eprintln!("ws: {e}");
            }
            if let Some(input) = &input {
                eprintln!("  your list is saved at {}", input.display());
                eprintln!("  fix it and run: ws -dispatch {}", input.display());
            }
            bail!("nothing dispatched ({} problem(s))", errs.len());
        }
    };
    if targets.is_empty() {
        println!("nothing left to do");
        return Ok(());
    }

    // Lock every target before the agent starts, so opening one elsewhere during
    // the run reports it busy. On a partial failure the guards already taken
    // drop, removing their files, as `?` returns.
    let mut guards = Vec::new();
    for t in &targets {
        guards.push(crate::lock::acquire(&lock_file(&t.path), force)?);
    }

    let plan = scratch.join("tasks.md");
    std::fs::write(&plan, render(&targets, &parsed.preamble, &crate::now_iso()))?;
    let actor = crate::actors::actor_slug();
    for t in &targets {
        let _ = crate::timeline::record(
            &t.path.join(".ws").join("timeline.jsonl"),
            "dispatched",
            &actor,
            serde_json::json!({ "dispatch": scratch.display().to_string(), "tasks": t.tasks.len() }),
        );
    }

    let dirs: Vec<PathBuf> = targets.iter().map(|t| t.path.clone()).collect();
    let (mode, note) = effective_mode(agent.id(), mode);
    if let Some(note) = note {
        eprintln!("{note}");
    }
    let cmd = agent.dispatch(&scratch, &dirs, PROMPT, mode)?;
    println!("dispatching {} project(s) — plan at {}", targets.len(), plan.display());

    // The agent inherits this pid, so the locks stay held until it exits and
    // are stale from then on — exactly how `ws <name>` hands its lock over.
    for g in guards {
        g.keep();
    }
    if std::env::var_os("WS_NO_EXEC").is_some() {
        let mut cmd = cmd;
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }
    crate::commands::exec(cmd)
}

/// The posture a dispatch actually runs with, and a line saying so when it is
/// not the one asked for.
///
/// Codex's `--add-dir` only makes a directory writable under the
/// `workspace-write` sandbox, and a dispatch starts in an untrusted scratch
/// directory where Codex defaults to `read-only` (seen on Codex CLI 0.156.1:
/// `sandbox: read-only`). With no posture given, that session could read every
/// project and edit none, so it runs `-sane` instead.
fn effective_mode(
    agent: &str,
    mode: Option<crate::agents::LaunchMode>,
) -> (Option<crate::agents::LaunchMode>, Option<&'static str>) {
    match (agent, mode) {
        ("codex", None) => (
            Some(crate::agents::LaunchMode::Sane),
            Some("ws: codex dispatch runs -sane so it can write to the granted directories"),
        ),
        (_, m) => (m, None),
    }
}

fn lock_file(root: &Path) -> PathBuf {
    root.join(".ws").join("local").join("lock")
}

/// Registry + meta + lock, for `check`.
fn known(name: &str) -> Option<Known> {
    let path = crate::registry::lookup(name)?;
    if !path.join(".ws").is_dir() {
        return None;
    }
    let archived = crate::meta::read(&path.join(".ws").join("workspace.toml")).archived;
    let held_by = crate::lock::live_pid_checked(&lock_file(&path)).ok().flatten();
    Some(Known { path, archived, held_by })
}

/// Drop the template's own comment lines, wherever the paste left them. Only
/// those exact lines: a `# heading` the user pasted is theirs.
fn strip_template(text: &str) -> String {
    let ours: Vec<&str> = TEMPLATE.lines().filter(|l| !l.trim().is_empty()).collect();
    text.replace("\r\n", "\n")
        .lines()
        .filter(|l| !ours.contains(l))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn new_scratch(root: &Path) -> Result<PathBuf> {
    let stamp = crate::now_iso().replace(':', "-");
    let mut dir = root.join(&stamp);
    if dir.exists() {
        dir = root.join(format!("{stamp}-{}", std::process::id()));
    }
    crate::atomic::create_private_dir_all(&dir)?;
    Ok(dir)
}

/// `$VISUAL`, else `$EDITOR`, else `vi`, through `sh -c` so an editor with its
/// own arguments (`code -w`) works. The file is a positional parameter, never
/// spliced into the command string.
fn edit(path: &Path) -> Result<()> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_string());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .with_context(|| format!("could not run the editor ({editor})"))?;
    if !status.success() {
        bail!("the editor exited with {status}; your list is saved at {}", path.display());
    }
    Ok(())
}

/// Remove dispatch directories untouched for a fortnight, except `keep`.
/// Best effort.
fn sweep(root: &Path, keep: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let keep = keep.canonicalize().unwrap_or_else(|_| keep.to_path_buf());
    for e in rd.flatten() {
        if e.path().canonicalize().is_ok_and(|p| p == keep) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > SWEEP_AFTER);
        if old && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sections_in_order_and_keeps_the_preamble() {
        let p =
            parse("notes for all\n\n@api\n- retry 429s\n* drop v1\n\n@web@redesign\n1. header\n");
        assert_eq!(p.preamble.trim(), "notes for all");
        let names: Vec<_> = p.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["api", "web@redesign"]);
        let api: Vec<_> = p.sections[0].tasks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(api, ["retry 429s", "drop v1"]);
        assert_eq!(p.sections[1].tasks[0].text, "header");
        assert_eq!(p.sections[0].line, 3);
        assert!(!p.rerun);
    }

    #[test]
    fn at_sign_mid_line_is_text() {
        let p = parse("@api\n- mail bob@example.com about it\n");
        assert_eq!(p.sections.len(), 1);
        assert_eq!(p.sections[0].tasks[0].text, "mail bob@example.com about it");
    }

    #[test]
    fn rest_of_the_marker_line_is_a_task() {
        let p = parse("@api fix the retry\n- and the logs\n");
        let t: Vec<_> = p.sections[0].tasks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(t, ["fix the retry", "and the logs"]);
    }

    #[test]
    fn crlf_indent_and_trailing_colon_are_tolerated() {
        let p = parse("  @api:\r\n\t- retry\r\n");
        assert_eq!(p.sections[0].name, "api");
        assert_eq!(p.sections[0].tasks[0].text, "retry");
    }

    #[test]
    fn checkbox_prefixes_are_stripped_and_ticks_read() {
        let p = parse("@api\n- [ ] one\n- [x] two\n");
        assert!(!p.sections[0].tasks[0].done);
        assert_eq!(p.sections[0].tasks[1].text, "two");
        assert!(p.sections[0].tasks[1].done);
    }

    #[test]
    fn a_generated_plan_is_read_back_by_its_headings() {
        let plan =
            "# Dispatch 2026-09-28T10:00:00Z\n\n## Protocol\n\n1. Work in order.\n- not a task\n\n\
                    ## 1. api — /p/api\n- [x] done one\n- [ ] open one\n  ? which one?\n\n\
                    ## 2. web — /p/web\n- [x] all done\n\n\
                    ## Unassigned (do not act on; ask the user at the end)\n\nleftover\n";
        let p = parse(plan);
        assert!(p.rerun);
        let names: Vec<_> = p.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["api", "web"]);
        assert_eq!(p.sections[0].tasks.len(), 2, "the `? question` line stays with its task");
        assert!(p.sections[0].tasks[1].text.contains("which one?"));
        assert_eq!(p.preamble.trim(), "leftover");
    }

    #[test]
    fn empty_and_marker_free_input_has_no_sections() {
        assert!(parse("").sections.is_empty());
        assert!(parse("just words\n").sections.is_empty());
    }

    fn known_fixture(name: &str) -> Option<Known> {
        match name {
            "api" => Some(Known { path: "/p/api".into(), archived: false, held_by: None }),
            "old" => Some(Known { path: "/p/old".into(), archived: true, held_by: None }),
            "busy" => Some(Known { path: "/p/busy".into(), archived: false, held_by: Some(4312) }),
            _ => None,
        }
    }

    fn all_fixture() -> Vec<String> {
        ["api", "old", "busy"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_clean_list_becomes_targets_in_order() {
        let p = parse("@busy\n- a\n@api\n- b\n");
        let t = check(&p, &known_fixture, &all_fixture(), true).unwrap();
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["busy", "api"]);
        assert_eq!(t[1].path, std::path::PathBuf::from("/p/api"));
    }

    #[test]
    fn every_problem_is_reported_together() {
        let p = parse(
            "@apii\n- a\n@api\n- b\n@api\n- c\n@old\n- d\n@busy\n- e\n@empty\n@bad;name\n- f\n",
        );
        let errs = check(&p, &known_fixture, &all_fixture(), false).unwrap_err();
        let all = errs.join("\n");
        assert!(all.contains("no workspace 'apii' — did you mean 'api'?"), "{all}");
        assert!(all.contains("'api' appears twice (lines 3 and 5)"), "{all}");
        assert!(all.contains("'old' is archived"), "{all}");
        assert!(all.contains("'busy' is open in another session (pid 4312)"), "{all}");
        assert!(all.contains("'@empty' has no tasks"), "{all}");
        assert!(all.contains("invalid workspace name"), "{all}");
    }

    #[test]
    fn no_sections_is_an_error() {
        let errs = check(&parse("words\n"), &known_fixture, &all_fixture(), false).unwrap_err();
        assert_eq!(errs, ["no @name sections found"]);
    }

    #[test]
    fn case_differences_are_suggested_not_folded() {
        let errs = check(&parse("@API\n- a\n"), &known_fixture, &all_fixture(), false).unwrap_err();
        assert!(errs[0].contains("did you mean 'api'?"), "{errs:?}");
    }

    #[test]
    fn a_far_off_name_gets_no_suggestion() {
        let errs =
            check(&parse("@zzzzzz\n- a\n"), &known_fixture, &all_fixture(), false).unwrap_err();
        assert_eq!(errs[0], "no workspace 'zzzzzz'");
    }

    #[test]
    fn a_rerun_drops_finished_projects_and_tasks() {
        let plan = "# Dispatch x\n\n## 1. api — /p/api\n- [x] one\n- [ ] two\n\n## 2. busy — /p/busy\n- [x] all\n";
        let t = check(&parse(plan), &known_fixture, &all_fixture(), false).unwrap();
        assert_eq!(t.len(), 1, "busy is finished, so its lock is irrelevant");
        assert_eq!(t[0].tasks.len(), 1);
        assert_eq!(t[0].tasks[0].text, "two");
    }

    #[test]
    fn a_finished_rerun_has_no_targets() {
        let plan = "# Dispatch x\n\n## 1. api — /p/api\n- [x] one\n";
        assert!(check(&parse(plan), &known_fixture, &all_fixture(), false).unwrap().is_empty());
    }

    fn targets_fixture() -> Vec<Target> {
        vec![
            Target {
                name: "api".into(),
                path: "/p/api".into(),
                tasks: vec![Item { text: "retry".into(), done: false }],
            },
            Target {
                name: "web@x".into(),
                path: "/p/My App".into(),
                tasks: vec![Item { text: "header".into(), done: false }],
            },
        ]
    }

    #[test]
    fn render_lists_projects_in_order_with_paths_and_protocol() {
        let s = render(&targets_fixture(), "", "2026-09-28T10:00:00Z");
        assert!(s.starts_with("# Dispatch 2026-09-28T10:00:00Z\n"));
        assert!(s.contains("## Protocol"));
        assert!(s.contains("Do not commit, push, or switch branches"));
        let a = s.find("## 1. api — /p/api\n- [ ] retry").unwrap();
        let b = s.find("## 2. web@x — /p/My App\n- [ ] header").unwrap();
        assert!(a < b);
        assert!(!s.contains("## Unassigned"), "no unassigned text, no section");
    }

    #[test]
    fn render_carries_unassigned_text_last() {
        let s = render(&targets_fixture(), "stray note\n", "t");
        assert!(s
            .trim_end()
            .ends_with("## Unassigned (do not act on; ask the user at the end)\n\nstray note"));
    }

    #[test]
    fn a_rendered_plan_parses_back_to_the_same_targets() {
        let s = render(&targets_fixture(), "stray\n", "t");
        let p = parse(&s);
        assert!(p.rerun);
        assert_eq!(
            p.sections.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["api", "web@x"]
        );
        assert_eq!(p.sections[1].tasks[0].text, "header");
        assert_eq!(p.preamble.trim(), "stray");
    }

    #[test]
    fn codex_without_a_posture_runs_sane_and_says_so() {
        use crate::agents::LaunchMode;
        let (mode, note) = effective_mode("codex", None);
        assert_eq!(mode, Some(LaunchMode::Sane));
        assert!(note.unwrap().contains("-sane"));
        assert_eq!(effective_mode("codex", Some(LaunchMode::Loco)), (Some(LaunchMode::Loco), None));
        assert_eq!(effective_mode("claude", None), (None, None));
    }
}
