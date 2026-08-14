//! Measure the redaction rule against a real population.
//!
//! Scoped to a diff, a review sees a matcher once and judges whether the change
//! looks right. Measured against the population it runs on, the same rule has
//! nowhere to hide: cs's corpus redactor survived twenty-nine releases of
//! diff-scoped review and, when finally run over 2,384 real transcripts, had
//! fired twice — both false positives — and caught zero credentials.
//!
//! This is the harness for doing that here. It is `#[ignore]`d because it reads
//! whatever tree you point it at and its output is a report to read rather than
//! an assertion to pass:
//!
//! ```sh
//! WS_MEASURE_ROOT=~/Projects cargo test --test redact_population -- --ignored --nocapture
//! ```
//!
//! It prints **names, never values**: the whole population is candidate
//! credentials, and a measurement that leaks what it measured would be a worse
//! bug than the one it is looking for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files worth scanning: the ones that carry `NAME=VALUE` lines.
fn interesting(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with(".env") || name == "Makefile" {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "env"
                | "sh"
                | "bash"
                | "zsh"
                | "toml"
                | "ini"
                | "cfg"
                | "conf"
                | "properties"
                | "yml"
                | "yaml"
                | "tf"
                | "tfvars"
                | "service"
                | "example"
                | "sample"
                | "template"
        )
    )
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "build"
            | "dist"
            | ".venv"
            | "venv"
            | "vendor"
            | "Pods"
            | "DerivedData"
            | ".next"
            | ".cache"
    )
}

fn walk(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 20_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !skip_dir(&name) {
                walk(&p, out, depth + 1);
            }
        } else if interesting(&p) {
            out.push(p);
        }
    }
}

/// The rule under measurement, kept byte-identical to `src/internal.rs`'s by
/// `the_measured_rule_matches_the_shipped_one` below. A copy that drifts would
/// measure a rule nobody runs.
#[allow(dead_code)] // the harness measures the matcher, not the placeholder text
mod rule {
    include!("../src/redact_rule.rs");
}

/// The measurement is only worth reading if it measured the shipped rule.
///
/// `include!` gives that for free *as long as it stays the only definition*: the
/// failure mode this guards is someone re-adding the functions to
/// `src/internal.rs` — the rule then ships from there, the harness keeps
/// including a file nobody runs, and the report describes a matcher that is not
/// in the binary.
#[test]
fn the_measured_rule_matches_the_shipped_one() {
    let internal = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("internal.rs"),
    )
    .expect("src/internal.rs");

    assert!(
        internal.contains(r#"include!("redact_rule.rs");"#),
        "src/internal.rs must ship the rule by including src/redact_rule.rs"
    );
    for f in ["fn parse_assignment", "fn is_secret_assignment", "fn name_looks_secret"] {
        assert!(
            !internal.contains(f),
            "`{f}` is defined in src/internal.rs as well as src/redact_rule.rs: \
             the measurement would report on a rule the binary does not use"
        );
    }
}

/// What a line looks like, without saying what it is.
fn shape(value: &str) -> String {
    let mut kinds = Vec::new();
    if value.chars().any(|c| c.is_ascii_digit()) {
        kinds.push("digits");
    }
    if value.chars().any(|c| c.is_ascii_lowercase()) {
        kinds.push("lower");
    }
    if value.chars().any(|c| c.is_ascii_uppercase()) {
        kinds.push("upper");
    }
    if value.chars().any(|c| !c.is_ascii_alphanumeric()) {
        kinds.push("punct");
    }
    format!("len={} {}", value.chars().count(), kinds.join("+"))
}

#[test]
#[ignore = "reads a real tree; run with WS_MEASURE_ROOT and --nocapture"]
fn measure_the_redaction_rule_against_a_real_population() {
    let Ok(root) = std::env::var("WS_MEASURE_ROOT") else {
        eprintln!("set WS_MEASURE_ROOT to the tree to measure");
        return;
    };
    let root = PathBuf::from(shellexpand(&root));
    let mut files = Vec::new();
    walk(&root, &mut files, 0);

    let mut lines = 0usize;
    let mut assignments = 0usize;
    // Count, value shape, and where it was first seen — a name with no file to
    // go and look at is a number you cannot check.
    let mut fired: BTreeMap<String, (usize, String, PathBuf)> = BTreeMap::new();
    // Names that look credential-shaped but whose value the rule let through:
    // the false-negative side, which a firing count alone cannot see.
    let mut name_only: BTreeMap<String, (usize, String, PathBuf)> = BTreeMap::new();
    // Values ws has already redacted. Counted apart because they are the one
    // non-fire that means the rule worked: read as near-misses they made the
    // miss list four times longer than the real one.
    let mut already: BTreeMap<String, (usize, String, PathBuf)> = BTreeMap::new();

    for f in &files {
        let Ok(body) = std::fs::read_to_string(f) else { continue };
        for line in body.lines() {
            lines += 1;
            let Some((name, value)) = rule::parse_assignment(line) else { continue };
            assignments += 1;
            if rule::is_secret_assignment(name, value) {
                let e = fired.entry(name.to_string()).or_insert((0, shape(value), f.clone()));
                e.0 += 1;
            } else if value.starts_with(rule::PLACEHOLDER_OPEN) {
                let e = already.entry(name.to_string()).or_insert((0, shape(value), f.clone()));
                e.0 += 1;
            } else if rule::name_looks_secret(name) && !value.is_empty() {
                let e = name_only.entry(name.to_string()).or_insert((0, shape(value), f.clone()));
                e.0 += 1;
            }
        }
    }

    println!("\n=== redaction rule, measured ===");
    println!("root:          {}", root.display());
    println!("files scanned: {}", files.len());
    println!("lines read:    {lines}");
    println!("assignments:   {assignments}");
    println!("fired:         {} distinct names", fired.len());
    println!("\n-- fired (would be redacted and moved into the store) --");
    for (name, (n, shape, first)) in &fired {
        println!("  {n:>4}x  {name}   [{shape}]  {}", first.display());
    }
    println!("\n-- already redacted by ws (left alone, as it must be) --");
    for (name, (n, shape, first)) in &already {
        println!("  {n:>4}x  {name}   [{shape}]  {}", first.display());
    }
    println!("\n-- credential-shaped name, value not judged secret (possible misses) --");
    for (name, (n, shape, first)) in &name_only {
        println!("  {n:>4}x  {name}   [{shape}]  {}", first.display());
    }
    println!(
        "\nJudge both lists by eye: every line above is a claim about a population, \
         and a rule that fires often and correctly is a different animal from one \
         that fires often.\n"
    );
}

fn shellexpand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{}", std::env::var("HOME").unwrap_or_default(), rest),
        None => p.to_string(),
    }
}
