//! The installer's verification gates, driven end to end.
//!
//! `install.sh` is the one piece of ws that runs before ws exists, and its two
//! gates — authenticity, then integrity — are the only thing standing between a
//! replaced release asset and a binary on your PATH. They had never been
//! exercised: the authenticity block was skipped entirely and silently whenever
//! no public key was configured, which is the default, so every stock install
//! printed a checksum pass and nothing else. That reads as "verified".
//!
//! These tests run the real script against a fabricated release, with `gh`
//! stubbed on PATH. Nothing here reaches the network.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERSION: &str = "9.9.9";

fn target() -> &'static str {
    if cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-musl"
    }
}

fn asset_name() -> String {
    format!("ws-v{VERSION}-{}.tar.gz", target())
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut p = std::fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(path, p).unwrap();
}

fn sha256_of(file: &Path) -> String {
    // Whichever digest tool this host has is the one install.sh will use, so
    // the fixture is built with the same one rather than a Rust reimplementation
    // that could agree with neither.
    let (prog, args): (&str, Vec<&str>) =
        if which("sha256sum") { ("sha256sum", vec![]) } else { ("shasum", vec!["-a", "256"]) };
    let out = Command::new(prog).args(&args).arg(file).output().unwrap();
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().to_string()
}

fn which(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A fabricated release: a tarball holding a runnable `ws`, plus its SHA256SUMS.
struct Release {
    dir: tempfile::TempDir,
}

impl Release {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        // install.sh finishes by running `ws --version`, so the payload has to
        // actually execute.
        write_exec(&stage.join("ws"), &format!("#!/bin/sh\necho \"ws {VERSION}\"\n"));

        let asset = dir.path().join(asset_name());
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&asset)
            .arg("-C")
            .arg(&stage)
            .arg("ws")
            .status()
            .unwrap();
        assert!(ok.success(), "could not build the fixture tarball");

        let sums = format!("{}  {}\n", sha256_of(&asset), asset_name());
        std::fs::write(dir.path().join("SHA256SUMS"), sums).unwrap();
        Release { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Publish a signature file. Its contents are never valid — every test that
    /// uses one is about what happens when nothing can check it.
    fn with_signature(self) -> Self {
        std::fs::write(self.path().join("SHA256SUMS.minisig"), "untrusted comment: fake\n")
            .unwrap();
        self
    }

    /// Corrupt the asset after its checksum was recorded.
    fn tampered(self) -> Self {
        let mut f =
            std::fs::OpenOptions::new().append(true).open(self.path().join(asset_name())).unwrap();
        f.write_all(b"tampered").unwrap();
        self
    }
}

/// A `gh` that serves the fabricated release and nothing else.
fn stub_gh(bin_dir: &Path, release: &Path) {
    std::fs::create_dir_all(bin_dir).unwrap();
    write_exec(
        &bin_dir.join("gh"),
        &format!(
            r#"#!/bin/sh
# Stub gh. `release view` names the tag; `release download` copies whatever the
# fixture has into --dir, ignoring --pattern (install.sh checks for itself that
# what it needs arrived, which is the behaviour under test).
case "$1 $2" in
  "auth status") exit 0 ;;
  "release view") echo "v{VERSION}"; exit 0 ;;
  "release download")
     dir=""
     while [ "$#" -gt 0 ]; do
       case "$1" in --dir) shift; dir="$1" ;; esac
       shift
     done
     for f in {release}/*; do
       [ -f "$f" ] && cp "$f" "$dir/"
     done
     exit 0 ;;
esac
exit 1
"#,
            release = release.display()
        ),
    );
}

struct Run {
    output: Output,
    destination: PathBuf,
    _home: tempfile::TempDir,
}

impl Run {
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).to_string()
    }
    fn installed(&self) -> bool {
        self.destination.exists()
    }
}

/// Run the real `install.sh` against `release`, with `gh` stubbed.
fn install(release: &Release, pubkey: Option<&str>, extra: &[&str]) -> Run {
    let home = tempfile::TempDir::new().unwrap();
    let stub = home.path().join("stub");
    stub_gh(&stub, release.path());
    let install_dir = home.path().join("bin");

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let mut cmd = Command::new("sh");
    cmd.arg(&script)
        .arg("--version")
        .arg(VERSION)
        .arg("--install-dir")
        .arg(&install_dir)
        .arg("--no-setup")
        .args(extra)
        .env("PATH", format!("{}:{}", stub.display(), std::env::var("PATH").unwrap()))
        .env("HOME", home.path())
        .env("WS_REPOSITORY", "example/ws")
        .env("WS_MINISIGN_PUBKEY", pubkey.unwrap_or(""));

    let output = cmd.output().unwrap();
    Run { output, destination: install_dir.join("ws"), _home: home }
}

/// The regression this file exists for: with no key configured the gate cannot
/// establish anything, and it used to say nothing at all — leaving a run whose
/// only verification output was a checksum pass, which reads as "verified".
#[test]
fn an_unverifiable_release_says_so_instead_of_installing_quietly() {
    let run = install(&Release::new(), None, &[]);
    let err = run.stderr();
    assert!(run.installed(), "an unsigned release still installs: {err}");
    assert!(
        err.contains("authenticity was NOT checked"),
        "the missing authenticity check must be announced: {err}"
    );
    assert!(
        err.contains("no signing key is published"),
        "and it must say why it could not be checked: {err}"
    );
}

/// A signature nobody can verify is worse than none: it is what a stripped key
/// looks like. That case refuses rather than warning.
#[test]
fn a_signed_release_with_no_key_to_check_it_refuses() {
    let run = install(&Release::new().with_signature(), None, &[]);
    assert!(!run.installed(), "must not install: {}", run.stderr());
    assert!(
        run.stderr().contains("carries no public key"),
        "the refusal names the cause: {}",
        run.stderr()
    );
}

#[test]
fn that_refusal_is_passable_only_by_typing_allow_unsigned() {
    let run = install(&Release::new().with_signature(), None, &["--allow-unsigned"]);
    assert!(run.installed(), "--allow-unsigned must get past it: {}", run.stderr());
    assert!(run.stderr().contains("WARNING"), "and must still warn: {}", run.stderr());
}

/// With a key configured, an unsigned release is refused — the gate fails
/// closed, which is the property the whole block is for.
#[test]
fn a_configured_key_refuses_an_unsigned_release() {
    let run = install(&Release::new(), Some("RWTfakekeyfakekeyfakekey"), &[]);
    assert!(!run.installed(), "must not install: {}", run.stderr());
    assert!(run.stderr().contains("not signed"), "the refusal names the cause: {}", run.stderr());
}

/// Integrity, independently of authenticity: a payload that does not match the
/// checksum never reaches the install directory.
#[test]
fn a_tampered_asset_never_reaches_the_install_directory() {
    let run = install(&Release::new().tampered(), None, &[]);
    assert!(!run.installed(), "a corrupt asset was installed: {}", run.stderr());
}
