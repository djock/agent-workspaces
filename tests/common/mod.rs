#![allow(dead_code)]
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

/// A temp HOME + WS_ROOT so tests never touch the real config/registry.
pub struct Env {
    pub home: TempDir,
    pub root: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let home = TempDir::new().unwrap();
        let root = home.path().join("agent-workspaces");
        std::fs::create_dir_all(&root).unwrap();
        Env { home, root }
    }

    /// Build a `ws` command with isolated HOME + WS_ROOT env.
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::cargo_bin("ws").unwrap();
        c.env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("WS_ROOT", &self.root)
            .env("XDG_CACHE_HOME", self.home.path().join(".cache"))
            // A launch checks GitHub for a newer release. Tests must never do
            // that: it is slow, it needs `gh` + network, and it would make the
            // suite's output depend on what is published. Tests that mean to
            // exercise the notice set the cache file and unset this.
            .env("WS_NO_UPDATE_CHECK", "1")
            .env_remove("NO_COLOR");
        // ws is developed from inside a ws workspace, so `cargo test` inherits
        // that launch's WS_WORKSPACE/WS_DIR/WS_AGENT. Tests asserting a hook does
        // nothing "outside a workspace" then ran *inside* one and failed — a red
        // suite that says nothing about the code. Each test sets whichever of
        // these it actually means to.
        for leaked in ["WS_WORKSPACE", "WS_DIR", "WS_AGENT"] {
            c.env_remove(leaked);
        }
        c
    }

    /// Seed the release-check cache as if `version` had just been looked up,
    /// so a launch reports on it without going near GitHub.
    pub fn write_update_cache(&self, version: &str) {
        let dir = self.home.path().join(".cache/ws");
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        std::fs::write(dir.join("update-check"), format!("{now} {version}\n")).unwrap();
    }

    /// Write a fake `gh` that answers the two calls the update check makes:
    /// `release view` with `tag`, and `api .../CHANGELOG.md` with `changelog`.
    /// Every call is appended to `gh.log` so a test can prove a cached answer
    /// was reused instead of re-fetched. Point `WS_GH_BIN` at the result.
    pub fn fake_gh(&self, tag: &str, changelog: &str) -> PathBuf {
        let bin = self.home.path().join("fake-gh");
        let md = self.home.path().join("changelog.md");
        std::fs::write(&md, changelog).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             echo \"$*\" >> \"{log}\"\n\
             case \"$1\" in\n\
             api) cat \"{md}\" ;;\n\
             release) echo \"{tag}\" ;;\n\
             *) exit 1 ;;\n\
             esac\n\
             exit 0\n",
            log = self.home.path().join("gh.log").display(),
            md = md.display(),
        );
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&bin).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&bin, p).unwrap();
        }
        bin
    }

    pub fn gh_log(&self) -> String {
        std::fs::read_to_string(self.home.path().join("gh.log")).unwrap_or_default()
    }

    pub fn update_cache(&self) -> String {
        std::fs::read_to_string(self.home.path().join(".cache/ws/update-check")).unwrap_or_default()
    }

    /// Write a fake `claude` shim that appends its argv + selected env to `argv.log`
    /// and exits 0. Returns the shim path (point WS_CLAUDE_BIN at it).
    pub fn fake_claude(&self) -> PathBuf {
        let bin = self.home.path().join("fake-claude");
        let log = self.home.path().join("argv.log");
        let script = format!(
            "#!/bin/sh\n\
             {{\n\
             echo \"ARGS: $*\"\n\
             echo \"CWD: $(pwd)\"\n\
             echo \"MEM: $CLAUDE_COWORK_MEMORY_PATH_OVERRIDE\"\n\
             echo \"WSW: $WS_WORKSPACE\"\n\
             echo \"WSDIR: $WS_DIR\"\n\
             }} >> \"{}\"\n\
             exit 0\n",
            log.display()
        );
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&bin).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&bin, p).unwrap();
        }
        bin
    }

    pub fn argv_log(&self) -> String {
        std::fs::read_to_string(self.home.path().join("argv.log")).unwrap_or_default()
    }

    /// Write a fake `codex` shim that appends its argv + selected env to `codex_argv.log`
    /// and exits 0. Returns the shim path (point WS_CODEX_BIN at it).
    pub fn fake_codex(&self) -> PathBuf {
        let bin = self.home.path().join("fake-codex");
        let log = self.home.path().join("codex_argv.log");
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then echo \"fake-codex 0.0.0\"; exit 0; fi\n\
             {{\n\
             echo \"ARGS: $*\"\n\
             echo \"CWD: $(pwd)\"\n\
             echo \"WSW: $WS_WORKSPACE\"\n\
             echo \"WSDIR: $WS_DIR\"\n\
             }} >> \"{}\"\n\
             exit 0\n",
            log.display()
        );
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&bin).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&bin, p).unwrap();
        }
        bin
    }

    pub fn codex_argv_log(&self) -> String {
        std::fs::read_to_string(self.home.path().join("codex_argv.log")).unwrap_or_default()
    }

    /// Write a fake codex rollout file under the default `$CODEX_HOME/sessions`
    /// (i.e. `<fake HOME>/.codex/sessions`) naming `cwd`, in the same shape
    /// `probe_session_exists` (`src/agents/codex.rs`) looks for: a JSONL file
    /// whose first line is a `session_meta` record carrying `cwd`. `launch`'s
    /// codex resume decision now checks this before trusting the ws-side
    /// marker, so a test asserting `resume --last` needs one of these on disk
    /// or the launch degrades to fresh (see the resume-loop fix, task-2-brief item 2).
    pub fn write_codex_rollout(&self, cwd: &std::path::Path) {
        let dir = self.home.path().join(".codex/sessions/2026/07/28");
        std::fs::create_dir_all(&dir).unwrap();
        let line = format!(
            r#"{{"timestamp":"2026-07-28T00:00:00.000Z","type":"session_meta","payload":{{"id":"test","cwd":{:?},"originator":"codex_cli_rs"}}}}"#,
            cwd.to_string_lossy()
        );
        std::fs::write(dir.join("rollout-test.jsonl"), format!("{line}\n")).unwrap();
    }
}
