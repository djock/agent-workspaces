//! Conversation lineage — which agent conversation succeeded which, and why.
//!
//! A workspace outlives any single agent conversation. Sessions get replaced by
//! `--fresh`, and work moves between Claude and Codex. Before this, the only
//! record of continuity was "the newest file in `.ws/handoffs/` by mtime", which
//! answers *what to read next* and nothing about *what happened*: no order, no
//! reasons, and no way to tell a deliberate rotation from an agent switch.
//!
//! The record is `rotated` and `agent-switch` events already in
//! `.ws/timeline.jsonl`, which is append-only and `merge=union`, so two
//! checkouts of the same workspace merge their lineages instead of conflicting.
//! This module only *reads* that file — nothing here is the source of truth,
//! which is why a malformed line is skipped rather than fatal.

use anyhow::Result;
use serde_json::json;

use crate::workspace::Workspace;

/// Record that `to` replaced `from` for `agent`. Best-effort by design: failing a
/// launch because a history line could not be appended would trade a working
/// session for a bookkeeping entry.
pub fn record_rotation(ws: &Workspace, agent: &str, from: Option<&str>, to: &str, reason: &str) {
    let _ = crate::timeline::record(
        &ws.timeline(),
        "rotated",
        &crate::actors::actor_slug_in(&ws.root),
        json!({
            "agent": agent,
            "from": from,
            "to": to,
            "reason": reason,
        }),
    );
}

#[derive(Debug, PartialEq)]
pub enum Link {
    /// One conversation replaced another for the same agent.
    Rotated { ts: String, agent: String, from: Option<String>, to: String, reason: String },
    /// Work moved from one agent to another, optionally naming the handoff read.
    Switch { ts: String, from: String, to: String, handoff: Option<String> },
}

impl Link {
    fn ts(&self) -> &str {
        match self {
            Link::Rotated { ts, .. } | Link::Switch { ts, .. } => ts,
        }
    }
}

fn s(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string())
}

/// Parse the lineage out of a timeline file's contents.
///
/// Takes the text rather than a path so the parser is testable without a
/// workspace on disk. Unparseable or unrecognised lines are skipped: this file is
/// union-merged across checkouts and appended to by several writers, so a single
/// bad line is an expected condition, not a reason to refuse the whole history.
pub fn parse(timeline: &str) -> Vec<Link> {
    let mut out = Vec::new();
    for line in timeline.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ts = s(&v, "ts").unwrap_or_default();
        match v.get("kind").and_then(|k| k.as_str()) {
            Some("rotated") => {
                // `to` is the only required field; without it the line says
                // nothing about lineage.
                if let Some(to) = s(&v, "to") {
                    out.push(Link::Rotated {
                        ts,
                        agent: s(&v, "agent").unwrap_or_else(|| "?".into()),
                        from: s(&v, "from"),
                        to,
                        reason: s(&v, "reason").unwrap_or_else(|| "?".into()),
                    });
                }
            }
            Some("agent-switch") => {
                if let (Some(from), Some(to)) = (s(&v, "from"), s(&v, "to")) {
                    out.push(Link::Switch { ts, from, to, handoff: s(&v, "handoff") });
                }
            }
            _ => {}
        }
    }
    // Timeline order is append order, which is chronological in practice; sorting
    // by ts makes a union-merged file from two checkouts read correctly too.
    out.sort_by(|a, b| a.ts().cmp(b.ts()));
    out
}

