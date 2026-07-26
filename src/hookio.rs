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
}

#[derive(Debug, Default, Deserialize)]
pub struct ToolInput {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub file_path: String,
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

pub fn decision_approve() -> String {
    json!({ "decision": "approve" }).to_string()
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
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decision_approve()).unwrap()["decision"],
            "approve"
        );
        let b: serde_json::Value = serde_json::from_str(&decision_block("do X")).unwrap();
        assert_eq!(b["decision"], "block");
        assert_eq!(b["reason"], "do X");
    }
}
