mod common;
use common::Env;

fn adopt(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    env.cmd().current_dir(&p).args(["-adopt", name]).assert().success();
    p
}

/// Run the redaction hook over a `Write` payload naming `file`.
fn write_hook(
    env: &Env,
    name: &str,
    root: &std::path::Path,
    file: &std::path::Path,
) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_WORKSPACE", name)
        .env("WS_DIR", root)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#,
            file.display()
        ));
    c
}

fn session_log(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join(".ws/local/log/session.log")).unwrap_or_default()
}

#[test]
fn redact_rewrites_secret_and_stores_it() {
    let env = Env::new();
    let proj = adopt(&env, "rp");
    // an agent just wrote a .env with a secret
    let envfile = proj.join(".env");
    std::fs::write(&envfile, "PORT=8080\nAPI_KEY=supersecret123\n").unwrap();

    let payload =
        format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, envfile.display());
    env.cmd()
        .env("WS_WORKSPACE", "rp")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    // file rewritten: the literal is gone, a placeholder took its place; PORT untouched
    let after = std::fs::read_to_string(&envfile).unwrap();
    assert!(!after.contains("supersecret123"), "secret literal must be gone");
    assert!(
        after.contains("API_KEY={{ws:secret:API_KEY}}"),
        "placeholder must be present: {after}"
    );
    assert!(after.contains("PORT=8080"), "non-secret untouched");

    // the value is retrievable from the store
    env.cmd()
        .env("WS_WORKSPACE", "rp")
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "get", "API_KEY"])
        .assert()
        .success()
        .stdout(predicates::str::diff("supersecret123\n"));
}

/// The Codex half of redaction, end to end through the real binary.
///
/// Codex never emits `Write`/`Edit` and never populates `tool_input.file_path`:
/// a file edit arrives as `tool_name: "apply_patch"` with the target named
/// inside a patch envelope in `tool_input.command`. This payload is the shape
/// captured from Codex CLI 0.145.0. Until both the matcher and the handler knew
/// about it, secret redaction was silently a no-op for every Codex user — the
/// one hook whose whole job is keeping credentials out of files.
#[test]
fn redact_handles_a_codex_apply_patch_payload() {
    let env = Env::new();
    let proj = adopt(&env, "cx");
    let envfile = proj.join(".env");
    std::fs::write(&envfile, "PORT=8080\nAPI_KEY=codexsecret456\n").unwrap();

    // Note: no `file_path` key at all, and the tool name is apply_patch.
    let payload = serde_json::json!({
        "tool_name": "apply_patch",
        "cwd": proj.to_string_lossy(),
        "tool_input": {
            "command": format!(
                "*** Begin Patch\n*** Add File: {}\n+API_KEY=codexsecret456\n*** End Patch",
                envfile.display()
            )
        }
    })
    .to_string();

    env.cmd()
        .env("WS_WORKSPACE", "cx")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    let after = std::fs::read_to_string(&envfile).unwrap();
    assert!(!after.contains("codexsecret456"), "secret literal must be gone: {after}");
    assert!(
        after.contains("API_KEY={{ws:secret:API_KEY}}"),
        "placeholder must be present: {after}"
    );
    assert!(after.contains("PORT=8080"), "non-secret untouched: {after}");

    env.cmd()
        .env("WS_WORKSPACE", "cx")
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "get", "API_KEY"])
        .assert()
        .success()
        .stdout(predicates::str::diff("codexsecret456\n"));
}

