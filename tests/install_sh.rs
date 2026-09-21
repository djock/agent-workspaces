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
# what it needs arrived, which is the behaviour under test). Like the real gh,
# it nags about its own upgrades unless GH_NO_UPDATE_NOTIFIER is set.
[ -n "${{GH_NO_UPDATE_NOTIFIER:-}}" ] || echo "A new release of gh is available: 2.0.0 → 2.1.0" >&2
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
    install_env(release, pubkey, extra, &[])
}

fn install_env(
    release: &Release,
    pubkey: Option<&str>,
    extra: &[&str],
    envs: &[(&str, &str)],
) -> Run {
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
        .env("WS_MINISIGN_PUBKEY", pubkey.unwrap_or(""))
        .env_remove("GH_NO_UPDATE_NOTIFIER")
        .envs(envs.iter().copied());

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

/// gh's "a new release of gh is available" is not ours to print; mid-install it
/// read like a failure.
#[test]
fn the_gh_upgrade_notice_is_kept_out_of_the_install() {
    let run = install(&Release::new(), None, &[]);
    assert!(run.installed(), "{}", run.stderr());
    assert!(!run.stderr().contains("new release of gh"), "gh nagged: {}", run.stderr());
}

/// `ws -update` narrates the install itself, so the installer it runs keeps to
/// warnings — but the unsigned-release warning is one of them and must survive.
#[test]
fn quiet_mode_prints_only_warnings() {
    let run = install_env(&Release::new(), None, &[], &[("WS_INSTALL_QUIET", "1")]);
    let out = String::from_utf8_lossy(&run.output.stdout).to_string();
    let err = run.stderr();
    assert!(run.installed(), "{err}");
    // The PATH hint may still appear (it is advice, not progress); the checksum
    // line and the "Installed" line are what `ws -update` replaces.
    assert!(!out.contains(": OK"), "quiet mode printed the checksum line: {out}");
    assert!(!out.contains("Installed"), "quiet mode printed the install line: {out}");
    assert!(err.contains("authenticity was NOT checked"), "the warning must survive: {err}");
    assert_eq!(err.trim().lines().count(), 1, "one warning line, not a paragraph: {err}");
}

/// Quiet must not swallow a checksum mismatch: that is an error, not progress.
#[test]
fn quiet_mode_still_reports_a_tampered_asset() {
    let run = install_env(&Release::new().tampered(), None, &[], &[("WS_INSTALL_QUIET", "1")]);
    assert!(!run.installed(), "a corrupt asset was installed: {}", run.stderr());
    assert!(run.stderr().contains(&asset_name()), "the mismatch must be named: {}", run.stderr());
}

/// A throwaway minisign keypair, returning the public key's base64 line.
/// `None` when minisign is not installed, so the caller can skip.
fn keypair(dir: &Path) -> Option<String> {
    if !which("minisign") {
        eprintln!("skipping: minisign is not installed");
        return None;
    }
    let (p, s) = (dir.join("k.pub"), dir.join("k.key"));
    let ok = Command::new("minisign")
        .args(["-G", "-W", "-p"])
        .arg(&p)
        .arg("-s")
        .arg(&s)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    Some(std::fs::read_to_string(&p).unwrap().lines().nth(1).unwrap().to_string())
}

fn sign(release: &Release, keys: &Path) {
    let ok = Command::new("minisign")
        .arg("-S")
        .arg("-s")
        .arg(keys.join("k.key"))
        .arg("-m")
        .arg(release.path().join("SHA256SUMS"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
}

/// The path every release takes once a key is published: signed, verified,
/// installed — and in quiet mode, with no warning at all.
#[test]
fn a_correctly_signed_release_installs_without_a_warning() {
    let keys = tempfile::TempDir::new().unwrap();
    let Some(pubkey) = keypair(keys.path()) else { return };
    let release = Release::new();
    sign(&release, keys.path());
    let run = install_env(&release, Some(&pubkey), &[], &[("WS_INSTALL_QUIET", "1")]);
    let out = String::from_utf8_lossy(&run.output.stdout).to_string();
    assert!(run.installed(), "{}", run.stderr());
    assert!(out.contains("signature verified"), "{out}");
    assert!(!run.stderr().contains("NOT checked"), "{}", run.stderr());
}

/// What a compromised release host can produce: a valid signature, from the
/// wrong key.
#[test]
fn a_signature_from_another_key_is_refused() {
    let (ours, theirs) = (tempfile::TempDir::new().unwrap(), tempfile::TempDir::new().unwrap());
    let Some(pubkey) = keypair(ours.path()) else { return };
    keypair(theirs.path()).unwrap();
    let release = Release::new();
    sign(&release, theirs.path());
    let run = install(&release, Some(&pubkey), &[]);
    assert!(!run.installed(), "a foreign signature was accepted: {}", run.stderr());
    assert!(run.stderr().contains("SIGNATURE VERIFICATION FAILED"), "{}", run.stderr());
}

/// The published key must stay baked in. Blanking it would not fail anything
/// else: installs would quietly drop back to "authenticity NOT checked".
#[test]
fn install_sh_ships_the_release_public_key() {
    let script =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh")).unwrap();
    let line = script.lines().find(|l| l.starts_with("MINISIGN_PUBKEY=")).unwrap();
    assert!(line.contains("${WS_MINISIGN_PUBKEY-RW"), "no default public key: {line}");
}
