use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: ToolInput,
    /// Set by the agent on a `Stop` payload when the turn is only ending
    /// *because* a Stop hook already blocked the previous one. Both Claude and
    /// Codex send it (verified in Codex 0.145.0's payload schema), and it is the
    /// only loop guard either offers: a hook that blocks again here is asking to
    /// be re-entered forever.
    #[serde(default)]
    pub stop_hook_active: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ToolInput {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub file_path: String,
    // NotebookEdit payloads name their target `notebook_path`, not `file_path`;
    // without this field the redaction hook would match the tool and then see
    // no path at all.
    #[serde(default)]
    pub notebook_path: String,
}

pub fn parse(raw: &str) -> HookInput {
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn read_stdin() -> HookInput {
    match std::io::read_to_string(std::io::stdin()) {
        Ok(s) => parse(&s),
        Err(_) => HookInput::default(),
    }
}

pub fn additional_context(event: &str, context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
    .to_string()
}

pub fn decision_block(reason: &str) -> String {
    json!({ "decision": "block", "reason": reason }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_fields_and_defaults() {
        let h = parse(r#"{"source":"startup","cwd":"/x","tool_name":"Bash","tool_input":{"command":"ls -la"}}"#);
        assert_eq!(h.source, "startup");
        assert_eq!(h.cwd, "/x");
        assert_eq!(h.tool_name, "Bash");
        assert_eq!(h.tool_input.command, "ls -la");
        assert_eq!(h.prompt, ""); // default
        assert!(h.agent_id.is_none());
        assert!(!h.stop_hook_active, "absent means this is a first stop");
    }

    #[test]
    fn parses_stop_hook_active() {
        let h = parse(r#"{"hook_event_name":"Stop","stop_hook_active":true}"#);
        assert!(h.stop_hook_active);
    }

    #[test]
    fn parse_garbage_is_default() {
        let h = parse("not json");
        assert_eq!(h.source, "");
        assert_eq!(h.prompt, "");
    }

    #[test]
    fn additional_context_shape_and_escaping() {
        let s = additional_context("SessionStart", "line1\n\"quoted\"");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "line1\n\"quoted\"");
    }

    #[test]
    fn decisions() {
        let b: serde_json::Value = serde_json::from_str(&decision_block("do X")).unwrap();
        assert_eq!(b["decision"], "block");
        assert_eq!(b["reason"], "do X");
    }
}