/// A single `apply_patch` can write several files. Redacting only the first
/// would leave credentials in the rest while reporting success.
#[test]
fn redact_handles_every_file_in_a_multi_file_codex_patch() {
    let env = Env::new();
    let proj = adopt(&env, "cxm");
    let one = proj.join("one.env");
    let two = proj.join("two.env");
    // Values are deliberately >= 12 chars: the value signal added in task 4 is
    // what keeps `TOKEN_BUDGET=4096` untouched, and a short fixture would be
    // testing the classifier rather than the multi-file walk.
    std::fs::write(&one, "A_TOKEN=first1112223334\n").unwrap();
    std::fs::write(&two, "B_SECRET=second2223334445\n").unwrap();

    let payload = serde_json::json!({
        "tool_name": "apply_patch",
        "cwd": proj.to_string_lossy(),
        "tool_input": {
            // one absolute, one relative — both must resolve.
            "command": format!(
                "*** Begin Patch\n*** Add File: {}\n+A_TOKEN=first1112223334\n*** Update File: two.env\n+B_SECRET=second2223334445\n*** End Patch",
                one.display()
            )
        }
    })
    .to_string();

    env.cmd()
        .env("WS_WORKSPACE", "cxm")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    let a = std::fs::read_to_string(&one).unwrap();
    let b = std::fs::read_to_string(&two).unwrap();
    assert!(!a.contains("first1112223334"), "first file not redacted: {a}");
    assert!(!b.contains("second2223334445"), "second file not redacted (relative path): {b}");
}

#[test]
fn redact_ignores_non_secret_files() {
    let env = Env::new();
    let proj = adopt(&env, "rp2");
    let f = proj.join("notes.txt");
    std::fs::write(&f, "just some text PORT=1\n").unwrap();
    let payload =
        format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, f.display());
    env.cmd()
        .env("WS_WORKSPACE", "rp2")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();
    // unchanged (no secret-looking assignment)
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "just some text PORT=1\n");
}

#[test]
fn redact_does_not_corrupt_non_secret_lookalikes() {
    let env = Env::new();
    let proj = adopt(&env, "rp3");
    let f = proj.join("app.toml");
    std::fs::write(&f, "api_url = \"https://example.com\"\nMONKEY = \"banana\"\n").unwrap();
    let payload =
        format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, f.display());
    env.cmd()
        .env("WS_WORKSPACE", "rp3")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();
    // neither line is a secret name → file byte-for-byte unchanged
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "api_url = \"https://example.com\"\nMONKEY = \"banana\"\n"
    );
}

// ---------- task 4: two-signal classification ----------

/// The four false positives the name-only classifier produced. Every one of
/// these is ordinary configuration whose *name* looks credential-shaped, and
/// redacting them replaced a working setting with a placeholder nothing
/// resolved — the feature actively broke the file it was protecting.
///
/// Discriminating: under name-only matching all four lines are rewritten, so
/// this test fails on the whole line-count assertion, not on a detail.
#[test]
fn redact_leaves_configuration_lookalikes_untouched() {
    let env = Env::new();
    let proj = adopt(&env, "fp");
    let f = proj.join("config.env");
    let original =
        "PASSWORD_MIN_LENGTH=8\nTOKENIZER=gpt2\nTOKEN_BUDGET=4096\nSECRET_SCAN_ENABLED=true\n";
    std::fs::write(&f, original).unwrap();

    write_hook(&env, "fp", &proj, &f).assert().success();

    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        original,
        "a credential-shaped NAME with a plainly non-credential VALUE must be left alone"
    );
}

/// The false *negative* the same change has to keep closing: `AWS_ACCESS_KEY_ID`
/// was missed entirely because the old name list had no `ACCESS_KEY` entry, and
/// `AKIA...` is about as unambiguous as a credential gets.
#[test]
fn redact_catches_an_aws_access_key_id() {
    let env = Env::new();
    let proj = adopt(&env, "aws");
    let f = proj.join(".env");
    std::fs::write(&f, "REGION=eu-west-1\nAWS_ACCESS_KEY_ID=AKIA0123456789EXAMPLE\n").unwrap();

    write_hook(&env, "aws", &proj, &f).assert().success();

    let after = std::fs::read_to_string(&f).unwrap();
    assert!(!after.contains("AKIA0123456789EXAMPLE"), "the key literal must be gone: {after}");
    assert!(
        after.contains("AWS_ACCESS_KEY_ID={{ws:secret:AWS_ACCESS_KEY_ID}}"),
        "placeholder must be present: {after}"
    );
    assert!(after.contains("REGION=eu-west-1"), "non-secret untouched: {after}");

    env.cmd()
        .env("WS_WORKSPACE", "aws")
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "get", "AWS_ACCESS_KEY_ID"])
        .assert()
        .success()
        .stdout(predicates::str::diff("AKIA0123456789EXAMPLE\n"));
}

