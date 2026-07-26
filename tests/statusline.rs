mod common;
use common::Env;

const SAMPLE: &str = r#"{
  "session_name":"demo",
  "model":{"display_name":"Opus 4.8"},
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

    env.cmd()
        .env("WS_WORKSPACE","sl").env("WS_DIR",&proj).env("NO_COLOR","1")
        .arg("statusline")
        .write_stdin(SAMPLE)
        .assert()
        .success()
        .stdout(predicates::str::contains("sl"))          // workspace name
        .stdout(predicates::str::contains("ctx 12%"))
        .stdout(predicates::str::contains("5h 73%"))
        .stdout(predicates::str::contains("$1.23"));

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

#[test]
fn subagent_statusline_emits_row_per_task() {
    let env = Env::new();
    let now_ms = 1_000_000_000i64;
    let start_ms = now_ms - 10_000; // 10s ago
    let payload = format!(
        r#"{{"columns":120,"tasks":[
          {{"id":"t1","model":"Sonnet 5","name":"local","description":"Implement Task 1","tokenCount":3000,"contextWindowSize":100000,"start":{start_ms}}}
        ]}}"#
    );
    env.cmd()
        .env("NO_COLOR","1")
        .env("WS_SUBAGENT_NOW_MS", now_ms.to_string())
        .arg("subagent-statusline")
        .write_stdin(payload)
        .assert()
        .success()
        .stdout(predicates::str::contains("\"id\":\"t1\""))
        .stdout(predicates::str::contains("Sonnet 5"))
        .stdout(predicates::str::contains("Implement Task 1"))
        .stdout(predicates::str::contains("ctx 3%"))
        .stdout(predicates::str::contains("0m10s"));
}

#[test]
fn subagent_statusline_one_line_per_task() {
    let env = Env::new();
    let payload = r#"{"columns":120,"tasks":[
      {"id":"a","model":"Sonnet 5","name":"one","description":"d1","tokenCount":1000,"contextWindowSize":100000,"start":0},
      {"id":"b","model":"Opus 4.8","name":"two","description":"d2","tokenCount":2000,"contextWindowSize":100000,"start":0}
    ]}"#;
    let out = env.cmd()
        .env("NO_COLOR","1")
        .arg("subagent-statusline")
        .write_stdin(payload)
        .assert().success()
        .get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected one line per task, got: {text}");
    assert!(text.contains("\"id\":\"a\""));
    assert!(text.contains("\"id\":\"b\""));
}

#[test]
fn subagent_statusline_survives_garbage_stdin() {
    let env = Env::new();
    env.cmd()
        .arg("subagent-statusline")
        .write_stdin("not json at all")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}
