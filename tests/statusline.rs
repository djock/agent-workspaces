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
    env.cmd().current_dir(&proj).args(["-adopt", "sl"]).assert().success();

    let out = env
        .cmd()
        .env("WS_WORKSPACE", "sl")
        .env("WS_DIR", &proj)
        .env("NO_COLOR", "1")
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

/// End to end: `-adopt` allocates a color, `-color` overrides it, and the chip
/// the status line prints is that color. This is the whole feature in one test —
/// the workspace's identity is drawn by ws's own status line, so nothing needs to
/// inject Claude's `/color` and hang a pill off the prompt divider.
#[test]
fn the_status_line_leads_with_a_colored_workspace_chip() {
    let env = Env::new();
    let proj = env.home.path().join("sl");
    std::fs::create_dir_all(&proj).unwrap();
    env.cmd().current_dir(&proj).args(["-adopt", "sl"]).assert().success();

    let toml = std::fs::read_to_string(proj.join(".ws/workspace.toml")).unwrap();
    assert!(toml.contains("color = "), "a new workspace is allocated a color: {toml}");

    env.cmd().current_dir(&proj).args(["-color", "green"]).assert().success();

    let render = |no_color: bool| {
        let mut c = env.cmd();
        c.env("WS_WORKSPACE", "sl").env("WS_DIR", &proj);
        if no_color {
            c.env("NO_COLOR", "1");
        }
        let out =
            c.arg("statusline").write_stdin(SAMPLE).assert().success().get_output().stdout.clone();
        String::from_utf8(out).unwrap()
    };

    // green is 22,163,74 — the same RGB the iTerm2 tab background is set to. The
    // bar leads with a reset so no residual SGR state bleeds into the first block.
    let colored = render(false);
    assert!(colored.starts_with("\x1b[0m\x1b[48;2;22;163;74m"), "chip leads: {colored:?}");
    assert!(colored.contains(" sl "), "{colored:?}");

    let plain = render(true);
    assert!(plain.starts_with("sl \u{b7} "), "NO_COLOR keeps the name: {plain:?}");
    assert!(!plain.contains('\x1b'), "NO_COLOR drops every escape: {plain:?}");
}

/// Outside a ws launch there is no workspace to name, and the line must be
/// exactly what it always was.
#[test]
fn the_chip_is_absent_outside_a_workspace() {
    let env = Env::new();
    let out = env
        .cmd()
        .arg("statusline")
        .write_stdin(SAMPLE)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let statusline = String::from_utf8(out).unwrap();
    // First block is the model, not a workspace name.
    assert!(statusline.starts_with("\x1b[0m\x1b[48;2;138;134;236m"), "{statusline:?}");
    assert!(statusline.contains(" Opus 4.8 high "), "{statusline:?}");
    assert!(!statusline.contains("sl"), "no workspace name to show: {statusline:?}");
}

#[test]
fn statusline_survives_garbage_stdin() {
    let env = Env::new();
    env.cmd().env("NO_COLOR", "1").arg("statusline").write_stdin("not json").assert().success();
    // never errors
}