/// `PAT` is three letters that live inside `PATH`, `PATTERN`, `XPATH` and
/// `PATCH`. As a substring it would redact half a build config; as a whole
/// `_`-separated segment it catches exactly the thing it is for.
#[test]
fn redact_matches_pat_only_as_a_whole_name_segment() {
    let env = Env::new();
    let proj = adopt(&env, "pat");
    let f = proj.join(".env");
    let body = "GITHUB_PAT=github_pat_11ABCDEFG0123456789\n\
                XPATH_QUERY=/html/body/div/span/text\n\
                PATTERN_FILE=a-long-pattern-file-name\n";
    std::fs::write(&f, body).unwrap();

    write_hook(&env, "pat", &proj, &f).assert().success();

    let after = std::fs::read_to_string(&f).unwrap();
    assert!(
        after.contains("GITHUB_PAT={{ws:secret:GITHUB_PAT}}"),
        "GITHUB_PAT is a personal access token: {after}"
    );
    assert!(
        after.contains("XPATH_QUERY=/html/body/div/span/text"),
        "XPATH_QUERY is not a token, and its value is long enough to trip the value signal alone: {after}"
    );
    assert!(
        after.contains("PATTERN_FILE=a-long-pattern-file-name"),
        "PATTERN_FILE is not a token: {after}"
    );
}

// ---------- task 4: containment and visible failure ----------

/// The hook redacted whatever path the payload named. A payload naming a file
/// outside the workspace — another workspace's `.env`, or `~/.aws/credentials`
/// — got its values pulled into *this* workspace's store and replaced with a
/// placeholder only this workspace can resolve.
#[test]
fn redact_skips_a_file_outside_the_workspace_root() {
    let env = Env::new();
    let proj = adopt(&env, "inside");
    let outsider = env.home.path().join("elsewhere.env");
    let original = "API_KEY=outsidersecret123\n";
    std::fs::write(&outsider, original).unwrap();

    write_hook(&env, "inside", &proj, &outsider)
        .assert()
        .success()
        // A path the agent wrote outside the workspace is not the user's
        // problem to read about on every tool call: log it, don't shout.
        .stderr(predicates::str::is_empty());

    assert_eq!(
        std::fs::read_to_string(&outsider).unwrap(),
        original,
        "a file outside the workspace root must not be touched"
    );
    let log = session_log(&proj);
    assert!(
        log.contains("elsewhere.env") && log.to_lowercase().contains("outside"),
        "the skip must be recorded in the session log: {log}"
    );
}

/// The old fail-open: no `$WS_SECRETS_PASSWORD` means the file backend cannot
/// open without prompting, and a hook has nobody to prompt. That returned
/// silently — the credential stayed in the file and *nothing anywhere said
/// so*, which is strictly worse than not having the feature.
///
/// Fail-visible now: the file is still left alone (there is nowhere safe to
/// put the value), but both stderr and the session log say why.
#[test]
fn redact_reports_an_unavailable_secret_store_instead_of_skipping_silently() {
    let env = Env::new();
    let proj = adopt(&env, "nostore");
    let f = proj.join(".env");
    let original = "API_KEY=supersecret123\n";
    std::fs::write(&f, original).unwrap();

    // Note: WS_SECRETS_PASSWORD is deliberately absent, and stdin is a pipe —
    // so the file backend has no password and no TTY to ask on.
    env.cmd()
        .env("WS_WORKSPACE", "nostore")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env_remove("WS_SECRETS_PASSWORD")
        .args(["internal", "secret-redact"])
        .write_stdin(format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#,
            f.display()
        ))
        .assert()
        // A PostToolUse hook must never fail the agent's tool call.
        .success()
        .stderr(predicates::str::contains("redaction skipped"));

    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        original,
        "with no store to put the value in, the file must be left exactly as written"
    );
    let log = session_log(&proj);
    assert!(
        log.contains("redaction skipped: secret store unavailable"),
        "the session log must carry the warning: {log}"
    );
}

