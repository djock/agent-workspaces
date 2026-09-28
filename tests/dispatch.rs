mod common;
use common::Env;

/// A workspace adopted at `<home>/projects/<name>` through the real CLI, so the
/// registry and meta are genuine.
fn workspace(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.home.path().join("projects").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    env.cmd().current_dir(&dir).args(["-adopt", name]).assert().success();
    dir.canonicalize().unwrap()
}

fn dispatch_cmd(env: &Env, shim: &std::path::Path) -> assert_cmd::Command {
    let mut c = env.cmd();
    c.env("WS_CLAUDE_BIN", shim).env("WS_NO_EXEC", "1");
    c
}

fn write(env: &Env, name: &str, body: &str) -> std::path::PathBuf {
    let p = env.home.path().join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// The stub agent's log without the `--version` probe `ws` runs first to check
/// the agent is installed: only the real launch is of interest here.
fn launched(env: &Env) -> String {
    env.argv_log()
        .split("ARGS: ")
        .filter(|b| !b.is_empty() && !b.starts_with("--version"))
        .map(|b| format!("ARGS: {b}"))
        .collect()
}

fn make_exec(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_file_launches_one_session_with_every_target_granted() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let web = workspace(&env, "web");
    let f = write(&env, "t.md", "for all\n@web\n- header\n@api\n- retry\n");

    dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().success();

    let log = launched(&env);
    let expected = format!(
        "ARGS: Work through tasks.md in this directory. --add-dir {} {}",
        web.display(),
        api.display()
    );
    assert!(log.contains(&expected), "{log}");
    assert!(log.contains("WSW: \n"), "no workspace identity: {log}");
    let cwd = log.lines().find_map(|l| l.strip_prefix("CWD: ")).unwrap();
    let plan = std::fs::read_to_string(std::path::Path::new(cwd).join("tasks.md")).unwrap();
    assert!(plan.find("## 1. web").unwrap() < plan.find("## 2. api").unwrap());
    assert!(plan.contains("## Unassigned"));
    let tl = std::fs::read_to_string(api.join(".ws/timeline.jsonl")).unwrap();
    assert!(tl.contains("\"dispatched\""), "{tl}");
}

#[test]
fn launched_from_inside_a_workspace_it_does_not_inherit_that_identity() {
    let env = Env::new();
    let shim = env.fake_claude();
    workspace(&env, "api");
    let f = write(&env, "t.md", "@api\n- retry\n");
    dispatch_cmd(&env, &shim)
        .env("WS_WORKSPACE", "other")
        .env("WS_DIR", "/elsewhere")
        .arg("-dispatch")
        .arg(&f)
        .assert()
        .success();
    let log = launched(&env);
    assert!(log.contains("WSW: \n") && log.contains("WSDIR: \n"), "{log}");
}

#[test]
fn a_bad_name_launches_nothing_and_takes_no_lock() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let f = write(&env, "t.md", "@apii\n- retry\n@api\n- b\n");
    let out =
        dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().failure().get_output().clone();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("did you mean 'api'?"), "{err}");
    assert!(launched(&env).is_empty(), "nothing may launch");
    assert!(!api.join(".ws/local/lock").exists(), "no lock may be taken");
}

#[test]
fn the_editor_path_saves_the_text_and_launches() {
    let env = Env::new();
    let shim = env.fake_claude();
    workspace(&env, "api");
    // An "editor" that writes a task list into the file it is given.
    let ed = write(&env, "ed.sh", "#!/bin/sh\nprintf '@api\\n- retry\\n' > \"$1\"\n");
    make_exec(&ed);
    dispatch_cmd(&env, &shim).env("VISUAL", &ed).arg("-dispatch").assert().success();
    assert!(launched(&env).contains("--add-dir"), "{}", launched(&env));
}

