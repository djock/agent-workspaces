mod common;
use common::Env;

const SAMPLE: &str = r#"{
  "session_name":"demo",
  "model":{"display_name":"Opus 4.8"},
  "effort":{"level":"high"},
  "context_window":{"used_percentage":12.4},
  "rate_limits":{
    "five_hour":{"used_percentage":73.0,"resets_at":9999999999},
    "seven_day":{"used_percentage":10.0,"resets_at":9999999999}
  },
  "cost":{"total_cost_usd":1.23},
  "workspace":{"current_dir":"/tmp/x"}
}"#;

#[test]
fn statusline_renders_and_captures() {
    let env = Env::new();
    // create a ws workspace so capture has a home
    let proj = env.home.path().join("sl");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt","sl"]).assert().success();

    let out = env.cmd()
        .env("WS_WORKSPACE","sl").env("WS_DIR",&proj).env("NO_COLOR","1")
        .arg("statusline")
        .write_stdin(SAMPLE)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let statusline = String::from_utf8(out).unwrap();
    assert_eq!(statusline.matches("Opus 4.8").count(), 1, "model must appear once: {statusline}");
    assert!(statusline.contains("Opus 4.8 (high)"));
    assert!(statusline.contains("ctx 12%"));
    assert!(statusline.contains("5h 73%"));
    assert!(statusline.contains("wk 10%"));
    assert!(!statusline.contains("/tmp/x"), "folder path must be omitted: {statusline}");
    assert!(!statusline.contains("$1.23"), "Claude-only cost must be omitted: {statusline}");

    // limits.json captured
    let lj = proj.join(".ws/local/limits.json");
    assert!(lj.is_file());
    let body = std::fs::read_to_string(lj).unwrap();
    assert!(body.contains("\"used_pct\": 73"));
}

#[test]
fn statusline_survives_garbage_stdin() {
    let env = Env::new();
    env.cmd()
        .env("NO_COLOR","1")
        .arg("statusline")
        .write_stdin("not json")
        .assert()
        .success(); // never errors
}