/// Silence is only acceptable when there was nothing to do: a file with no
/// credential-shaped assignment must not log a warning even when the store is
/// unopenable, or the log fills with noise from every ordinary file write.
#[test]
fn an_unavailable_store_is_not_reported_when_there_was_nothing_to_redact() {
    let env = Env::new();
    let proj = adopt(&env, "quiet");
    let f = proj.join("notes.txt");
    std::fs::write(&f, "PORT=8080\n").unwrap();

    env.cmd()
        .env("WS_WORKSPACE", "quiet")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env_remove("WS_SECRETS_PASSWORD")
        .args(["internal", "secret-redact"])
        .write_stdin(format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#,
            f.display()
        ))
        .assert()
        .success()
        .stderr(predicates::str::is_empty());

    assert!(
        !session_log(&proj).contains("redaction skipped"),
        "no candidate line means no store touch and nothing to warn about"
    );
}

// ---------- task 4: ws -secrets restore ----------

/// The placeholder was write-only: `{{ws:secret:NAME}}` went into the file and
/// no code path anywhere put the value back, so a redacted `.env` was simply a
/// broken `.env`. Round-trip is the whole contract.
#[test]
fn secrets_restore_round_trips_a_redacted_file() {
    let env = Env::new();
    let proj = adopt(&env, "rt");
    let f = proj.join(".env");
    let original = "PORT=8080\nAPI_KEY=supersecret123\n";
    std::fs::write(&f, original).unwrap();

    write_hook(&env, "rt", &proj, &f).assert().success();
    let redacted = std::fs::read_to_string(&f).unwrap();
    assert!(redacted.contains("{{ws:secret:API_KEY}}"), "sanity: redaction ran: {redacted}");

    env.cmd()
        .current_dir(&proj)
        .env("WS_WORKSPACE", "rt")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "restore", ".env"])
        .assert()
        .success()
        .stdout(predicates::str::contains("1"));

    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        original,
        "restore must reproduce the file the agent originally wrote"
    );
}

/// A name the store does not have must not be papered over: guessing an empty
/// string, or dropping the placeholder, silently corrupts the file. Leave it
/// in place, name it, and exit non-zero so a script notices.
#[test]
fn secrets_restore_leaves_an_unknown_placeholder_and_fails() {
    let env = Env::new();
    let proj = adopt(&env, "unk");
    let f = proj.join(".env");
    let body = "A_TOKEN={{ws:secret:A_TOKEN}}\nB_TOKEN={{ws:secret:B_TOKEN}}\n";
    std::fs::write(&f, body).unwrap();

    // only A_TOKEN is in the store
    env.cmd()
        .env("WS_WORKSPACE", "unk")
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "set", "A_TOKEN"])
        .write_stdin("aaaa1111bbbb")
        .assert()
        .success();

    env.cmd()
        .current_dir(&proj)
        .env("WS_WORKSPACE", "unk")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "restore", ".env"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("B_TOKEN"));

    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("A_TOKEN=aaaa1111bbbb"), "the known one is resolved: {after}");
    assert!(
        after.contains("B_TOKEN={{ws:secret:B_TOKEN}}"),
        "the unknown one keeps its placeholder rather than becoming empty: {after}"
    );
}

/// A `.env` is commonly 0600, and both halves of this feature rewrite it.
/// Recreating it under the process umask instead would leave the *restored
/// plaintext credential* world-readable — the file whose permissions matter
/// most, loosened by the tool whose job is protecting it.
#[test]
#[cfg(unix)]
fn redact_and_restore_both_preserve_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let proj = adopt(&env, "perm");
    let f = proj.join(".env");
    std::fs::write(&f, "API_KEY=supersecret123\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();

    write_hook(&env, "perm", &proj, &f).assert().success();
    let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the redaction rewrite must not loosen the file");

    env.cmd()
        .current_dir(&proj)
        .env("WS_WORKSPACE", "perm")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "restore", ".env"])
        .assert()
        .success();
    let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "and neither must the restore rewrite");
    assert!(std::fs::read_to_string(&f).unwrap().contains("supersecret123"));
}

