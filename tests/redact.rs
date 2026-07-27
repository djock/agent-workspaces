mod common;
use common::Env;

fn adopt(env: &Env, name: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::create_dir_all(&p).unwrap();
    env.cmd().current_dir(&p).args(["-adopt", name]).assert().success();
    p
}

#[test]
fn redact_rewrites_secret_and_stores_it() {
    let env = Env::new();
    let proj = adopt(&env, "rp");
    // an agent just wrote a .env with a secret
    let envfile = proj.join(".env");
    std::fs::write(&envfile, "PORT=8080\nAPI_KEY=supersecret123\n").unwrap();

    let payload = format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, envfile.display());
    env.cmd()
        .env("WS_WORKSPACE", "rp").env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file").env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    // file rewritten: the literal is gone, a placeholder took its place; PORT untouched
    let after = std::fs::read_to_string(&envfile).unwrap();
    assert!(!after.contains("supersecret123"), "secret literal must be gone");
    assert!(after.contains("API_KEY={{ws:secret:API_KEY}}"), "placeholder must be present: {after}");
    assert!(after.contains("PORT=8080"), "non-secret untouched");

    // the value is retrievable from the store
    env.cmd().env("WS_WORKSPACE","rp").env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","pw")
        .args(["-secrets","get","API_KEY"]).assert().success()
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
        .env("WS_WORKSPACE", "cx").env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file").env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    let after = std::fs::read_to_string(&envfile).unwrap();
    assert!(!after.contains("codexsecret456"), "secret literal must be gone: {after}");
    assert!(after.contains("API_KEY={{ws:secret:API_KEY}}"), "placeholder must be present: {after}");
    assert!(after.contains("PORT=8080"), "non-secret untouched: {after}");

    env.cmd().env("WS_WORKSPACE","cx").env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","pw")
        .args(["-secrets","get","API_KEY"]).assert().success()
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
    std::fs::write(&one, "A_TOKEN=first111\n").unwrap();
    std::fs::write(&two, "B_SECRET=second222\n").unwrap();

    let payload = serde_json::json!({
        "tool_name": "apply_patch",
        "cwd": proj.to_string_lossy(),
        "tool_input": {
            // one absolute, one relative — both must resolve.
            "command": format!(
                "*** Begin Patch\n*** Add File: {}\n+A_TOKEN=first111\n*** Update File: two.env\n+B_SECRET=second222\n*** End Patch",
                one.display()
            )
        }
    })
    .to_string();

    env.cmd()
        .env("WS_WORKSPACE", "cxm").env("WS_DIR", &proj)
        .env("WS_SECRETS_BACKEND", "file").env("WS_SECRETS_PASSWORD", "pw")
        .args(["internal", "secret-redact"])
        .write_stdin(payload)
        .assert()
        .success();

    let a = std::fs::read_to_string(&one).unwrap();
    let b = std::fs::read_to_string(&two).unwrap();
    assert!(!a.contains("first111"), "first file not redacted: {a}");
    assert!(!b.contains("second222"), "second file not redacted (relative path): {b}");
}

#[test]
fn redact_ignores_non_secret_files() {
    let env = Env::new();
    let proj = adopt(&env, "rp2");
    let f = proj.join("notes.txt");
    std::fs::write(&f, "just some text PORT=1\n").unwrap();
    let payload = format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, f.display());
    env.cmd().env("WS_WORKSPACE","rp2").env("WS_DIR",&proj)
        .env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","pw")
        .args(["internal","secret-redact"]).write_stdin(payload).assert().success();
    // unchanged (no secret-looking assignment)
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "just some text PORT=1\n");
}

#[test]
fn redact_does_not_corrupt_non_secret_lookalikes() {
    let env = Env::new();
    let proj = adopt(&env, "rp3");
    let f = proj.join("app.toml");
    std::fs::write(&f, "api_url = \"https://example.com\"\nMONKEY = \"banana\"\n").unwrap();
    let payload = format!(r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#, f.display());
    env.cmd().env("WS_WORKSPACE","rp3").env("WS_DIR",&proj)
        .env("WS_SECRETS_BACKEND","file").env("WS_SECRETS_PASSWORD","pw")
        .args(["internal","secret-redact"]).write_stdin(payload).assert().success();
    // neither line is a secret name → file byte-for-byte unchanged
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "api_url = \"https://example.com\"\nMONKEY = \"banana\"\n");
}
