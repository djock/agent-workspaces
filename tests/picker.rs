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

/// `-pick` without a terminal prints the list instead of failing.
///
/// The dashboard this replaced *panicked* here (exit 101, plus an escape sequence
/// on stdout) — `ratatui::init()` on a non-terminal did neither of the two things
/// its docs promised. The picker has no interactive work it must do, so the honest
/// answer to "no terminal" is the same information without the arrow keys, not an
/// error. assert_cmd pipes all three streams, so this is exactly the
/// `ws -pick < /dev/null > f` case.
#[test]
fn explicit_pick_without_a_terminal_lists_instead_of_failing() {
    let e = Env::new();
    let out = e.cmd().arg("-pick").output().unwrap();

    assert!(out.status.success(), "-pick must degrade, not fail: {out:?}");
    assert_ne!(out.status.code(), Some(101), "101 is a Rust panic");
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("no workspaces yet"), "it printed the list: {text}");
    assert!(!err.contains("panicked"), "no backtrace: {err}");
    assert!(
        !text.contains('\x1b'),
        "no escape sequences when there is no terminal to interpret them: {text:?}"
    );
}

/// The picker must never leave a terminal in raw mode or on an alternate screen.
/// The surest proof from outside is that a non-interactive run emits no terminal
/// control at all — not the alternate-screen switch, not a clear, not a highlight.
#[test]
fn pick_emits_no_terminal_control_sequences_without_a_tty() {
    let e = Env::new();
    let ws = e.root.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    e.cmd().args(["-adopt", "proj"]).current_dir(&ws).assert().success();

    let out = e.cmd().arg("-pick").output().unwrap();
    let all = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    for seq in ["\x1b[?1049h", "\x1b[2J", "\x1b[7m"] {
        assert!(!all.contains(seq), "must not emit {seq:?}: {all:?}");
    }
}
