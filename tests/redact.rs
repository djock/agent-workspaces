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
