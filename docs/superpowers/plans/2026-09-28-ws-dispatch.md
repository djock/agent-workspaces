# `ws -dispatch` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ws -dispatch [<tasks.md>]` reads a multi-project task list (from a file or `$EDITOR`), checks every `@name` against the registry, and launches one agent session that works the projects in order.

**Architecture:** A new `src/dispatch.rs` holds the pure parts (parse, check, render) and the command (`run`). Launch goes through a new `Agent::dispatch` method per agent, because `Agent::launch` is built around one workspace and its resume state. Target locks are taken, `keep()`-ed and inherited through `exec`, exactly as `ws <name>` does it.

**Tech Stack:** Rust 2021, anyhow, existing `lock`, `registry`, `meta`, `timeline`, `workspace` modules; assert_cmd + tempfile for integration tests.

**Spec:** `docs/superpowers/specs/2026-09-28-ws-dispatch-design.md`

## Global Constraints

- `cargo` is at `~/.cargo/bin`: every shell step starts with `export PATH="$HOME/.cargo/bin:$PATH"`.
- Gates: `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --all-targets --all-features` both clean.
- No new crate dependencies.
- Names go through `workspace::validate_name`; only `@name` at line start counts.
- The run refuses as a whole: any check error → nothing launched, no lock taken.
- Never commit, push or switch branches in a target workspace (protocol text).
- Every new command token appears in `cli::help_text()` (a test enforces it).

## Review Focus

1. **Launched from inside a ws session.** `WS_WORKSPACE`/`WS_DIR`/`WS_AGENT`/`CLAUDE_COWORK_MEMORY_PATH_OVERRIDE` are inherited, so the dispatcher's hooks would act as that workspace. Expected: the dispatch command removes them. Pinned in Task 4.
2. **Claude's `--add-dir <directories...>` is variadic.** A prompt placed after it is swallowed as a directory. Expected: the prompt is the first argument. Pinned in Task 4.
3. **Paths with spaces** (`~/Projects/My App`). Expected: passed as one argv element each, never through a shell. Pinned in Task 4.
4. **Pasted text with CRLF, tabs, `@api:` (trailing colon), or `@API` case.** Expected: CRLF and indentation tolerated, trailing `:` stripped; case is **not** folded (names are case-sensitive in the registry), but the "did you mean" suggestion catches it. Pinned in Tasks 1 and 2.
5. **Editor closed without changes / cancelled.** Expected: exit 0, nothing launched, no lock taken, no timeline row. Pinned in Task 5.

---

### Task 1: Parser

**Files:**
- Create: `src/dispatch.rs`
- Modify: `src/main.rs` (add `mod dispatch;`)

**Interfaces:**
- Produces:
  ```rust
  pub struct Section { pub name: String, pub line: usize, pub tasks: Vec<Item> }
  pub struct Item { pub text: String, pub done: bool }
  pub struct Parsed { pub preamble: String, pub sections: Vec<Section>, pub rerun: bool }
  pub fn parse(text: &str) -> Parsed
  ```
  `line` is 1-based. `rerun` is true when the text is a generated plan.

- [ ] **Step 1: Write failing tests** (in `src/dispatch.rs`, `#[cfg(test)] mod tests`):

```rust
#[test]
fn splits_sections_in_order_and_keeps_the_preamble() {
    let p = parse("notes for all\n\n@api\n- retry 429s\n* drop v1\n\n@web@redesign\n1. header\n");
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
    let plan = "# Dispatch 2026-09-28T10:00:00Z\n\n## Protocol\n\n1. Work in order.\n- not a task\n\n\
                ## 1. api — /p/api\n- [x] done one\n- [ ] open one\n  ? which one?\n\n\
                ## 2. web — /p/web\n- [x] all done\n\n\
                ## Unassigned (do not act on; ask the user at the end)\n\nleftover\n";
    let p = parse(plan);
    assert!(p.rerun);
    let names: Vec<_> = p.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["api", "web"]);
    assert_eq!(p.sections[0].tasks.len(), 2, "the `? question` line is kept with its task, not a task");
    assert!(p.sections[0].tasks[1].text.contains("which one?"));
    assert_eq!(p.preamble.trim(), "leftover");
}

#[test]
fn empty_and_marker_free_input_has_no_sections() {
    assert!(parse("").sections.is_empty());
    assert!(parse("just words\n").sections.is_empty());
}
```

- [ ] **Step 2: Run** `cargo test --bin ws dispatch::` — Expected: compile failure (module missing).

- [ ] **Step 3: Implement**

