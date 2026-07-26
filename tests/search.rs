mod common;
use common::Env;
use predicates::prelude::*;

fn make_ws(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    dir
}

#[test]
fn search_finds_notebook_text_across_workspaces() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    let b = make_ws(&env, "beta");
    std::fs::write(a.join(".ws/notebook/notes.md"), "the kraken retries on 429\n").unwrap();
    std::fs::write(b.join(".ws/README.md"), "# beta\nno sea monsters here\n").unwrap();

    env.cmd().args(["-search", "kraken"]).assert().success()
        .stdout(predicate::str::contains("\nalpha\n"))
        .stdout(predicate::str::contains("429"))
        .stdout(predicate::str::contains("\nbeta\n").not());
}

#[test]
fn search_skips_archived_unless_asked() {
    let env = Env::new();
    let a = make_ws(&env, "old");
    std::fs::write(a.join(".ws/notebook/notes.md"), "kraken lore\n").unwrap();
    env.cmd().args(["-archive", "old"]).assert().success();

    // Anchor on the workspace-header form ("\nold\n"), not a bare "old" substring —
    // macOS temp paths contain "folders", which itself contains "old" as a substring,
    // so a bare `contains("old")` assertion is unreliable in this suite.
    env.cmd().args(["-search", "kraken"]).assert().success()
        .stdout(predicate::str::contains("\nold\n").not());
    env.cmd().args(["-search", "kraken", "--include-archived"]).assert().success()
        .stdout(predicate::str::contains("\nold\n"));
}

#[test]
fn search_never_returns_local_log_contents() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    std::fs::create_dir_all(a.join(".ws/local/log")).unwrap();
    std::fs::write(a.join(".ws/local/log/session.log"), "export TOKEN=hunter2\n").unwrap();

    // The "no matches" message echoes the query text back to the user, so
    // asserting the query string itself is absent from stdout is self-defeating
    // (searching for "hunter2" would make "no matches for \"hunter2\"" fail that
    // assertion even though nothing leaked). Anchor instead on the file name that
    // would appear in a match line, which only shows up if local/ was searched.
    env.cmd().args(["-search", "hunter2"]).assert().success()
        .stdout(predicate::str::contains("session.log").not())
        .stdout(predicate::str::contains("no matches"));
}

#[test]
fn search_with_no_matches_says_so() {
    let env = Env::new();
    make_ws(&env, "alpha");
    env.cmd().args(["-search", "zzzznope"]).assert().success()
        .stdout(predicate::str::contains("no matches"));
}

#[test]
fn search_truncates_a_busy_workspace_and_says_so() {
    let env = Env::new();
    let a = make_ws(&env, "alpha");
    // Well over the MAX_HITS_PER_WORKSPACE (20) cap.
    let busy = (0..40).map(|i| format!("kraken sighting {i}\n")).collect::<String>();
    std::fs::write(a.join(".ws/notebook/notes.md"), busy).unwrap();

    let assert = env.cmd().args(["-search", "kraken"]).assert().success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();

    let match_lines = out.matches("kraken sighting").count();
    assert_eq!(match_lines, 20, "output must cap at MAX_HITS_PER_WORKSPACE: {out}");
    assert!(
        out.contains("more matches in this workspace"),
        "truncation must be called out, not hidden: {out}"
    );
    assert!(
        out.contains("(some results hidden)"),
        "the summary line must not present a truncated count as a total: {out}"
    );
}