/// Same containment rule as the hook, on the other side: `restore` writes
/// plaintext credentials, so it must refuse a path outside the workspace root
/// no matter how it is spelled.
#[test]
fn secrets_restore_refuses_a_path_outside_the_workspace() {
    let env = Env::new();
    let proj = adopt(&env, "guard");
    let outsider = env.home.path().join("outside.env");
    let body = "API_KEY={{ws:secret:API_KEY}}\n";
    std::fs::write(&outsider, body).unwrap();

    env.cmd()
        .current_dir(&proj)
        .env("WS_WORKSPACE", "guard")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "restore", "../outside.env"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("outside"));

    assert_eq!(std::fs::read_to_string(&outsider).unwrap(), body, "untouched");
}

/// Claude's `NotebookEdit` names its target `notebook_path`, not `file_path`.
/// The matcher was widened to include the tool, but the hook input struct only
/// read `file_path` — so redaction matched the call and then found no path to
/// scan, making notebook coverage an advertised no-op.
#[test]
fn redact_handles_a_notebook_edit_payload() {
    let env = Env::new();
    let proj = adopt(&env, "nb");
    let target = proj.join("secrets.env");
    std::fs::write(&target, "PORT=8080\nAPI_KEY=notebooksecret789\n").unwrap();

    let payload = serde_json::json!({
        "tool_name": "NotebookEdit",
        "tool_input": { "notebook_path": target.to_string_lossy() },
    })
    .to_string();

    env.cmd()
        .env("WS_WORKSPACE", "nb")
        .env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    let after = std::fs::read_to_string(&target).unwrap();
    assert!(!after.contains("notebooksecret789"), "the secret literal must be gone: {after}");
    assert!(
        after.contains("API_KEY={{ws:secret:API_KEY}}"),
        "a placeholder must have replaced it: {after}"
    );
}

/// One workspace, one secret namespace: if two files assign the same NAME
/// different values, storing the second over the first would make
/// `ws -secrets restore` write the second file's credential into the first, and
/// the first value would be gone for good. The later file is left as plaintext
/// and the collision reported instead.
#[test]
fn redact_refuses_to_overwrite_a_stored_secret_with_a_different_value() {
    let env = Env::new();
    let proj = adopt(&env, "coll");

    let first = proj.join(".env");
    std::fs::write(&first, "API_KEY=firstvalue12345\n").unwrap();
    write_hook(&env, "coll", &proj, &first).assert().success();
    let after_first = std::fs::read_to_string(&first).unwrap();
    assert!(
        after_first.contains("API_KEY={{ws:secret:API_KEY}}"),
        "the first file is redacted normally: {after_first}"
    );

    let second = proj.join(".env.production");
    std::fs::write(&second, "API_KEY=secondvalue67890\n").unwrap();
    write_hook(&env, "coll", &proj, &second).assert().success();

    let after_second = std::fs::read_to_string(&second).unwrap();
    assert!(
        after_second.contains("API_KEY=secondvalue67890"),
        "the colliding line is left alone, not replaced: {after_second}"
    );
    assert!(
        session_log(&proj).contains("already stored with a different value"),
        "the collision is reported in the session log: {}",
        session_log(&proj)
    );

    // The store still holds the FIRST value, so restoring the first file works.
    env.cmd()
        .env("WS_WORKSPACE", "coll")
        .env("WS_SECRETS_BACKEND", "file")
        .env("WS_SECRETS_PASSWORD", "pw")
        .args(["-secrets", "get", "API_KEY"])
        .assert()
        .success()
        .stdout(predicates::str::diff("firstvalue12345\n"));
}