```rust
//! `ws -dispatch`: one agent session that works tasks for several workspaces
//! in order. See docs/superpowers/specs/2026-09-28-ws-dispatch-design.md.

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
    let mut out = Parsed { rerun: true, ..Parsed::default() };
    // Where lines currently go: nowhere (title, protocol), a section, or the
    // unassigned text.
    enum At { Skip, Section, Unassigned }
    let mut at = At::Skip;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with(UNASSIGNED_HEADING) {
            at = At::Unassigned;
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            // `## N. name — path`
            let numbered = h.split_once(". ").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()));
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
                let sec = out.sections.last_mut().expect("At::Section implies a section");
                if line.is_empty() {
                    continue;
                }
                if line.starts_with("- ") || line.starts_with("* ") {
                    sec.tasks.push(item(line));
                } else if let Some(last) = sec.tasks.last_mut() {
                    // A note the agent wrote under a task (`? question`): keep it
                    // with that task so a rerun still sees it.
                    last.text.push_str("\n  ");
                    last.text.push_str(line);
                }
            }
        }
    }
    out.preamble = out.preamble.trim().to_string();
    if !out.preamble.is_empty() {
        out.preamble.push('\n');
    }
    out
}

/// One task line, with its list marker and checkbox stripped.
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
```

Add `mod dispatch;` to `src/main.rs` in alphabetical position (after `mod detail;`). Add `#[allow(dead_code)]` on the module line only if clippy complains before Task 5 wires it; remove it in Task 5.

- [ ] **Step 4: Run** `cargo test --bin ws dispatch::` — Expected: all PASS.

- [ ] **Step 5: Commit** `git add src/dispatch.rs src/main.rs && git commit -m "feat(dispatch): parse @name task lists and generated plans"`

---

### Task 2: Checks

**Files:** Modify `src/dispatch.rs`

**Interfaces:**
- Consumes: `Parsed`, `Section` (Task 1).
- Produces:
  ```rust
  pub struct Known { pub path: PathBuf, pub archived: bool, pub held_by: Option<u32> }
  pub struct Target { pub name: String, pub path: PathBuf, pub tasks: Vec<Item> }
  pub fn check(p: &Parsed, known: &dyn Fn(&str) -> Option<Known>, all: &[String], force: bool)
      -> Result<Vec<Target>, Vec<String>>
  fn suggest(name: &str, all: &[String]) -> Option<String>
  ```
  For a rerun, sections whose tasks are all `done` are dropped **before** the checks, and `Target.tasks` keeps only the undone ones.

- [ ] **Step 1: Failing tests**

```rust
fn known_fixture(name: &str) -> Option<Known> {
    match name {
        "api" => Some(Known { path: "/p/api".into(), archived: false, held_by: None }),
        "old" => Some(Known { path: "/p/old".into(), archived: true, held_by: None }),
        "busy" => Some(Known { path: "/p/busy".into(), archived: false, held_by: Some(4312) }),
        _ => None,
    }
}
fn all_fixture() -> Vec<String> { ["api", "old", "busy"].iter().map(|s| s.to_string()).collect() }

#[test]
fn a_clean_list_becomes_targets_in_order() {
    let p = parse("@busy\n- a\n@api\n- b\n");
    let t = check(&p, &known_fixture, &all_fixture(), true).unwrap();
    assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["busy", "api"]);
    assert_eq!(t[1].path, std::path::PathBuf::from("/p/api"));
}

#[test]
fn every_problem_is_reported_together() {
    let p = parse("@apii\n- a\n@api\n- b\n@api\n- c\n@old\n- d\n@busy\n- e\n@empty\n@bad;name\n- f\n");
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
    let errs = check(&parse("@zzzzzz\n- a\n"), &known_fixture, &all_fixture(), false).unwrap_err();
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
```

- [ ] **Step 2: Run** `cargo test --bin ws dispatch::` — Expected: FAIL (items undefined).

- [ ] **Step 3: Implement**

```rust
use std::path::PathBuf;

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
            .map(|s| Section { tasks: s.tasks.iter().filter(|t| !t.done).cloned().collect(), ..s.clone() })
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
            errs.push(format!("'{}' appears twice (lines {first} and {}); merge the sections", s.name, s.line));
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
            errs.push(format!("'{}' is open in another session (pid {pid}); close it or pass --force", s.name));
            continue;
        }
        targets.push(Target { name: s.name.clone(), path: k.path, tasks: s.tasks.clone() });
    }
    if errs.is_empty() { Ok(targets) } else { Err(errs) }
}

/// The one registered name within edit distance 2, compared case-insensitively
/// so `@API` finds `api`. None when zero or several are that close — a guess
/// between two is worse than no guess.
fn suggest(name: &str, all: &[String]) -> Option<String> {
    let lower = name.to_lowercase();
    let close: Vec<&String> = all.iter().filter(|c| distance(&lower, &c.to_lowercase()) <= 2).collect();
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
```