#[test]
fn an_unchanged_template_cancels_quietly() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let ed = write(&env, "ed.sh", "#!/bin/sh\nexit 0\n");
    make_exec(&ed);
    let out = dispatch_cmd(&env, &shim)
        .env("VISUAL", &ed)
        .arg("-dispatch")
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to dispatch"));
    assert!(launched(&env).is_empty());
    let tl = std::fs::read_to_string(api.join(".ws/timeline.jsonl")).unwrap_or_default();
    assert!(!tl.contains("dispatched"), "{tl}");
}

#[test]
fn an_editor_list_with_a_typo_is_kept_for_the_rerun() {
    let env = Env::new();
    let shim = env.fake_claude();
    workspace(&env, "api");
    let ed = write(&env, "ed.sh", "#!/bin/sh\nprintf '@apii\\n- retry\\n' > \"$1\"\n");
    make_exec(&ed);
    let out = dispatch_cmd(&env, &shim)
        .env("VISUAL", &ed)
        .arg("-dispatch")
        .assert()
        .failure()
        .get_output()
        .clone();
    let err = String::from_utf8_lossy(&out.stderr);
    let saved = err
        .lines()
        .find_map(|l| l.trim().strip_prefix("your list is saved at "))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(std::fs::read_to_string(saved).unwrap().contains("@apii"));
}

#[test]
fn a_rerun_launches_only_unfinished_projects() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let web = workspace(&env, "web");
    let plan = format!(
        "# Dispatch t\n\n## Protocol\n\n## 1. api — {}\n- [x] done\n\n## 2. web — {}\n- [ ] open\n",
        api.display(),
        web.display()
    );
    let f = write(&env, "tasks.md", &plan);
    dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().success();
    let log = launched(&env);
    let args = log.lines().find(|l| l.starts_with("ARGS: ")).unwrap();
    assert!(args.ends_with(&format!("--add-dir {}", web.display())), "{args}");
    let replan = std::fs::read_to_string(&f).unwrap();
    assert!(!replan.contains("## 1. api"), "the rewritten plan holds only what is left: {replan}");
}

#[test]
fn a_finished_rerun_says_so_and_launches_nothing() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let f = write(
        &env,
        "tasks.md",
        &format!("# Dispatch t\n\n## 1. api — {}\n- [x] done\n", api.display()),
    );
    let out =
        dispatch_cmd(&env, &shim).arg("-dispatch").arg(&f).assert().success().get_output().clone();
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing left to do"));
    assert!(launched(&env).is_empty());
}

#[test]
fn a_missing_agent_is_reported_before_anything_is_touched() {
    let env = Env::new();
    let api = workspace(&env, "api");
    let f = write(&env, "t.md", "@api\n- retry\n");
    let out = env
        .cmd()
        .env("WS_CLAUDE_BIN", env.home.path().join("no-such-claude"))
        .env("WS_NO_EXEC", "1")
        .arg("-dispatch")
        .arg(&f)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&out.stderr).contains("not installed"));
    assert!(!api.join(".ws/local/lock").exists(), "no lock");
    let tl = std::fs::read_to_string(api.join(".ws/timeline.jsonl")).unwrap_or_default();
    assert!(!tl.contains("dispatched"), "no timeline row for a run that never started: {tl}");
}

#[test]
fn rerunning_a_plan_older_than_the_sweep_keeps_it() {
    let env = Env::new();
    let shim = env.fake_claude();
    let api = workspace(&env, "api");
    let dir = env.home.path().join(".cache/ws/dispatch/2020-01-01T00-00-00Z");
    std::fs::create_dir_all(&dir).unwrap();
    let plan = dir.join("tasks.md");
    std::fs::write(&plan, format!("# Dispatch t\n\n## 1. api — {}\n- [ ] open\n", api.display()))
        .unwrap();
    for p in [&plan, &dir] {
        let ok = std::process::Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(p)
            .status()
            .unwrap();
        assert!(ok.success());
    }
    dispatch_cmd(&env, &shim).arg("-dispatch").arg(&plan).assert().success();
    assert!(plan.exists(), "the plan being rerun must not be swept");
}
