use serde::Deserialize;
use serde_json::json;

/// Which agent's hook contract a payload came in, and so which one the answer
/// must go back in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Claude's snake_case payload and `hookSpecificOutput`; Codex speaks it too.
    #[default]
    Claude,
    /// agy's camelCase payload (`conversationId`, `toolCall`) and its
    /// `injectSteps` / `decision: "continue"` answers.
    Agy,
}

#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(skip)]
    pub dialect: Dialect,
    /// agy's `invocationNum`: which model call of the turn a PreInvocation is.
    #[serde(skip)]
    pub invocation_num: Option<u64>,
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
    /// The agent's own transcript. For Codex this is the session rollout, which
    /// is also where Codex records its rate-limit windows (Codex CLI 0.155.1
    /// sends it on every hook payload).
    #[serde(default)]
    pub transcript_path: String,
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
    let v: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return HookInput::default(),
    };
    // Claude and Codex both send snake_case `hook_event_name`; agy sends
    // camelCase on every payload.
    if v.get("hookEventName").is_some() || v.get("conversationId").is_some() {
        return from_agy(&v);
    }
    serde_json::from_value(v).unwrap_or_default()
}

/// agy's payload, read into the fields the handlers already use.
///
/// Field names seen in live agy 1.5.16 payloads: `conversationId`, `cwd`,
/// `hookEventName`, `lastUserInput`, `transcriptPath`, `invocationNum`,
/// `continuationActive` (Stop's loop guard), and `toolCall{name,args}` with
/// `args.CommandLine` for `run_command` and `args.Path` for `write_file`. The
/// edit tools have not been seen live, so the other path spellings agy's tools
/// use (`TargetFile`, `AbsolutePath`) are read too.
fn from_agy(v: &serde_json::Value) -> HookInput {
    let s = |key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let args = v.pointer("/toolCall/args");
    let arg = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.and_then(|a| a.get(*k)).and_then(|x| x.as_str()))
            .unwrap_or_default()
            .to_string()
    };
    HookInput {
        dialect: Dialect::Agy,
        invocation_num: v.get("invocationNum").and_then(|n| n.as_u64()),
        session_id: s("conversationId"),
        cwd: s("cwd"),
        // agy sends no source. Its SessionStart is a conversation starting; a
        // resume is told apart by the recorded id, as for Codex.
        source: if s("hookEventName") == "SessionStart" { "startup".into() } else { String::new() },
        agent_id: None,
        prompt: s("lastUserInput"),
        tool_name: v
            .pointer("/toolCall/name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        tool_input: ToolInput {
            command: arg(&["CommandLine"]),
            file_path: arg(&["Path", "TargetFile", "AbsolutePath", "FilePath"]),
            notebook_path: String::new(),
        },
        stop_hook_active: v.get("continuationActive").and_then(|b| b.as_bool()).unwrap_or(false),
        transcript_path: s("transcriptPath"),
    }
}

/// A handler's answer, put in the payload's dialect.
///
/// The handlers write Claude's shapes. For agy:
/// - `hookSpecificOutput.additionalContext` becomes an `injectSteps` system
///   message, and a `systemMessage` banner a step the model never reads
///   (`metadata.user_markdown` with empty text);
/// - Stop's `decision: "block"` becomes `decision: "continue"`, which is how
///   agy spells "don't stop yet";
/// - no answer becomes `{}`. agy reads every hook's stdout as JSON, and an
///   answer is what keeps a hook from being taken as a failure.
pub fn render(h: &HookInput, out: Option<String>) -> Option<String> {
    if h.dialect == Dialect::Claude {
        return out;
    }
    let v: serde_json::Value =
        out.as_deref().and_then(|o| serde_json::from_str(o).ok()).unwrap_or_else(|| json!({}));
    let mut steps = Vec::new();
    if let Some(ctx) = v.pointer("/hookSpecificOutput/additionalContext").and_then(|c| c.as_str()) {
        steps.push(json!({ "systemMessage": { "systemMessage": ctx } }));
    }
    if let Some(msg) = v.get("systemMessage").and_then(|m| m.as_str()) {
        steps.push(json!({
            "systemMessage": { "systemMessage": "", "metadata": { "user_markdown": msg } }
        }));
    }
    let answer = if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
        json!({ "decision": "continue", "reason": v.get("reason").cloned().unwrap_or(json!("")) })
    } else if !steps.is_empty() {
        json!({ "injectSteps": steps })
    } else {
        json!({})
    };
    Some(answer.to_string())
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

/// `additional_context` plus a `systemMessage`: shown to the user in the
/// terminal, never sent to the model. Both Claude and Codex (0.161.0) accept it.
pub fn additional_context_with_message(event: &str, context: &str, message: &str) -> String {
    json!({
        "systemMessage": message,
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
        let h = parse(
            r#"{"source":"startup","cwd":"/x","tool_name":"Bash","tool_input":{"command":"ls -la"}}"#,
        );
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

    /// Trimmed from a live agy 1.5.16 PostToolUse payload.
    #[test]
    fn an_agy_payload_is_read_into_the_shared_fields() {
        let h = parse(
            r#"{"conversationId":"291f","cwd":"/w","hookEventName":"PostToolUse","error":"",
                "lastUserInput":"hi","transcriptPath":"/t.jsonl",
                "toolCall":{"args":{"Path":"/w/out.txt","Content":"done","Overwrite":false},"id":"c1","name":"write_file"}}"#,
        );
        assert_eq!(h.dialect, Dialect::Agy);
        assert_eq!(h.session_id, "291f");
        assert_eq!(h.tool_name, "write_file");
        assert_eq!(h.tool_input.file_path, "/w/out.txt");
        assert_eq!(h.prompt, "hi");
        assert_eq!(h.transcript_path, "/t.jsonl");
        assert_eq!(h.source, "");

        let start = parse(r#"{"conversationId":"291f","hookEventName":"SessionStart"}"#);
        assert_eq!(start.source, "startup");
        let stop = parse(r#"{"hookEventName":"Stop","continuationActive":true}"#);
        assert!(stop.stop_hook_active);
        let pre = parse(r#"{"hookEventName":"PreInvocation","invocationNum":2}"#);
        assert_eq!(pre.invocation_num, Some(2));
        let shell = parse(
            r#"{"hookEventName":"PreToolUse","toolCall":{"name":"run_command","args":{"CommandLine":"ls"}}}"#,
        );
        assert_eq!(shell.tool_input.command, "ls");
    }

    #[test]
    fn answers_are_rendered_in_the_payload_dialect() {
        let claude = parse(r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(render(&claude, None), None);
        assert_eq!(render(&claude, Some("x".into())), Some("x".into()));

        let agy = parse(r#"{"hookEventName":"Stop"}"#);
        assert_eq!(render(&agy, None).unwrap(), "{}");
        let v: serde_json::Value =
            serde_json::from_str(&render(&agy, Some(decision_block("write it"))).unwrap()).unwrap();
        assert_eq!(v, json!({"decision": "continue", "reason": "write it"}));

        let ctx = additional_context_with_message("SessionStart", "the ctx", "banner");
        let v: serde_json::Value = serde_json::from_str(&render(&agy, Some(ctx)).unwrap()).unwrap();
        assert_eq!(v["injectSteps"][0]["systemMessage"]["systemMessage"], "the ctx");
        assert_eq!(v["injectSteps"][1]["systemMessage"]["metadata"]["user_markdown"], "banner");
        assert_eq!(v["injectSteps"][1]["systemMessage"]["systemMessage"], "");
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