Note the test fixture for `every_problem_is_reported_together` has `api` both as a duplicate and a valid name; with `apii` at distance 1 from `api` and distance 2+ from `old`/`busy`, the suggestion is unique.

- [ ] **Step 4: Run** `cargo test --bin ws dispatch::` — Expected: PASS.

- [ ] **Step 5: Commit** `git commit -am "feat(dispatch): check names, duplicates, archive and locks together"`

---

### Task 3: Plan rendering

**Files:** Modify `src/dispatch.rs`

**Interfaces:**
- Consumes: `Target`, `Item` (Task 2).
- Produces: `pub fn render(targets: &[Target], unassigned: &str, stamp: &str) -> String` and `pub const PROMPT: &str = "Work through tasks.md in this directory.";`

- [ ] **Step 1: Failing tests**

```rust
fn targets_fixture() -> Vec<Target> {
    vec![
        Target { name: "api".into(), path: "/p/api".into(),
                 tasks: vec![Item { text: "retry".into(), done: false }] },
        Target { name: "web@x".into(), path: "/p/My App".into(),
                 tasks: vec![Item { text: "header".into(), done: false }] },
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
    assert!(!s.contains("Unassigned"), "no unassigned text, no section");
}

#[test]
fn render_carries_unassigned_text_last() {
    let s = render(&targets_fixture(), "stray note\n", "t");
    assert!(s.trim_end().ends_with("## Unassigned (do not act on; ask the user at the end)\n\nstray note"));
}

#[test]
fn a_rendered_plan_parses_back_to_the_same_targets() {
    let s = render(&targets_fixture(), "stray\n", "t");
    let p = parse(&s);
    assert!(p.rerun);
    assert_eq!(p.sections.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["api", "web@x"]);
    assert_eq!(p.sections[1].tasks[0].text, "header");
    assert_eq!(p.preamble.trim(), "stray");
}
```

- [ ] **Step 2: Run** — Expected: FAIL (`render` undefined).

- [ ] **Step 3: Implement**

```rust
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

pub fn render(targets: &[Target], unassigned: &str, stamp: &str) -> String {
    let mut s = format!("{PLAN_TITLE}{stamp}\n\n## Protocol\n\n{PROTOCOL}\n");
    for (i, t) in targets.iter().enumerate() {
        s.push_str(&format!("## {}. {} — {}\n", i + 1, t.name, t.path.display()));
        for task in &t.tasks {
            // A multi-line task (a rerun's `? question` note) keeps its note
            // on the following, indented line.
            s.push_str(&format!("- [ ] {}\n", task.text));
        }
        s.push('\n');
    }
    let unassigned = unassigned.trim();
    if !unassigned.is_empty() {
        s.push_str(&format!("{UNASSIGNED_HEADING} (do not act on; ask the user at the end)\n\n{unassigned}\n"));
    }
    s
}
```

The PROTOCOL's own numbered lines contain no `## ` heading and sit in the Skip zone, so they never parse as tasks (the round-trip test pins this).

- [ ] **Step 4: Run** — Expected: PASS.

- [ ] **Step 5: Commit** `git commit -am "feat(dispatch): render the plan file the agent works from"`

---

### Task 4: `Agent::dispatch` for both agents

**Files:**
- Modify: `src/agents/mod.rs` (trait method), `src/agents/claude.rs`, `src/agents/codex.rs` (impl + tests)

**Interfaces:**
- Produces:
  ```rust
  fn dispatch(&self, scratch: &Path, dirs: &[PathBuf], prompt: &str, mode: Option<LaunchMode>) -> anyhow::Result<Command>;
  ```
  No default body (same rule as `mode_args`). Always fresh; records nothing.

- [ ] **Step 1: Failing tests** in `src/agents/claude.rs` tests module:

```rust
#[test]
fn dispatch_puts_the_prompt_first_and_grants_each_dir() {
    let d = TempDir::new().unwrap();
    let dirs = vec![d.path().join("api"), d.path().join("My App")];
    let cmd = ClaudeAgent
        .dispatch(d.path(), &dirs, "Work.", Some(crate::agents::LaunchMode::Sane))
        .unwrap();
    let args = args_of(&cmd);
    // `--add-dir <directories...>` is variadic: a prompt after it would be read
    // as one more directory.
    assert_eq!(args[0], "Work.");
    let i = args.iter().position(|a| a == "--add-dir").unwrap();
    assert_eq!(args[i + 1], dirs[0].to_string_lossy());
    assert_eq!(args[i + 2], dirs[1].to_string_lossy(), "a path with a space stays one argument");
    assert!(args.contains(&"--permission-mode".to_string()));
    assert!(!args.iter().any(|a| a == "--resume" || a == "--session-id"));
    assert_eq!(cmd.get_current_dir(), Some(d.path()));
}

#[test]
fn dispatch_drops_an_inherited_workspace_identity() {
    let d = TempDir::new().unwrap();
    let cmd = ClaudeAgent.dispatch(d.path(), &[d.path().to_path_buf()], "Work.", None).unwrap();
    for key in ["WS_WORKSPACE", "WS_DIR", "WS_AGENT", "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"] {
        let removed = cmd.get_envs().any(|(k, v)| k == key && v.is_none());
        assert!(removed, "{key} must be removed, or the dispatcher's hooks act as that workspace");
    }
}
```

