use anyhow::Result;
use std::path::Path;
use std::process::Command;

use crate::agents::{Agent, LaunchCtx};
use crate::workspace::Workspace;

pub struct CodexAgent;

fn marker_present(ws: &Workspace) -> bool {
    std::fs::read_to_string(ws.state_toml())
        .ok()
        .and_then(|s| toml::from_str::<toml::Table>(&s).ok())
        .and_then(|t| t.get("codex").and_then(|c| c.get("launched")).and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// Record "codex has been launched here" in `.ws/local/state.toml`.
///
/// C2: this shares the file with `contract::write_session_id` (hook handlers),
/// so it must use the same clobber-safe path — per-process temp name, cleanup
/// on failure, and a refusal to overwrite a `state.toml` that failed to parse.
/// It previously used a fixed `state.toml.tmp`, replaced an unparseable file
/// with a fresh table, and dropped every other agent's `session_id` on the way.
fn record_marker(ws: &Workspace) -> Result<()> {
    let state = ws.state_toml();
    let mut t = crate::contract::read_state_table(&state)?;
    let mut e = match t.get("codex").and_then(|v| v.as_table()) {
        Some(existing) => existing.clone(),
        None => toml::Table::new(),
    };
    e.insert("launched".into(), toml::Value::Boolean(true));
    t.insert("codex".into(), toml::Value::Table(e));
    crate::contract::write_state_table(&state, &t)
}

impl Agent for CodexAgent {
    fn id(&self) -> &'static str { "codex" }
    fn binary(&self) -> String { std::env::var("WS_CODEX_BIN").unwrap_or_else(|_| "codex".into()) }
    fn is_installed(&self) -> bool {
        Command::new(self.binary()).arg("--version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }
    fn context_file(&self) -> &'static str { "AGENTS.md" }
    fn has_prior_session(&self, ws: &Workspace) -> bool { marker_present(ws) }

    fn hooks_config_path(&self) -> std::path::PathBuf {
        crate::hooksetup::codex_hooks_path()
    }

    fn prompts_dir(&self) -> std::path::PathBuf {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join(".codex").join("prompts")
    }

    fn prompt_filename(&self, base: &str) -> String {
        format!("ws-{base}.md")
    }

    fn hook_trust_note(&self) -> Option<&'static str> {
        Some("Run `/hooks` in Codex to trust the ws hooks before they take effect.")
    }
    fn launch(&self, ws: &Workspace, ctx: &LaunchCtx) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        if ctx.fresh || !marker_present(ws) {
            record_marker(ws)?;               // fresh: `codex`
        } else {
            cmd.arg("resume").arg("--last");  // resume most recent in this cwd
        }
        cmd.current_dir(&ws.root)
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
        Ok(cmd)
    }

    fn headless(&self, ws: &Workspace, prompt: &str, ctx: &LaunchCtx, out_file: &Path) -> Result<Command> {
        let mut cmd = Command::new(self.binary());
        cmd.arg("exec");
        if !ctx.fresh && marker_present(ws) {
            cmd.arg("resume").arg("--last");
        } else if ctx.fresh {
            // Mirror `launch`'s fresh path: record the marker ourselves so
            // task 2..N of THIS drain resume it, instead of every task
            // running fresh because no prior *interactive* launch happened
            // to record one.
            record_marker(ws)?;
        }
        cmd.arg(prompt)
            .arg("-C")
            .arg(&ws.root)
            .arg("--color")
            .arg("never")
            // C1: `codex exec` prints its banner and the model's reasoning to
            // stdout and exits 0 whenever the CLI itself didn't error — even
            // when the model refused the task outright. `--json -o <file>`
            // is the only channel that carries a real success signal: the
            // final assistant message, written to `out_file` and nowhere
            // else. `headless_succeeded` reads that file, never stdout.
            .arg("--json")
            .arg("-o")
            .arg(out_file);
        cmd.current_dir(&ws.root)
            .env("WS_WORKSPACE", &ws.name)
            .env("WS_DIR", &ws.root)
            .env("WS_ROOT", &ctx.sessions_root);
        Ok(cmd)
    }

    fn headless_succeeded(&self, out: &std::process::Output, out_file: &Path) -> bool {
        // Safety Model rule 6: failure is "non-zero exit, OR the
        // --output-last-message file missing or empty". `codex exec` exits 0
        // and writes a chatty stdout banner even on an outright refusal, so
        // stdout carries no signal at all — only `out_file`, which the CLI
        // itself writes, can be trusted.
        if !out.status.success() {
            return false;
        }
        match std::fs::read(out_file) {
            Ok(bytes) => !bytes.is_empty(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, LaunchCtx};
    use crate::workspace::Workspace;
    use tempfile::TempDir;

    fn ws_at(d: &std::path::Path) -> Workspace {
        std::fs::create_dir_all(d.join(".ws/local")).unwrap();
        Workspace { name: "proj".into(), root: d.to_path_buf() }
    }
    fn args(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn fresh_launches_codex_and_records_marker() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: "/root".into() };
        let cmd = CodexAgent.launch(&ws, &ctx).unwrap();
        assert!(args(&cmd).is_empty(), "fresh codex takes no resume args");
        assert!(CodexAgent.has_prior_session(&ws), "marker recorded after fresh launch");
    }

    #[test]
    fn resume_uses_resume_last() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        // simulate a prior launch
        CodexAgent.launch(&ws, &LaunchCtx { fresh: true, sessions_root: "/r".into() }).unwrap();
        let cmd = CodexAgent.launch(&ws, &LaunchCtx { fresh: false, sessions_root: "/r".into() }).unwrap();
        assert_eq!(args(&cmd), vec!["resume", "--last"]);
    }

    /// C2. `record_marker` and `contract::write_session_id` write the same
    /// file from different processes; neither may throw away the other's keys,
    /// and neither may replace a `state.toml` it could not parse.
    #[test]
    fn record_marker_preserves_other_keys_and_refuses_a_corrupt_state_toml() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let state = ws.state_toml();

        // A hook handler got there first.
        crate::contract::write_session_id(&state, "claude", "abc-123").unwrap();
        record_marker(&ws).unwrap();
        assert_eq!(
            crate::contract::read_session_id(&state, "claude"),
            Some("abc-123".into()),
            "another agent's session id must survive the codex marker"
        );
        assert!(marker_present(&ws));
        // ...and the reverse direction: a later session-id write keeps it.
        crate::contract::write_session_id(&state, "codex", "xyz").unwrap();
        assert!(marker_present(&ws), "the launched marker must survive a session_id write");

        // Corrupt → refuse, byte for byte.
        std::fs::write(&state, "not toml {{{").unwrap();
        assert!(record_marker(&ws).is_err(), "must not replace an unparseable state.toml");
        assert!(
            crate::contract::write_session_id(&state, "claude", "z").is_err(),
            "and neither may write_session_id"
        );
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "not toml {{{");
    }

    /// The temp file must not be a path two processes share.
    #[test]
    fn record_marker_uses_a_per_process_temp_name() {
        let d = TempDir::new().unwrap();
        let ws = ws_at(d.path());
        let fixed = ws.state_toml().with_extension("toml.tmp");
        // Squat the old fixed temp path with a file we own.
        std::fs::write(&fixed, "another process was mid-write").unwrap();
        record_marker(&ws).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fixed).unwrap(),
            "another process was mid-write",
            "a fixed temp name is a shared path between processes; it must not be used"
        );
        assert!(marker_present(&ws));
    }

    #[test]
    fn context_file_and_binary() {
        assert_eq!(CodexAgent.context_file(), "AGENTS.md");
        std::env::set_var("WS_CODEX_BIN", "/fake/codex");
        let b = CodexAgent.binary();
        std::env::remove_var("WS_CODEX_BIN");
        assert_eq!(b, "/fake/codex");
    }

    #[test]
    fn headless_uses_exec_and_never_bypasses_the_sandbox() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let ctx = LaunchCtx { fresh: true, sessions_root: td.path().to_path_buf() };
        let out_file = td.path().join("out.txt");
        let a = args(&CodexAgent.headless(&ws, "do the thing", &ctx, &out_file).unwrap());
        assert_eq!(a.first().map(String::as_str), Some("exec"));
        assert!(a.contains(&"do the thing".to_string()), "{a:?}");
        // C1: the success signal is the --output-last-message file, not stdout.
        assert!(a.contains(&"--json".to_string()), "{a:?}");
        assert!(a.windows(2).any(|w| w[0] == "-o" && w[1] == out_file.to_string_lossy()),
                "-o must point at out_file: {a:?}");
        for forbidden in ["--dangerously-bypass-approvals-and-sandbox",
                          "--dangerously-bypass-hook-trust", "-s", "--sandbox"] {
            assert!(!a.iter().any(|x| x == forbidden), "{forbidden} must never be passed: {a:?}");
        }
    }

    #[test]
    fn headless_resumes_when_a_marker_exists_and_not_when_fresh() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        record_marker(&ws).unwrap();
        let out_file = td.path().join("out.txt");

        let resumed = args(&CodexAgent
            .headless(&ws, "next", &LaunchCtx { fresh: false, sessions_root: td.path().into() }, &out_file)
            .unwrap());
        assert_eq!(resumed.get(1).map(String::as_str), Some("resume"), "{resumed:?}");

        let first = args(&CodexAgent
            .headless(&ws, "next", &LaunchCtx { fresh: true, sessions_root: td.path().into() }, &out_file)
            .unwrap());
        assert_ne!(first.get(1).map(String::as_str), Some("resume"), "{first:?}");
    }

    /// I1 (mild, codex side). `headless` on the fresh path must record the
    /// marker itself, so task 2 of a drain resumes task 1's headless session
    /// instead of running fresh because no *interactive* launch happened to
    /// record one first.
    #[test]
    fn headless_records_the_marker_on_the_fresh_path_so_task_two_resumes() {
        let td = TempDir::new().unwrap();
        let ws = ws_at(td.path());
        let out_file = td.path().join("out.txt");
        assert!(!CodexAgent.has_prior_session(&ws), "no prior session before task 1");

        CodexAgent
            .headless(&ws, "one", &LaunchCtx { fresh: true, sessions_root: td.path().into() }, &out_file)
            .unwrap();
        assert!(CodexAgent.has_prior_session(&ws), "task 1's headless run must record the marker itself");

        let second = args(&CodexAgent
            .headless(&ws, "two", &LaunchCtx { fresh: false, sessions_root: td.path().into() }, &out_file)
            .unwrap());
        assert_eq!(second.get(1).map(String::as_str), Some("resume"), "{second:?}");
    }

    /// C1 (critical). This is the discriminator: exit 0 with a full,
    /// realistic stdout banner (exactly what `codex exec` prints on a
    /// refusal) must still be a failure when `out_file` is absent or empty.
    /// If `headless_succeeded` regresses to `status.success() &&
    /// !out.stdout.is_empty()`, this test fails.
    #[test]
    fn headless_succeeded_reads_the_output_file_not_stdout() {
        use std::os::unix::process::ExitStatusExt;
        let td = TempDir::new().unwrap();
        let out_file = td.path().join("out.txt");
        let ok = |code: i32, stdout: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        };

        // Exit 0, chatty non-empty stdout, no output file at all: the exact
        // shape of a refusal. The old `!out.stdout.is_empty()` heuristic
        // called this a success; it must be a failure.
        assert!(
            !CodexAgent.headless_succeeded(
                &ok(0, "codex banner\nreasoning...\nI won't do that."),
                &out_file
            ),
            "a clean exit with chatty stdout but no output file must be a failure"
        );

        // File exists but is empty.
        std::fs::write(&out_file, "").unwrap();
        assert!(!CodexAgent.headless_succeeded(&ok(0, "banner"), &out_file), "empty output file is a failure");

        // File has real content: success, regardless of what stdout says.
        std::fs::write(&out_file, "final assistant message").unwrap();
        assert!(
            CodexAgent.headless_succeeded(&ok(0, ""), &out_file),
            "a non-empty output file with a clean exit is success"
        );

        // Non-zero exit trumps a good file.
        assert!(
            !CodexAgent.headless_succeeded(&ok(1, ""), &out_file),
            "non-zero exit is a failure regardless of the output file"
        );
    }
}