/// First 12 *characters*, not bytes.
///
/// `&id[..12]` panicked ("byte index 12 is not a char boundary") for any id whose
/// 12th byte lands mid-character — and ids arrive from `timeline.jsonl`, which is
/// `merge=union` across checkouts and appended to by anything. `len()` counts
/// bytes, so the guard did not guard.
fn short(id: &str) -> String {
    let mut it = id.chars();
    let head: String = it.by_ref().take(12).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Human-readable lineage. `current` maps agent id → its live session id, so the
/// conversation you are in is marked rather than left for the reader to work out.
pub fn render(links: &[Link], current: &[(String, String)]) -> String {
    if links.is_empty() {
        return "no conversation history recorded yet\n".to_string();
    }
    let is_current = |agent: &str, id: &str| {
        current.iter().any(|(a, c)| a == agent && c == id)
    };
    let mut out = String::new();
    for l in links {
        match l {
            Link::Rotated { ts, agent, from, to, reason } => {
                let marker = if is_current(agent, to) { "  ← current" } else { "" };
                match from {
                    Some(f) => out.push_str(&format!(
                        "{ts}  {agent}: {} → {} ({reason}){marker}\n",
                        short(f),
                        short(to)
                    )),
                    None => out.push_str(&format!(
                        "{ts}  {agent}: {} ({reason}){marker}\n",
                        short(to)
                    )),
                }
            }
            Link::Switch { ts, from, to, handoff } => {
                let via = match handoff {
                    Some(h) => format!(" via {h}"),
                    None => String::new(),
                };
                out.push_str(&format!("{ts}  agent: {from} → {to}{via}\n"));
            }
        }
    }
    out
}

pub fn run(name: Option<String>) -> Result<()> {
    let (name, root) = crate::commands::current_or_named(name)?;
    let ws = Workspace { name, root };
    let text = std::fs::read_to_string(ws.timeline()).unwrap_or_default();
    let links = parse(&text);

    // The live id per agent, so `← current` is accurate rather than guessed.
    let mut current = Vec::new();
    for agent in ["claude", "codex"] {
        if let Some(id) = crate::contract::read_session_id(&ws.state_toml(), agent) {
            current.push((agent.to_string(), id));
        }
    }
    print!("{}", render(&links, &current));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
{"ts":"2026-07-27T10:00:00Z","kind":"created","actor":"a","agent":"claude"}
{"ts":"2026-07-27T10:01:00Z","kind":"rotated","actor":"a","agent":"claude","from":null,"to":"aaaaaaaaaaaaaaaa-1","reason":"first"}
{"ts":"2026-07-27T11:00:00Z","kind":"opened","actor":"a"}
{"ts":"2026-07-27T12:00:00Z","kind":"rotated","actor":"a","agent":"claude","from":"aaaaaaaaaaaaaaaa-1","to":"bbbbbbbbbbbbbbbb-2","reason":"fresh"}
{"ts":"2026-07-27T13:00:00Z","kind":"agent-switch","actor":"a","from":"claude","to":"codex","handoff":"rotate-1300.md"}
"#;

    #[test]
    fn only_lineage_events_are_extracted_in_order() {
        let links = parse(SAMPLE);
        assert_eq!(links.len(), 3, "created/opened are not lineage: {links:?}");
        match &links[0] {
            Link::Rotated { from, to, reason, agent, .. } => {
                assert!(from.is_none(), "the first conversation has no predecessor");
                assert_eq!(to, "aaaaaaaaaaaaaaaa-1");
                assert_eq!(reason, "first");
                assert_eq!(agent, "claude");
            }
            other => panic!("{other:?}"),
        }
        match &links[1] {
            Link::Rotated { from, to, reason, .. } => {
                assert_eq!(from.as_deref(), Some("aaaaaaaaaaaaaaaa-1"), "the chain must link");
                assert_eq!(to, "bbbbbbbbbbbbbbbb-2");
                assert_eq!(reason, "fresh");
            }
            other => panic!("{other:?}"),
        }
        match &links[2] {
            Link::Switch { from, to, handoff, .. } => {
                assert_eq!(from, "claude");
                assert_eq!(to, "codex");
                assert_eq!(handoff.as_deref(), Some("rotate-1300.md"));
            }
            other => panic!("{other:?}"),
        }
    }

    /// `timeline.jsonl` is union-merged across checkouts and appended by several
    /// writers, so a truncated or foreign line is an expected condition. Losing
    /// the whole history to one bad line would be worse than skipping it.
    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let text = format!("{SAMPLE}\nnot json at all\n{{\"kind\":\"rotated\"}}\n\n");
        let links = parse(&text);
        assert_eq!(links.len(), 3, "a rotated line with no `to` says nothing and is dropped");
    }

    /// A union merge can interleave two checkouts' lines out of append order.
    #[test]
    fn out_of_order_lines_are_sorted_chronologically() {
        let text = concat!(
            r#"{"ts":"2026-07-27T12:00:00Z","kind":"rotated","agent":"claude","from":"x","to":"y","reason":"fresh"}"#, "\n",
            r#"{"ts":"2026-07-27T10:00:00Z","kind":"rotated","agent":"claude","from":null,"to":"x","reason":"first"}"#, "\n",
        );
        let links = parse(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].ts(), "2026-07-27T10:00:00Z", "earliest first: {links:?}");
    }

    #[test]
    fn the_live_conversation_is_marked_and_others_are_not() {
        let links = parse(SAMPLE);
        let current = vec![("claude".to_string(), "bbbbbbbbbbbbbbbb-2".to_string())];
        let out = render(&links, &current);

        let marked: Vec<&str> = out.lines().filter(|l| l.contains("← current")).collect();
        assert_eq!(marked.len(), 1, "exactly one line is current:\n{out}");
        assert!(marked[0].contains("bbbbbbbbbbbb"), "and it is the live id: {}", marked[0]);
        assert!(out.contains("claude → codex"), "the agent switch is shown:\n{out}");
        assert!(out.contains("via rotate-1300.md"), "and what was handed off:\n{out}");
    }

    #[test]
    fn an_empty_history_says_so_rather_than_printing_nothing() {
        assert!(render(&[], &[]).contains("no conversation history"));
    }

    #[test]
    fn long_ids_are_shortened_but_short_ones_are_left_alone() {
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("0123456789abcdef-more"), "0123456789ab…");
    }
}