In `src/agents/codex.rs` tests module:

```rust
#[test]
fn dispatch_grants_each_dir_and_ends_with_the_prompt() {
    let d = TempDir::new().unwrap();
    let dirs = vec![d.path().join("api"), d.path().join("My App")];
    let cmd = CodexAgent.dispatch(d.path(), &dirs, "Work.", None).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    let expected_dirs: Vec<String> = dirs
        .iter()
        .flat_map(|p| ["--add-dir".to_string(), p.to_string_lossy().to_string()])
        .collect();
    assert_eq!(&args[..4], expected_dirs.as_slice());
    assert_eq!(args.last().unwrap(), "Work.");
    assert!(!args.iter().any(|a| a == "resume"));
    let removed = cmd.get_envs().any(|(k, v)| k == "WS_WORKSPACE" && v.is_none());
    assert!(removed);
}
```

- [ ] **Step 2: Run** `cargo test --bin ws agents::` — Expected: FAIL (no method `dispatch`).

- [ ] **Step 3: Implement**

`src/agents/mod.rs`, in `trait Agent` after `launch`:

```rust
    /// A one-time session for `ws -dispatch`: started in `scratch`, granted
    /// `dirs`, handed `prompt`. Always fresh, and records no session id — there
    /// is no workspace to file it under.
    ///
    /// **No default**: each agent spells both the directory grant and the prompt
    /// position differently (claude's `--add-dir` is variadic and would swallow a
    /// trailing prompt).
    fn dispatch(
        &self,
        scratch: &std::path::Path,
        dirs: &[PathBuf],
        prompt: &str,
        mode: Option<LaunchMode>,
    ) -> anyhow::Result<Command>;
```

Add a shared helper in `src/agents/mod.rs`:

```rust
/// The environment a launch inside a workspace sets. A dispatch started from
/// inside a ws session inherits it, and its hooks would then act as that
/// workspace; every dispatch command removes it.
pub(crate) const WORKSPACE_ENV: [&str; 4] =
    ["WS_WORKSPACE", "WS_DIR", "WS_AGENT", "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE"];
```

`src/agents/claude.rs`, in `impl Agent for ClaudeAgent`:

```rust
    fn dispatch(
        &self,
        scratch: &std::path::Path,
        dirs: &[std::path::PathBuf],
        prompt: &str,
        mode: Option<crate::agents::LaunchMode>,
    ) -> anyhow::Result<Command> {
        let mut cmd = Command::new(self.binary());
        // Prompt first: `--add-dir` takes every following non-flag argument.
        cmd.arg(prompt);
        if !dirs.is_empty() {
            cmd.arg("--add-dir").args(dirs);
        }
        if let Some(m) = mode {
            cmd.args(self.mode_args(m));
        }
        cmd.current_dir(scratch);
        for key in crate::agents::WORKSPACE_ENV {
            cmd.env_remove(key);
        }
        Ok(cmd)
    }
```

`src/agents/codex.rs`, in `impl Agent for CodexAgent`:

```rust
    fn dispatch(
        &self,
        scratch: &std::path::Path,
        dirs: &[std::path::PathBuf],
        prompt: &str,
        mode: Option<crate::agents::LaunchMode>,
    ) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        for d in dirs {
            cmd.arg("--add-dir").arg(d);
        }
        if let Some(m) = mode {
            cmd.args(self.mode_args(m));
        }
        cmd.arg(prompt).current_dir(scratch);
        for key in crate::agents::WORKSPACE_ENV {
            cmd.env_remove(key);
        }
        Ok(cmd)
    }
```

