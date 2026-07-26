mod common;
use common::Env;

#[test]
fn list_reports_a_corrupt_workspace_instead_of_hiding_it() {
    let e = Env::new();
    let ws = e.root.join("broken");
    std::fs::create_dir_all(ws.join(".ws")).unwrap();
    std::fs::write(ws.join(".ws/workspace.toml"), "not toml {{{").unwrap();
    e.cmd().args(["-adopt", "broken"]).current_dir(&ws).assert().success();

    let out = e.cmd().arg("-list").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("broken"), "the workspace is still listed: {text}");
    assert!(text.contains("corrupt"), "and its state is reported: {text}");
}

#[test]
fn list_fails_loudly_when_the_registry_itself_is_unreadable() {
    let e = Env::new();
    let reg = e.home.path().join(".config/ws/registry.toml");
    std::fs::create_dir_all(reg.parent().unwrap()).unwrap();
    std::fs::write(&reg, "not toml {{{").unwrap();

    let out = e.cmd().arg("-list").output().unwrap();
    assert!(!out.status.success(), "a corrupt registry must not exit 0 with an empty list");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("registry.toml"), "stderr names the file: {err}");
}

#[test]
fn bare_ws_falls_back_to_the_text_list_when_not_a_tty() {
    let e = Env::new();
    // assert_cmd pipes stdout, so this exercises the non-TTY branch. A TUI
    // would emit terminal escape sequences or hang waiting for a keypress.
    let out = e.cmd().output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    // An empty registry says so plainly: "no active workspaces (try:
    // -list --archived)" is for the case where workspaces exist but all of
    // them are archived. See commands::list.
    assert!(text.contains("no workspaces yet"), "{text}");
}

#[test]
fn explicit_tui_without_a_terminal_is_a_clean_error_not_a_panic() {
    // The plan (Task 2, Step 9) said not to test this because `ratatui::init()`
    // on a non-terminal "either errors or blocks". It did neither: it panicked
    // (exit 101) and left an escape sequence on stdout. assert_cmd pipes all
    // three streams, so this is exactly the `ws -tui < /dev/null > f` case.
    let e = Env::new();
    let out = e.cmd().arg("-tui").output().unwrap();

    assert!(!out.status.success(), "-tui without a terminal must fail");
    assert_ne!(
        out.status.code(),
        Some(101),
        "101 is a Rust panic; -tui must report a normal error instead"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("requires a terminal"), "stderr explains why: {err}");
    assert!(
        !err.contains("panicked"),
        "a documented flag must not answer with a backtrace: {err}"
    );
    assert!(
        out.stdout.is_empty(),
        "and nothing is written to stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
