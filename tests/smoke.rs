use assert_cmd::Command;

#[test]
fn prints_version() {
    Command::cargo_bin("ws")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::starts_with("ws "));
}

#[test]
fn unknown_dash_command_errors() {
    Command::cargo_bin("ws")
        .unwrap()
        .arg("-nonsense")
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown command"));
}