(Codex's test asserts `args[..4]` are the dir pairs, so mode args go after the dirs.)

- [ ] **Step 4: Run** `cargo test --bin ws agents::` — Expected: PASS.

- [ ] **Step 5: Commit** `git commit -am "feat(agents): one-time dispatch launch for claude and codex"`

---

### Task 5: The command — CLI, input, scratch dir, locks, launch

**Files:**
- Modify: `src/cli.rs` (Cmd variant, parse arm, help line, parse tests), `src/main.rs` (dispatch arm), `src/dispatch.rs` (`run` and helpers)
- Create: `tests/dispatch.rs`

**Interfaces:**
- Consumes: `parse`, `check`, `render`, `PROMPT`, `Known`, `Target` (Tasks 1-3); `Agent::dispatch` (Task 4).
- Produces: `Cmd::Dispatch { file: Option<PathBuf>, agent: Option<String>, mode: Option<LaunchMode>, force: bool }` and `pub fn run(file: Option<PathBuf>, agent: Option<String>, mode: Option<LaunchMode>, force: bool) -> anyhow::Result<()>`.

- [ ] **Step 1: Failing CLI tests** (in `src/cli.rs` tests):

```rust
#[test]
fn dispatch_parses_file_agent_mode_and_force() {
    match parse(vec!["-dispatch".into(), "t.md".into(), "-codex".into(), "-loco".into(), "--force".into()]).unwrap() {
        Cmd::Dispatch { file, agent, mode, force } => {
            assert_eq!(file.as_deref(), Some(std::path::Path::new("t.md")));
            assert_eq!(agent.as_deref(), Some("codex"));
            assert_eq!(mode, Some(crate::agents::LaunchMode::Loco));
            assert!(force);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(parse(vec!["-dispatch".into()]).unwrap(), Cmd::Dispatch { file: None, .. }));
}

#[test]
fn dispatch_rejects_two_files_and_unknown_flags() {
    assert!(parse(vec!["-dispatch".into(), "a.md".into(), "b.md".into()]).is_err());
    assert!(parse(vec!["-dispatch".into(), "--forec".into()]).is_err());
}
```

- [ ] **Step 2: Failing integration tests** — create `tests/dispatch.rs`:

```rust
mod common;
use common::Env;

/// A workspace registered at `<root>/<name>` with a `.ws/` skeleton, created
/// through the real CLI so the registry and meta are genuine.
fn workspace(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.home.path().join("projects").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    dir
}

fn dispatch_cmd(env: &Env, shim: &std::path::Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", shim).env("WS_NO_EXEC", "1");
    c
}

fn write(env: &Env, name: &str, body: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn a_file_launches_one_session_with_every_target_granted() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let web = workspace(&env, "web");
    let f = write(&env, "t.md", "for all\n@web\n- header\n@api\n- retry\n");

    dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().success();

    let log = env.argv_log();
    assert!(log.contains(&format!("ARGS: Work through tasks.md in this directory. --add-dir {} {}", web.display(), api.display())), "{log}");
    assert!(log.contains("WSW: \n"), "no workspace identity: {log}");
    let cwd = log.lines().find_map(|l| l.strip_prefix("CWD: ")).unwrap();
    let plan = std::fs::read_to_string(std::path::Path::new(cwd).join("tasks.md")).unwrap();
    assert!(plan.find("## 1. web").unwrap() < plan.find("## 2. api").unwrap());
    assert!(plan.contains("## Unassigned"));
    let tl = std::fs::read_to_string(api.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"dispatched\""), "{tl}");
}

#[test]
fn launched_from_inside_a_workspace_it_does_not_inherit_that_identity() {
    let env = Env::new();
    let shim = env.fake_claude();
    workspace(&env, "api");
    let f = write(&env, "t.md", "@api\n- retry\n");
    dispatch_cmd(&env, &shim)
        .env("WS_WORKSPACE", "other").env("WS_DIR", "/elsewhere")
        .arg("-dispatch").arg(&f).assert().success();
    let log = env.argv_log();
    assert!(log.contains("WSW: \n") && log.contains("WSDIR: \n"), "{log}");
}

#[test]
fn a_bad_name_launches_nothing_and_takes_no_lock() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let f = write(&env, "t.md", "@apii\n- retry\n@api\n- b\n");
    let out = dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().failure().get_output().clone();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("did you mean 'api'?"), "{err}");
    assert!(env.argv_log().is_empty(), "nothing may launch");
    assert!(!api.join(".ws/local/lock").exists(), "no lock may be taken");
}

#[test]
fn the_editor_path_saves_the_text_and_launches() {
    let env = Env::new();
    let shim = env.fake_claude();
    workspace(&env, "api");
    // An "editor" that writes a task list into the file it is given.
    let ed = write(&env, "ed.sh", "#!/bin/sh\nprintf '@api\\n- retry\\n' > \"$1\"\n");
    make_exec(&ed);
    dispatch_cmd(&env, &shim).env("VISUAL", &ed).arg("-dispatch").assert().success();
    assert!(env.argv_log().contains("--add-dir"), "{}", env.argv_log());
}

#[test]
fn an_unchanged_template_cancels_quietly() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let ed = write(&env, "ed.sh", "#!/bin/sh\nexit 0\n");
    make_exec(&ed);
    let out = dispatch_cmd(&env, &shim).env("VISUAL", &ed).arg("-dispatch").assert().success().get_output().clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to dispatch"));
    assert!(env.argv_log().is_empty());
    assert!(!api.join(".ws/timeline.jsonl").exists()
        || !std::fs::read_to_string(api.join(".ws/timeline.jsonl")).unwrap().contains("dispatched"));
}

#[test]
fn an_editor_list_with_a_typo_is_kept_for_the_rerun() {
    let env = Env::new();
    let shim = env.fake_claude();
    workspace(&env, "api");
    let ed = write(&env, "ed.sh", "#!/bin/sh\nprintf '@apii\\n- retry\\n' > \"$1\"\n");
    make_exec(&ed);
    let out = dispatch_cmd(&env, &shim).env("VISUAL", &ed).arg("-dispatch").assert().failure().get_output().clone();
    let err = String::from_utf8_lossy(&out.stderr);
    let saved = err.lines().find_map(|l| l.trim().strip_prefix("your list is saved at ")).expect(&err);
    assert!(std::fs::read_to_string(saved).unwrap().contains("@apii"));
}

#[test]
fn a_rerun_launches_only_unfinished_projects() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let web = workspace(&env, "web");
    let plan = format!(
        "# Dispatch t\n\n## Protocol\n\n## 1. api — {}\n- [x] done\n\n## 2. web — {}\n- [ ] open\n",
        api.display(), web.display()
    );
    let f = write(&env, "tasks.md", &plan);
    dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().success();
    let log = env.argv_log();
    assert!(log.contains(&web.display().to_string()) && !log.contains(&format!("{} ", api.display())), "{log}");
}

#[test]
fn a_finished_rerun_says_so_and_launches_nothing() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let f = write(&env, "tasks.md", &format!("# Dispatch t\n\n## 1. api — {}\n- [x] done\n", api.display()));
    let out = dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().success().get_output().clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing left to do"));
    assert!(env.argv_log().is_empty());
}

fn make_exec(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}
```

- [ ] **Step 3: Run** `cargo test --test dispatch` and `cargo test --bin ws cli::` — Expected: FAIL (no `-dispatch`).

- [ ] **Step 4: Implement the CLI**

`src/cli.rs` — add to `enum Cmd`:

```rust
    /// `ws -dispatch [<file>]` — one session working several workspaces' tasks.
    Dispatch {
        file: Option<std::path::PathBuf>,
        agent: Option<String>,
        mode: Option<crate::agents::LaunchMode>,
        force: bool,
    },
```

Parse arm, beside `"-adopt"`:

```rust
        "-dispatch" => {
            let (mut file, mut agent, mut mode, mut force) = (None, None, None, false);
            for a in it {
                match a.as_str() {
                    "-claude" => agent = Some("claude".to_string()),
                    "-codex" => agent = Some("codex".to_string()),
                    "-loco" | "--loco" => mode = Some(crate::agents::LaunchMode::Loco),
                    "-sane" | "--sane" => mode = Some(crate::agents::LaunchMode::Sane),
                    "--force" => force = true,
                    other if other.starts_with('-') && other != "-" => bail!("unexpected argument: {other}"),
                    other if file.is_none() => file = Some(std::path::PathBuf::from(other)),
                    other => bail!("usage: ws -dispatch [<tasks.md>] [-claude|-codex] [-loco|-sane] [--force] (unexpected: {other})"),
                }
            }
            Ok(Cmd::Dispatch { file, agent, mode, force })
        }
```

Help text, new section after "Manage" (before "Worktrees"):

```
         Dispatch\n\
         \x20 ws -dispatch [<tasks.md>]     work several workspaces' tasks in one session:\n\
         \x20                                `@name` lines start each project's list; no\n\
         \x20                                file opens $EDITOR (-claude|-codex, -loco|-sane, --force)\n\
         \n\
```

`src/main.rs` match arm: `Cmd::Dispatch { file, agent, mode, force } => dispatch::run(file, agent, mode, force)?,`

- [ ] **Step 5: Implement `run`** in `src/dispatch.rs`:

```rust
use anyhow::{bail, Context, Result};
use std::path::Path;

const TEMPLATE: &str = "\
# One section per workspace, worked in this order. Lines starting with @name
# begin a section; every non-blank line under it is one task. Text above the
# first @name is shown to you at the end, not acted on. Save and close to start;
# leave this unchanged to cancel.

";

/// Dispatch directories older than this are removed on the next run — the same
/// fortnight crash snapshots are kept.
const SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 3600);

fn dispatch_root() -> std::path::PathBuf {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => dirs::home_dir().unwrap_or_else(|| ".".into()).join(".cache"),
    };
    base.join("ws").join("dispatch")
}

pub fn run(
    file: Option<std::path::PathBuf>,
    agent: Option<String>,
    mode: Option<crate::agents::LaunchMode>,
    force: bool,
) -> Result<()> {
    let root = dispatch_root();
    sweep(&root);

    // A rerun of a generated plan reuses its own directory; anything else gets
    // a new one.
    let (text, scratch, from_editor) = match &file {
        Some(f) => {
            let text = std::fs::read_to_string(f).with_context(|| format!("cannot read {}", f.display()))?;
            let scratch = if text.starts_with(PLAN_TITLE) {
                f.canonicalize()?.parent().map(Path::to_path_buf).context("plan has no directory")?
            } else {
                new_scratch(&root)?
            };
            (text, scratch, false)
        }
        None => {
            let scratch = new_scratch(&root)?;
            let input = scratch.join("input.md");
            std::fs::write(&input, TEMPLATE)?;
            edit(&input)?;
            let text = std::fs::read_to_string(&input)?;
            if text.trim().is_empty() || text == TEMPLATE {
                let _ = std::fs::remove_dir_all(&scratch);
                println!("nothing to dispatch");
                return Ok(());
            }
            (text, scratch, true)
        }
    };

    let parsed = parse(&strip_template_comments(&text));
    let all: Vec<String> = crate::registry::all().into_iter().map(|(n, _)| n).collect();
    let targets = match check(&parsed, &known, &all, force) {
        Ok(t) => t,
        Err(errs) => {
            for e in &errs {
                eprintln!("ws: {e}");
            }
            if from_editor {
                eprintln!("  your list is saved at {}", scratch.join("input.md").display());
                eprintln!("  fix it and run: ws -dispatch {}", scratch.join("input.md").display());
            }
            bail!("nothing dispatched ({} problem(s))", errs.len());
        }
    };
    if targets.is_empty() {
        println!("nothing left to do");
        return Ok(());
    }

    // Lock every target before the agent starts, so opening one elsewhere during
    // the run reports it busy. On a partial failure the guards already taken drop
    // (and remove their files) as `?` returns.
    let mut guards = Vec::new();
    for t in &targets {
        guards.push(crate::lock::acquire(&t.path.join(".ws/local/lock"), force)?);
    }

    let stamp = crate::now_iso();
    std::fs::write(scratch.join("tasks.md"), render(&targets, &parsed.preamble, &stamp))?;
    let actor = crate::actors::actor_slug();
    for t in &targets {
        let _ = crate::timeline::record(
            &t.path.join(".ws/timeline.jsonl"),
            "dispatched",
            &actor,
            serde_json::json!({ "dispatch": scratch.display().to_string(), "tasks": t.tasks.len() }),
        );
    }

    let cfg = crate::config::load();
    let agent = crate::agents::for_id(agent.as_deref().unwrap_or(&cfg.default_agent))?;
    let dirs: Vec<std::path::PathBuf> = targets.iter().map(|t| t.path.clone()).collect();
    let cmd = agent.dispatch(&scratch, &dirs, PROMPT, mode)?;
    println!("dispatching {} project(s) — plan at {}", targets.len(), scratch.join("tasks.md").display());

    // The agent inherits this pid, so the locks stay held until it exits and are
    // then stale — exactly how `ws <name>` hands its lock over.
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

/// Registry + meta + lock, for `check`.
fn known(name: &str) -> Option<Known> {
    let path = crate::registry::lookup(name)?;
    if !path.join(".ws").is_dir() {
        return None;
    }
    let archived = crate::meta::read(&path.join(".ws/workspace.toml")).archived;
    let held_by = crate::lock::live_pid_checked(&path.join(".ws/local/lock")).ok().flatten();
    Some(Known { path, archived, held_by })
}

/// Drop the template's own `#` comment lines — but only those, and only in
/// editor input: a `# heading` the user pasted below is theirs.
fn strip_template_comments(text: &str) -> String {
    match text.strip_prefix(TEMPLATE) {
        Some(rest) => rest.to_string(),
        None => text.to_string(),
    }
}

fn new_scratch(root: &Path) -> Result<std::path::PathBuf> {
    let dir = root.join(crate::now_iso().replace(':', "-"));
    let dir = if dir.exists() { root.join(format!("{}-{}", dir.display(), std::process::id())) } else { dir };
    crate::atomic::create_private_dir_all(&dir)?;
    Ok(dir)
}

/// `$VISUAL`, else `$EDITOR`, else `vi`, through `sh -c` so an editor with its
/// own arguments (`code -w`) works; the file is a positional, never spliced
/// into the command string.
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
        .with_context(|| format!("could not run editor {editor}"))?;
    if !status.success() {
        bail!("editor exited with {status}; your list is saved at {}", path.display());
    }
    Ok(())
}

