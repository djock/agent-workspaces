//! `ws -dispatch`: one agent session that works tasks for several workspaces
//! in order. See docs/superpowers/specs/2026-09-28-ws-dispatch-design.md.

use std::path::PathBuf;

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
            let numbered =
                h.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