/// Remove dispatch directories untouched for a fortnight. Best effort.
fn sweep(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > SWEEP_AFTER);
        if old && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}
```

Make `commands::exec` `pub(crate)` (it is `fn exec` today, `#[cfg(unix)]` and `#[cfg(not(unix))]` variants — change both).

In the `a_bad_name…` and `a_file…` tests the task file is not in the dispatch root, so `new_scratch` is used; in `a_rerun…` the plan's own directory is reused, which is `env.home` — acceptable for a test, and the real flow always passes a file inside the dispatch root.

- [ ] **Step 6: Run** `cargo test --test dispatch && cargo test --bin ws` — Expected: PASS. Fix mismatches between the exact strings in the tests and the messages (the tests are the contract).

- [ ] **Step 7: Run gates** `cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-targets --all-features` — Expected: clean. The help-coverage test must now see `-dispatch`.

- [ ] **Step 8: Commit** `git add -A src tests && git commit -m "feat: ws -dispatch — one session, several workspaces, in order"`

---

### Task 6: Real-binary check, Codex sandbox, docs, release

**Files:** `README.md`, `CHANGELOG.md`, `Cargo.toml`, `Cargo.lock`, notebook.

- [ ] **Step 1: Pty check of the real binary** (memory: picker/launch key paths need a real run). Build release, then in the scratchpad: two throwaway adopted workspaces under a temp `HOME`/`XDG_CONFIG_HOME`/`WS_ROOT`, `WS_CLAUDE_BIN` pointing at a stub that logs argv and `cat tasks.md`. Run `script -q /dev/null ws -dispatch` with `VISUAL` set to a stub that writes a list; confirm argv, the plan, and that an unchanged template prints `nothing to dispatch`.

- [ ] **Step 2: Codex sandbox question (spec open question).** With the real `codex` binary: `codex --add-dir <tmpdir> --help`-style check is not enough; run `codex exec --add-dir <tmp>/a "create file ok.txt in <tmp>/a"` from another temp dir (non-interactive, no posture flag) and see whether the file appears. If it does not, make `run` apply `LaunchMode::Sane` when `agent.id() == "codex"` and `mode.is_none()`, printing `ws: codex dispatch runs -sane so it can write to the granted directories`, plus a unit test. Record the result in the notebook either way.

- [ ] **Step 3: README** — add after "Messages between workspaces":

```markdown
## Dispatching tasks to several workspaces

    ws -dispatch            # opens $EDITOR: paste the list, save, close
    ws -dispatch tasks.md   # or read it from a file

A line starting with `@name` begins that workspace's tasks; every non-blank line
under it is one task. Projects are worked in the order written, in **one**
session started outside all of them with each named workspace granted. Text
above the first `@name` is not acted on; you are asked about it at the end.

Every name is checked before anything starts — an unknown name (with a "did you
mean"), a duplicate, an empty section, an archived workspace, or one open in
another session (`--force` takes it over) refuses the whole run, and the list
you typed is kept so you can fix it and rerun.

The agent reads each workspace's `.ws/` notes and context files first, leaves
its changes **uncommitted**, ticks tasks off in the plan, writes questions under
the tasks it could not finish, and adds an entry to that workspace's notebook.
The plan lives in `~/.cache/ws/dispatch/<time>/tasks.md`; `ws -dispatch <that
file>` resumes the unticked tasks. Plans older than two weeks are removed.
```

Also add `ws -dispatch [<tasks.md>]      Work several workspaces' tasks in one session` to the "Useful commands" block.

- [ ] **Step 4: CHANGELOG + version** — under `## [Unreleased]` → new `## [0.14.0] — <date>` with `### Added` entry: "`ws -dispatch`: paste one list of tasks for several workspaces (`@name` sections) into `$EDITOR`, or pass a file, and one agent session works them in order, leaving changes uncommitted and notes in each workspace." Bump `Cargo.toml` to `0.14.0`; run `cargo build` (not `--locked`, memory: a bump breaks `--locked` until the lock is refreshed) so `Cargo.lock` updates.

- [ ] **Step 5: Gates** — clippy + full test suite + `cargo build --release --locked` — all clean.

- [ ] **Step 6: Commit** `git commit -am "release: v0.14.0 — ws -dispatch"`

- [ ] **Step 7: Release per `docs/releasing.md`** — merge `feat/dispatch` into `main` (fast-forward), push, wait for CI green, cross-compile check per memory, tag `v0.14.0`, push tag, wait for the release workflow, verify the draft (minisign signature over `SHA256SUMS`, checksums, binary `--version` = 0.14.0), publish the draft.
