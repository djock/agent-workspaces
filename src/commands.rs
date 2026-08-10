use crate::agents::{self, LaunchCtx};
use crate::cli::ConfigCmd;
use crate::cli::SecretsCmd;
use crate::config;
use crate::context;
use crate::contract;
use crate::limits;
use crate::lock;
use crate::registry;
use crate::secrets;
use crate::term;
use crate::workspace;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::io::Read;

pub fn secrets(cmd: SecretsCmd) -> Result<()> {
    // Before `workspace_name`/`open`: asking what the subcommands are must work
    // outside a workspace, and must never trigger the file backend's master
    // password prompt.
    if matches!(cmd, SecretsCmd::Help) {
        println!("{}", crate::cli::SECRETS_USAGE);
        return Ok(());
    }
    let ws = secrets::workspace_name()?;
    let store = secrets::open(&ws)?;
    match cmd {
        SecretsCmd::Set(name) => {
            let mut value = String::new();
            std::io::stdin().read_to_string(&mut value)?;
            let value = value.strip_suffix('\n').unwrap_or(&value);
            store.set(&name, value)?;
            println!("stored {name}"); // never echoes the value
        }
        SecretsCmd::Get(name) => match store.get(&name)? {
            Some(v) => println!("{v}"),
            // A name the index lists but the vault cannot resolve is not the
            // same failure as a name nobody ever stored, and saying "no such
            // secret" for both sent people hunting for a typo. Versions before
            // 0.6.1 linked `keyring` with no platform feature, so the keyring
            // backend wrote to an in-memory mock that was discarded at process
            // exit while the on-disk index kept the name. Those values are not
            // recoverable — say so, and say what to do instead.
            None if store.list().unwrap_or_default().iter().any(|n| n == &name) => {
                anyhow::bail!(
                    "{name} is listed for workspace {ws} but its value is missing from the \
                     {} store.\nws before 0.6.3 reported `set` as succeeding while storing \
                     nothing that survived the process — if {name} was stored by one of those \
                     versions the value is gone and cannot be recovered.\nStore it again with \
                     `ws -secrets set {name}`, or drop the stale name with `ws -secrets rm {name}`.",
                    store.backend_name(),
                )
            }
            None => anyhow::bail!("no such secret: {name}"),
        },
        SecretsCmd::List => {
            for n in store.list()? {
                println!("{n}");
            }
        }
        SecretsCmd::Rm(name) => {
            store.remove(&name)?;
            println!("removed {name}");
        }
        SecretsCmd::Purge => {
            if std::io::stdin().is_terminal() {
                eprint!("Purge ALL secrets for workspace {ws}? [y/N] ");
                use std::io::Write;
                std::io::stderr().flush().ok();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    println!("cancelled");
                    return Ok(());
                }
            } else {
                anyhow::bail!("refusing to purge without a TTY to confirm");
            }
            store.purge()?;
            println!("purged all secrets for {ws}");
        }
        SecretsCmd::Export => {
            for n in store.list()? {
                if let Some(v) = store.get(&n)? {
                    let escaped = v.replace('\'', "'\\''");
                    println!("export {n}='{escaped}'");
                }
            }
        }
        SecretsCmd::Backend => println!("{}", store.backend_name()),
        SecretsCmd::Restore(file) => secrets_restore(&ws, store.as_ref(), &file)?,
        SecretsCmd::Help => unreachable!("handled before the store is opened"),
    }
    Ok(())
}

/// `ws -secrets restore <file>` — put stored values back where the redaction
/// hook took them from.
///
/// The hook writes `{{ws:secret:NAME}}` placeholders, and until this existed no
/// code path anywhere resolved one: a redacted `.env` was simply a broken
/// `.env`, and the only honest advice was to keep the value out of the file
/// yourself. This is the other half of that feature.
fn secrets_restore(ws_name: &str, store: &dyn secrets::SecretStore, file: &str) -> Result<()> {
    let root = workspace_root(ws_name)?;
    let arg = std::path::Path::new(file);
    let path = if arg.is_absolute() { arg.to_path_buf() } else { std::env::current_dir()?.join(arg) };
    // The same containment rule the hook applies, for a sharper reason: this
    // writes *plaintext credentials*. Resolved rather than compared textually,
    // so neither `../` nor a symlink can spell its way out of the workspace.
    let path = crate::internal::contained(&root, &path)
        .map_err(|reason| anyhow::anyhow!("refusing to restore: {reason}"))?;

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    // Capture the mode before the rewrite: a redacted `.env` is commonly 0600,
    // and restoring the plaintext under the process umask instead would publish
    // the credential to every account on the machine.
    let mode = crate::atomic::mode_of(&path);
    let done = crate::internal::resolve_placeholders(&text, |name| store.get(name))?;

    if done.resolved > 0 {
        crate::atomic::atomic_write_with_mode(&path, &done.text, mode)
            .with_context(|| format!("failed to rewrite {}", path.display()))?;
    }
    println!("restored {} placeholder(s) in {}", done.resolved, path.display());
    if !done.missing.is_empty() {
        // Non-zero exit, and the placeholders stay: a script that pipes this
        // into a deploy must not proceed with a file that still has holes in it.
        anyhow::bail!(
            "no such secret in workspace {ws_name}: {} — those placeholders were left in place",
            done.missing.join(", ")
        );
    }
    Ok(())
}

/// Where the workspace whose store is open actually lives on disk.
///
/// `restore` is the only `-secrets` subcommand that needs the root rather than
/// the name, because it is the only one that touches a path. Inside a launched
/// session `$WS_DIR` names it; outside one the registry does (the same lookup
/// `-msg` and `-queue` use, checked so an unreadable registry is an error
/// rather than a guess); a workspace directory that was never registered falls
/// back to the cwd.
fn workspace_root(name: &str) -> Result<std::path::PathBuf> {
    if let Some(dir) = std::env::var("WS_DIR").ok().filter(|s| !s.is_empty()) {
        return Ok(std::path::PathBuf::from(dir));
    }
    if let Some(p) = registry::lookup_checked(name)? {
        return Ok(p);
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".ws").is_dir() {
        return Ok(cwd);
    }
    anyhow::bail!(
        "cannot tell where workspace {name} lives \
         (run this inside the workspace directory, or launch it with `ws {name}` first)"
    )
}

pub fn setup() -> Result<()> {
    let ws_bin = std::env::current_exe()?;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        if !agent.is_installed() {
            continue;
        }
        let nh = crate::hooksetup::install_hooks_for(&agent.hooks_config_path(), &ws_bin, agent.as_ref())?;
        let np = crate::prompts::install_for(&agent.prompts_dir(), |b| agent.prompt_filename(b))?;
        println!(
            "ws setup [{}]: installed {nh} hook(s) → {}\n            installed {np} prompt(s) → {}",
            agent.id(),
            agent.hooks_config_path().display(),
            agent.prompts_dir().display(),
        );
        if let Some(note) = agent.hook_trust_note() {
            println!("  note: {note}");
        }
    }
    // Configure each installed agent's status bar with the same core fields —
    // unless the user turned it off. `statusline` used to be settable and read
    // nowhere, so `ws config set statusline false` reported success and the next
    // `ws setup` claimed the status bar anyway.
    if !config::load().statusline {
        println!("skipped status line registration (config statusline = false)");
        return Ok(());
    }
    if crate::agents::for_id("claude")?.is_installed() {
        crate::hooksetup::register_statuslines(&ws_bin)?;
        println!("registered Claude statusline + subagent-statusline");
    }
    if crate::agents::for_id("codex")?.is_installed() {
        crate::hooksetup::register_codex_statusline()?;
        println!("registered Codex statusline");
    }
    Ok(())
}

/// A note when `cwd` is a workspace whose `.ws/` the repository ignores.
///
/// `contract::init` commits `.ws/` on creation, writes `.ws/.gitignore` to keep
/// `local/` and the encrypted store out of it, and writes `.ws/.gitattributes`
/// giving the append-only files `merge=union`. All three assume `.ws/` is
/// tracked. When an ancestor gitignore excludes it, `init`'s `git add -- .ws`
/// fails, the staged-diff check finds nothing, and the commit is skipped
/// **silently** — so the loss shows up only as notebooks that never reach a
/// co-developer and a worktree merge that conflicts where it should have
/// unioned.
///
/// Reported as a note, never a failure: ignoring `.ws/` is the right call for a
/// public repository whose working notes are not meant to ship, which is what
/// this repository itself does.
fn gitignored_ws_note(cwd: &std::path::Path) -> Option<String> {
    if !cwd.join(".ws").is_dir() {
        return None;
    }
    // `check-ignore -q` exits 0 when the path is ignored, 1 when it is not, and
    // 128 outside a repository — so anything but 0 means there is nothing to say.
    let ignored = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["check-ignore", "-q", ".ws"])
        .status()
        .ok()?
        .code()
        == Some(0);
    if !ignored {
        return None;
    }
    Some(
        "… .ws/ is gitignored — ws could not record its init commit, notebooks and\n  \
         handoffs are not shared with anyone cloning this repo, and merge=union in\n  \
         .ws/.gitattributes cannot apply (a merge driver only runs on tracked files).\n  \
         Deliberate for a repo whose notes stay local; otherwise un-ignore .ws/."
            .to_string(),
    )
}

pub fn doctor() -> Result<()> {
    let mut any_agent = false;
    let mut hard_fail = false;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        if agent.is_installed() {
            any_agent = true;
            println!("✓ {id}: installed ({})", agent_version(&agent.binary()));
            let cfg_path = agent.hooks_config_path();
            // Absent, unreadable and registered are three different answers.
            // Folding "unreadable" into "not registered" printed the same line
            // for both — in the one command whose entire job is telling you what
            // is actually wrong.
            match crate::io_read::read_or_absent(&cfg_path) {
                Ok(Some(s))
                    if s.contains(&crate::hooksetup::hooks_dir().to_string_lossy().to_string()) =>
                {
                    println!("  ✓ ws hooks registered in {}", cfg_path.display());
                }
                Ok(Some(_)) => println!(
                    "  … ws hooks not registered in {} — run `ws setup`",
                    cfg_path.display()
                ),
                Ok(None) => println!(
                    "  … {} does not exist yet — run `ws setup`",
                    cfg_path.display()
                ),
                Err(e) => {
                    println!("  ✗ cannot read {}: {e:#}", cfg_path.display());
                    hard_fail = true;
                }
            }
            if let Some(note) = agent.hook_trust_note() {
                println!("  note: {note}");
            }
        } else {
            println!("… {id}: not installed");
        }
    }
    // shims present?
    let shim = crate::hooksetup::hooks_dir().join("session-start.sh");
    if shim.exists() {
        println!("✓ ws hook scripts present");
    } else {
        println!("… ws hook scripts missing — run `ws setup`");
    }

    // User-defined hooks: an invalid hooks.toml means `ws setup` will refuse, and
    // the user should hear that here rather than the next time they run setup.
    let hooks_toml = crate::hooks_user::hooks_toml_path();
    match crate::hooks_user::load() {
        Ok(hooks) if hooks.is_empty() => {
            println!("✓ no user hooks ({})", hooks_toml.display());
        }
        Ok(hooks) => {
            println!("✓ {} user hook(s) in {}", hooks.len(), hooks_toml.display());
            println!("  see `ws hooks list` for what each agent registers");
        }
        Err(e) => {
            println!("✗ {} is invalid: {e:#}", hooks_toml.display());
            hard_fail = true;
        }
    }

    if let Some(note) = gitignored_ws_note(&std::env::current_dir()?) {
        println!("{note}");
    }

    if !any_agent {
        eprintln!("ws: no agent installed (need claude or codex on PATH)");
        hard_fail = true;
    }
    if hard_fail {
        // A returned error, not `process::exit`: exiting from inside a
        // `-> Result` skips `main`'s error formatting, and (as `drain` had to
        // learn) also skips every `Drop`, so any guard held here would leak.
        anyhow::bail!("doctor found problems (see above)");
    }
    Ok(())
}

pub fn uninstall(force: bool) -> Result<()> {
    if !force {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("refusing to uninstall without a TTY; use `ws -uninstall --force`");
        }
        eprint!(
            "Remove the ws binary, hooks, prompts, and status lines?\n\
             Workspaces and configuration will be kept. [y/N] "
        );
        use std::io::Write;
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
            println!("cancelled");
            return Ok(());
        }
    }

    let ws_bin = std::env::current_exe().context("cannot locate the current ws binary")?;
    if ws_bin.file_name().and_then(|name| name.to_str()) != Some("ws") {
        anyhow::bail!(
            "refusing to remove unexpected executable {}; remove it manually",
            ws_bin.display()
        );
    }
    // A cargo build artifact is not an installation, and deleting one is never
    // what the user meant: the name check above passes for `target/debug/ws`, so
    // running this in a checkout removed the binary cargo had just built. The
    // *integrations* are still unregistered either way — they are what `ws setup`
    // wrote into the user's agent config, and leaving them pointing at a shim
    // directory that is about to go is worse than doing nothing.
    let is_build_artifact = ws_bin.components().any(|c| c.as_os_str() == "target");

    let mut hooks = 0;
    let mut prompts = 0;
    for id in ["claude", "codex"] {
        let agent = crate::agents::for_id(id)?;
        hooks += crate::hooksetup::unregister_hooks_for(&agent.hooks_config_path())?;
        prompts += crate::prompts::uninstall_for(&agent.prompts_dir(), |base| {
            agent.prompt_filename(base)
        })?;
    }
    let statuslines = crate::hooksetup::unregister_statuslines(&ws_bin)?
        + crate::hooksetup::unregister_codex_statusline()?;
    let scripts = crate::hooksetup::remove_hook_scripts()?;
    println!(
        "Removed ws integrations ({hooks} hooks, {scripts} scripts, {prompts} prompts, \
         {statuslines} status lines)."
    );

    if is_build_artifact {
        anyhow::bail!(
            "{} looks like a cargo build artifact, not an installed ws, so it was left in \
             place. Uninstall the copy on your PATH instead.",
            ws_bin.display()
        );
    }

    std::fs::remove_file(&ws_bin)
        .with_context(|| format!("failed to remove {}", ws_bin.display()))?;
    println!("Uninstalled ws from {}.", ws_bin.display());
    println!("Your workspaces and ws configuration were kept.");
    Ok(())
}

fn agent_version(bin: &str) -> String {
    std::process::Command::new(bin).arg("--version").output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn adopt(name: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let name = match name {
        Some(n) => n,
        None => cwd
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("cannot derive a workspace name from {}", cwd.display()))?,
    };
    if cwd.join(".ws").is_dir() {
        // Already a workspace: just (re)register.
        crate::registry::register(&name, &cwd)?;
        println!("re-registered existing workspace: {name}");
        return Ok(());
    }
    let cfg = config::load();
    let agent = cfg.default_agent.clone();
    contract::init(&name, &cwd, &agent, /* commit */ false)?;
    println!("adopted {name} at {}", cwd.display());
    Ok(())
}

/// The one resolver: which workspace does this command apply to?
///
/// There used to be four, and two of them disagreed in a way that silently split
/// a workspace's data. `current_or_named` reported the name from
/// `workspace.toml`, while `secrets::workspace_name` used the *current
/// directory's* name — so for a directory adopted under a different name,
/// `ws -secrets set` wrote to a different store than `ws -tag`/`-task` named.
/// Everything now comes through here, so there is one answer to the question.
pub(crate) fn resolve_named(name: Option<String>) -> Result<crate::workspace::Workspace> {
    let (name, root) = current_or_named(name)?;
    Ok(crate::workspace::Workspace { name, root })
}

/// Resolve which workspace a metadata command applies to: an explicit name,
/// else $WS_WORKSPACE, else the current directory if it is a workspace.
pub(crate) fn current_or_named(name: Option<String>) -> Result<(String, std::path::PathBuf)> {
    if let Some(n) = name {
        let path = registry::lookup(&n)
            .ok_or_else(|| anyhow::anyhow!("no such workspace: {n}"))?;
        return Ok((n, path));
    }
    if let Ok(n) = std::env::var("WS_WORKSPACE") {
        if let Some(path) = registry::lookup(&n) {
            return Ok((n, path));
        }
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".ws").is_dir() {
        let n = crate::meta::read(&cwd.join(".ws/workspace.toml")).name;
        let n = if n.is_empty() {
            cwd.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()
        } else {
            n
        };
        return Ok((n, cwd));
    }
    anyhow::bail!("not in a workspace (name one with --workspace, or run inside one)")
}

pub fn tag(cmd: crate::cli::TagCmd) -> Result<()> {
    use crate::cli::TagCmd;
    let name = match &cmd {
        TagCmd::Add { name, .. } | TagCmd::Rm { name, .. } | TagCmd::List { name } => name.clone(),
    };
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    match cmd {
        TagCmd::Add { tags, .. } => {
            // The contract gate covers mutating entry points; List (below)
            // does not go through it — a read must not refuse just because a
            // newer `ws` touched this workspace.
            contract::check_gate(&ws_name, &wt)?;
            let all = crate::meta::add_tags(&wt, &tags)?;
            println!("{ws_name}: {}", all.join(" "));
        }
        TagCmd::Rm { tags, .. } => {
            contract::check_gate(&ws_name, &wt)?;
            let all = crate::meta::remove_tags(&wt, &tags)?;
            println!("{ws_name}: {}", all.join(" "));
        }
        TagCmd::List { .. } => {
            let all = crate::meta::read(&wt).tags;
            if all.is_empty() {
                println!("{ws_name}: (no tags)");
            } else {
                println!("{ws_name}: {}", all.join(" "));
            }
        }
    }
    Ok(())
}

pub fn status(name: Option<String>, text: Option<String>) -> Result<()> {
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    contract::check_gate(&ws_name, &wt)?;
    crate::meta::set_status(&wt, text.as_deref())?;
    match text {
        Some(t) => println!("{ws_name}: {t}"),
        None => println!("{ws_name}: status cleared"),
    }
    Ok(())
}

/// Set or clear the workspace color. Clearing leaves it uncolored only until the
/// next launch, which backfills a new one — there is no permanently drab state.
pub fn color(name: Option<String>, color: Option<String>) -> Result<()> {
    let (ws_name, path) = current_or_named(name)?;
    let wt = path.join(".ws/workspace.toml");
    contract::check_gate(&ws_name, &wt)?;
    crate::meta::set_color(&wt, color.as_deref())?;
    match color {
        Some(c) => println!("{ws_name}: color {c} (applies on the next launch)"),
        None => println!("{ws_name}: color cleared; the next launch will pick a new one"),
    }
    Ok(())
}

/// Whether the launch should stop and ask before resuming.
///
/// Pure so the four conditions can be tested without a terminal. Every one of
/// them is a case where asking would be wrong rather than merely unhelpful:
/// `--fresh` is already an answer, a workspace with no recorded session has
/// nothing to resume, a disabled prompt is an explicit preference, and a launch
/// with no TTY has nobody to answer — that last one would hang a scripted `ws`
/// forever on a read that never returns.
fn should_ask_new(has_prior: bool, fresh: bool, enabled: bool, tty: bool) -> bool {
    has_prior && !fresh && enabled && tty
}

/// Ask whether to resume the previous conversation. Defaults to No — pressing
/// Enter starts a fresh one, and only `y` resumes.
///
/// Asked in the positive ("resume?") rather than as "start a new conversation?",
/// because the prompt should name the thing being *kept*: the previous
/// conversation is the only object in play the user might not want to lose.
/// Nothing here is destructive either way — an unresumed conversation is still
/// listed by `ws -conversations`.
fn ask_resume(name: &str) -> bool {
    use std::io::Write;
    eprint!("Resume previous conversation in {name}? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    // A read that fails (closed stdin, EOF) is not a yes; it takes the default
    // the prompt just advertised rather than quietly doing the other thing.
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    answer_resumes(&line)
}

/// Does this answer mean "resume"? Split out so the mapping is testable without
/// a terminal — it is the one place the launch decides between continuing a
/// conversation and starting over.
fn answer_resumes(line: &str) -> bool {
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
}

pub fn archive(names: Vec<String>, archived: bool) -> Result<()> {
    let mut failed = false;
    for name in names {
        let path = match registry::lookup(&name) {
            Some(p) => p,
            None => {
                eprintln!("ws: no such workspace: {name}");
                failed = true;
                continue;
            }
        };
        let wt = path.join(".ws/workspace.toml");
        if let Err(e) = contract::check_gate(&name, &wt) {
            eprintln!("ws: {e}");
            failed = true;
            continue;
        }
        if let Err(e) = crate::meta::set_archived(&wt, archived) {
            eprintln!("ws: failed to update {name}: {e}");
            failed = true;
            continue;
        }
        println!("{name}: {}", if archived { "archived" } else { "unarchived" });
    }
    if failed {
        anyhow::bail!("some workspaces could not be updated");
    }
    Ok(())
}

pub fn list(tag: Option<String>, archived: bool) -> Result<()> {
    let opts = crate::rows::ListOpts { tag: tag.clone(), include_archived: archived };
    let listing = crate::rows::list_all(&opts)?;
    if listing.rows.is_empty() {
        // I8: "you have none" and "none matched" are different sentences, and
        // they were the wrong way round. Only the unfiltered count can tell
        // them apart — pointing a user with an empty registry at
        // `-list --archived` sends them after workspaces that cannot exist.
        match (listing.total, tag) {
            (0, _) => println!("no workspaces yet — create one with: ws <name>"),
            (_, Some(t)) => println!("no workspaces tagged {t}"),
            // Registered workspaces exist but all are archived. (With
            // --archived and no tag nothing is filtered, so this is the
            // plain-`-list` case.)
            (_, None) => println!("no active workspaces (try: ws -list --archived)"),
        }
        return Ok(());
    }
    for r in listing.rows {
        let state = match &r.state {
            crate::rows::RowState::Ok => String::new(),
            crate::rows::RowState::Missing => "  (missing)".to_string(),
            crate::rows::RowState::Corrupt(e) => format!("  (corrupt: {e})"),
        };
        let flag = if r.archived { "  [archived]" } else { "" };
        let tags = if r.tags.is_empty() { String::new() } else { format!("  [{}]", r.tags.join(" ")) };
        let status = r.status.map(|s| format!("  — {s}")).unwrap_or_default();
        println!("{}\t{}{state}{flag}{tags}{status}", r.name, r.path.display());
    }
    Ok(())
}

/// One `-limits` row.
///
/// Two things are stated that this used to leave implied. The **agent** the
/// numbers describe: limits are captured only as a side effect of rendering
/// Claude's status line, so every row is Claude's, and a reader with a Codex
/// workspace open would otherwise reasonably read them as covering both. And the
/// **age**: `stamped_at` was recorded and never read, so a week-old reading was
/// printed in exactly the same format as a live one.
fn print_limits_row(label: &str, snap: &limits::LimitsSnapshot, now: i64) {
    let agent = if snap.agent.is_empty() { "?" } else { &snap.agent };
    let freshness = match limits::age_secs(snap, now) {
        Some(age) if age > limits::STALE_AFTER_SECS => {
            format!("\tSTALE ({} old)", limits::humanize_age(age))
        }
        Some(_) => String::new(),
        None => "\tSTALE (age unknown)".to_string(),
    };
    println!(
        "{label}\t[{agent}]\t5h {}% (resets in {})\twk {}% (resets in {}){freshness}",
        snap.five_hour.used_pct.round() as i64,
        limits::countdown(snap.five_hour.resets_at, now),
        snap.seven_day.used_pct.round() as i64,
        limits::countdown(snap.seven_day.resets_at, now),
    );
}

pub fn limits() -> Result<()> {
    let now = limits::now_epoch();
    let mut shown = 0;
    let mut any_stale = false;
    for (name, path) in crate::registry::all() {
        let m = crate::meta::read(&path.join(".ws/workspace.toml"));
        if m.archived {
            continue;
        }
        let lp = path.join(".ws/local/limits.json");
        if let Some(snap) = limits::read(&lp) {
            any_stale |= limits::is_stale(&snap, now);
            print_limits_row(&name, &snap, now);
            shown += 1;
        }
    }
    if shown == 0 {
        if let Some(snap) = limits::read(&limits::global_path()) {
            any_stale |= limits::is_stale(&snap, now);
            print_limits_row("(global)", &snap, now);
        } else {
            println!("no limit data yet (run a ws session so the statusline can sense them)");
            return Ok(());
        }
    }
    if any_stale {
        println!(
            "\nSTALE means the reading predates the 5-hour window it describes, so it may have \
             reset since. Open the workspace in Claude to refresh it."
        );
    }
    // Say this once, unconditionally, rather than letting an all-Claude table
    // imply coverage it does not have. There is nowhere to read Codex usage from:
    // Codex renders its own limits natively in its status bar and exposes them to
    // no hook, so ws cannot capture them at all — see
    // docs/2026-07-27-codex-hook-contract-verified.md.
    println!(
        "note: these are Claude's limits only. Codex shows its own in its status bar; \
         it exposes them to no hook, so ws cannot record them."
    );
    Ok(())
}

/// What went wrong in `remove_one`, distinguished so callers never report a
/// successful removal as a failure. `Delete` means nothing changed — the
/// directory (or `.ws/`) is still there. `Unregister` means the removal
/// itself *succeeded* and only the registry write afterward failed, leaving
/// a stale (but harmless — `lookup()` still resolves it) registry entry.
#[derive(Debug)]
pub enum RemoveError {
    Delete(anyhow::Error),
    Unregister(anyhow::Error),
    /// A live process holds the workspace lock. Deleting it out from under a
    /// running agent loses whatever that session has not written yet.
    Live(u32),
    /// The lock file exists but could not be read (permission error, I/O
    /// error) — so whether a live process holds it could not be determined.
    /// Unreadable is not proof of absence: refuse rather than guess and risk
    /// deleting a live workspace out from under it. `--force` still overrides.
    LockUnreadable(anyhow::Error),
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoveError::Delete(e) => write!(f, "{e}"),
            RemoveError::Unregister(e) => {
                write!(f, "removed, but could not clear the registry entry: {e}")
            }
            RemoveError::Live(pid) => write!(
                f,
                "workspace is in use by pid {pid} (another terminal). Close it, or re-run with --force"
            ),
            RemoveError::LockUnreadable(e) => write!(
                f,
                "could not tell whether this workspace is in use (lock file unreadable: {e:#}). \
                 Close any process that might be using it, or re-run with --force"
            ),
        }
    }
}

impl std::error::Error for RemoveError {}

/// Is `path_c` (already canonicalized) a workspace ws itself created — i.e. a
/// *direct child* of the sessions root — and therefore safe to delete whole?
///
/// C3: this is the only guard standing between `-rm` and `remove_dir_all` on a
/// user-supplied path, and the old form (`path_c.starts_with(&root_c)` over a
/// `canonicalize().unwrap_or(root)` fallback) failed open in three ways:
///
/// * `Path::starts_with("")` is **true for every path**, and `sessions_root`
///   is unvalidated — `ws config set sessions_root ""` made every adopted
///   project anywhere on disk take the delete-the-whole-directory branch.
/// * `path_c == root_c` satisfies `starts_with`, so a workspace registered
///   *at* the root deleted the entire root.
/// * A relative root would match relative paths by accident.
///
/// Everything that is not provably a direct child of a real, absolute,
/// non-filesystem-root directory falls to the conservative `.ws`-only branch.
fn is_managed_workspace(root: &std::path::Path, path_c: &std::path::Path) -> bool {
    // An unreadable/nonexistent root cannot be proven to contain anything.
    let Ok(root_c) = root.canonicalize() else { return false };
    // Reject "" (canonicalize already errors on it, but be explicit), any
    // relative root, and the filesystem root itself — under `/` every
    // absolute path is a descendant.
    if !root_c.is_absolute() || root_c.parent().is_none() {
        return false;
    }
    // Exactly one component below the root: `<root>/<name>`, nothing deeper
    // and never the root itself.
    path_c.strip_prefix(&root_c).is_ok_and(|rest| rest.components().count() == 1)
}

/// Does removing `path` delete the whole workspace directory (true) or only
/// its `.ws/` (false)? This is the exact predicate `remove_one` acts on —
/// exported so the TUI's confirm dialog can state the same thing it does,
/// rather than re-deriving (and risking disagreeing with) the root
/// comparison that C3 made deliberately fail closed.
pub fn deletes_whole_directory(path: &std::path::Path) -> bool {
    let cfg = config::load();
    let root = config::sessions_root(&cfg);
    // Canonicalize before comparing: on macOS temp dirs live under a symlink
    // (/var -> /private/var), so a literal prefix check can mismatch even when
    // one path is truly nested under the other.
    let path_c = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    is_managed_workspace(&root, &path_c)
}

/// Remove one workspace with no prompting: the whole directory when it lives
/// under the workspaces root, otherwise just its `.ws/` (an adopted project
/// keeps its source), then drop the registry entry. Refuses (unless `force`)
/// when a live process holds the workspace's lock — `lock::acquire` already
/// refuses a live workspace for launch, and deleting it out from under a
/// running agent loses whatever that session has not written yet.
pub fn remove_one(name: &str, path: &std::path::Path, force: bool) -> std::result::Result<(), RemoveError> {
    if !force {
        match crate::lock::live_pid_checked(&path.join(".ws/local/lock")) {
            Ok(Some(pid)) => return Err(RemoveError::Live(pid)),
            Ok(None) => {}
            Err(e) => return Err(RemoveError::LockUnreadable(e)),
        }
    }
    let result = if deletes_whole_directory(path) {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_dir_all(path.join(".ws"))
    };
    if let Err(e) = result {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(RemoveError::Delete(
                anyhow::Error::new(e).context(format!("failed to remove {name}")),
            ));
        }
        // NotFound → already gone; fall through and unregister the stale entry.
    }
    crate::registry::unregister(name).map_err(RemoveError::Unregister)
}

pub fn rm(names: Vec<String>, force: bool) -> Result<()> {
    for name in names {
        let path = match crate::registry::lookup(&name) {
            Some(p) => p,
            None => {
                eprintln!("ws: no such workspace: {name}");
                continue;
            }
        };
        if !force {
            if !std::io::stdin().is_terminal() {
                anyhow::bail!("refusing to remove {name} without --force (no TTY to confirm)");
            }
            eprint!("Remove workspace {name} at {}? [y/N] ", path.display());
            use std::io::Write;
            std::io::stderr().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if !matches!(line.trim(), "y" | "Y" | "yes") {
                println!("skipped {name}");
                continue;
            }
        }
        match remove_one(&name, &path, force) {
            Ok(()) => println!("removed {name}"),
            Err(RemoveError::Delete(e)) => {
                eprintln!("ws: failed to remove {name}: {e}");
                continue; // keep the registry entry — it still exists on disk
            }
            Err(RemoveError::Live(pid)) => {
                // A live workspace is a per-name condition, not a reason to
                // abandon the rest of the batch.
                eprintln!("ws: {name}: workspace is in use by pid {pid} (another terminal). Close it, or re-run with --force");
                continue;
            }
            Err(RemoveError::LockUnreadable(e)) => {
                // Same per-name reasoning as Live: could not prove this one
                // is safe, but that says nothing about the rest of the batch.
                eprintln!("ws: {name}: {e}");
                continue;
            }
            Err(RemoveError::Unregister(e)) => {
                // The workspace itself is gone; only the registry write
                // failed. This restores the pre-extraction behavior of
                // `unregister(&name)?`: abort the whole batch rather than
                // silently continuing with a stale registry entry.
                return Err(e).with_context(|| {
                    format!("{name} was removed, but its registry entry could not be cleared")
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod remove_tests {
    use std::sync::Mutex;
    use tempfile::TempDir;

    // remove_one() resolves config through process-global XDG_CONFIG_HOME /
    // WS_ROOT and deletes real directories — serialize explicitly rather than
    // leaning on the RUST_TEST_THREADS pin (see registry.rs, rows.rs).
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn remove_one_deletes_only_ws_for_an_adopted_project() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::fs::write(project.join("keep-me.txt"), "source code").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        super::remove_one("myproj", &project, false).unwrap();

        assert!(!project.join(".ws").exists(), ".ws is gone");
        assert!(project.join("keep-me.txt").exists(), "an adopted project itself must survive");
        assert!(crate::registry::lookup("myproj").is_none(), "and the registry entry is cleared");
    }

    /// M2. `config::load` is lenient — a corrupt or unreadable `config.toml`
    /// silently becomes `Config::default()` — and its `sessions_root` is what
    /// `deletes_whole_directory` compares against. `load`'s doc comment
    /// asserts that this fails *closed*, i.e. that a substituted default can
    /// only ever narrow a whole-directory delete to a `.ws`-only one and
    /// never the reverse. That claim gates an irreversible operation, so pin
    /// it rather than leaving it as prose.
    #[test]
    fn a_corrupt_config_narrows_the_deletion_scope_and_never_widens_it() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::remove_var("WS_ROOT"); // the config value is what's under test

        let root = d.path().join("roots");
        let managed = root.join("alpha");
        std::fs::create_dir_all(&managed).unwrap();
        let cfg_path = crate::config::config_path();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();

        // With a valid config naming this root, `alpha` is a direct child and
        // the whole directory goes.
        std::fs::write(&cfg_path, format!("sessions_root = {:?}\n", root.display().to_string())).unwrap();
        assert!(
            super::deletes_whole_directory(&managed),
            "a correctly configured root must still delete the whole directory"
        );

        // Corrupt the same config. `load` substitutes defaults, so the root
        // becomes ~/.agent-workspaces, `alpha` stops looking like a direct
        // child of it, and the narrow branch wins.
        std::fs::write(&cfg_path, "sessions_root = not valid toml ][").unwrap();
        assert!(
            !super::deletes_whole_directory(&managed),
            "a corrupt config must fail closed: narrow to .ws/, never widen to the whole directory"
        );
    }

    /// C3 regression. An empty `sessions_root` made `Path::starts_with`
    /// return true for every path, so `-rm` deleted an adopted project's
    /// entire source tree. Only `.ws` may go.
    #[test]
    fn an_empty_sessions_root_does_not_delete_an_adopted_projects_source() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::remove_var("WS_ROOT"); // so the config value is what's used
        // `config set` now rejects "", but a hand-edited config.toml can still
        // carry one — write it directly so the guard itself is what's tested.
        let cfg_path = crate::config::config_path();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "sessions_root = \"\"\n").unwrap();
        assert_eq!(crate::config::sessions_root(&crate::config::load()).as_os_str(), "");

        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::fs::write(project.join("src.rs"), "fn main() {}").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        super::remove_one("myproj", &project, false).unwrap();

        assert!(project.join("src.rs").exists(), "the project's source must survive");
        assert!(!project.join(".ws").exists(), "only .ws goes");
    }

    /// A workspace registered *at* the sessions root must never take the
    /// delete-the-whole-directory branch — `path == root` also satisfied
    /// `starts_with`, which deleted the entire root.
    #[test]
    fn a_workspace_at_the_sessions_root_itself_is_never_deleted_whole() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join(".ws")).unwrap();
        std::fs::write(root.join("other-stuff.txt"), "not ours").unwrap();
        std::env::set_var("WS_ROOT", &root);
        crate::registry::register("root", &root).unwrap();

        super::remove_one("root", &root, false).unwrap();

        assert!(root.exists(), "the sessions root itself must survive");
        assert!(root.join("other-stuff.txt").exists(), "and everything in it");
        assert!(!root.join(".ws").exists());
    }

    /// The counterpart: a genuine `<root>/<name>` workspace still goes whole.
    #[test]
    fn a_direct_child_of_the_sessions_root_is_still_removed_whole() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        let root = d.path().join("root");
        let project = root.join("mine");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        std::env::set_var("WS_ROOT", &root);
        crate::registry::register("mine", &project).unwrap();

        super::remove_one("mine", &project, false).unwrap();

        assert!(!project.exists(), "a ws-created workspace is removed whole");
        assert!(root.exists(), "but not its root");
    }

    #[test]
    fn remove_one_reports_a_deletion_failure_as_delete_not_unregister() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(&project).unwrap();
        // `.ws` exists as a *file*, not a directory: `remove_dir_all` on it
        // fails with a real error (not NotFound) — this must surface as
        // Delete, not silently fall through to unregister and get reported
        // as if the registry write were the problem.
        std::fs::write(project.join(".ws"), "not a directory").unwrap();
        crate::registry::register("myproj", &project).unwrap();

        let err = super::remove_one("myproj", &project, false).unwrap_err();

        assert!(
            matches!(err, super::RemoveError::Delete(_)),
            "a real deletion failure must not be reported as a registry failure: {err:?}"
        );
        assert!(project.join(".ws").exists(), "nothing was removed");
        assert!(crate::registry::lookup("myproj").is_some(), "registry entry untouched on a failed delete");
    }

    #[test]
    fn remove_one_refuses_a_workspace_a_live_process_holds() {
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let ws = d.path().join("busy");
        std::fs::create_dir_all(ws.join(".ws/local")).unwrap();
        crate::registry::register("busy", &ws).unwrap();
        // Our own pid is by definition alive.
        let me = std::process::id();
        std::fs::write(
            ws.join(".ws/local/lock"),
            format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n"),
        )
        .unwrap();

        let err = super::remove_one("busy", &ws, false).unwrap_err();
        assert!(matches!(err, super::RemoveError::Live(p) if p == me), "got {err:?}");
        assert!(ws.join(".ws").exists(), "nothing was deleted");
        assert!(crate::registry::lookup("busy").is_some(), "and it is still registered");

        // force overrides, the same escape hatch launch has.
        super::remove_one("busy", &ws, true).unwrap();
        assert!(!ws.join(".ws").exists());
    }

    #[test]
    #[cfg(unix)]
    fn remove_one_refuses_a_workspace_whose_lock_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string()).unwrap_or_default();
        if uid == "0" { return; }

        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path());
        let ws = d.path().join("busy");
        std::fs::create_dir_all(ws.join(".ws/local")).unwrap();
        crate::registry::register("busy", &ws).unwrap();
        let me = std::process::id();
        let lock_path = ws.join(".ws/local/lock");
        std::fs::write(&lock_path, format!("pid = {me}\nhost = \"x\"\ntty = \"?\"\nstarted = \"t\"\n")).unwrap();

        // Write-only, no read: a live pid is recorded (this very process),
        // but `remove_one` cannot read it to find out. Pre-fix, `live_pid`
        // folded that read failure into "not live" and proceeded to delete
        // the whole (live) workspace directory.
        let mut perms = std::fs::metadata(&lock_path).unwrap().permissions();
        perms.set_mode(0o200);
        std::fs::set_permissions(&lock_path, perms).unwrap();

        let err = super::remove_one("busy", &ws, false).unwrap_err();

        // Restore permissions before asserting so TempDir teardown works.
        let mut perms = std::fs::metadata(&lock_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&lock_path, perms).unwrap();

        assert!(
            matches!(err, super::RemoveError::LockUnreadable(_)),
            "an unreadable lock must be its own error, not silently treated as 'not live': {err:?}"
        );
        assert!(ws.join(".ws").exists(), "the workspace directory must survive — nothing was proven safe to delete");
        assert!(crate::registry::lookup("busy").is_some(), "and it is still registered");

        // force overrides, the same escape hatch launch has.
        super::remove_one("busy", &ws, true).unwrap();
        assert!(!ws.join(".ws").exists());
    }

    #[test]
    #[cfg(unix)]
    fn remove_one_reports_unregister_failure_after_a_successful_delete() {
        use std::os::unix::fs::PermissionsExt;
        // Running as root defeats file permissions — the read would succeed.
        let uid = std::process::Command::new("id").arg("-u").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if uid == "0" {
            return;
        }
        let _g = lock_env();
        let d = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", d.path().join(".config"));
        std::env::set_var("WS_ROOT", d.path().join("root"));
        let project = d.path().join("elsewhere/myproj");
        std::fs::create_dir_all(project.join(".ws")).unwrap();
        crate::registry::register("myproj", &project).unwrap();

        let registry_path = crate::registry::registry_path();
        let mut perms = std::fs::metadata(&registry_path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&registry_path, perms).unwrap();

        let err = super::remove_one("myproj", &project, false).unwrap_err();

        // Restore permissions before any assertion (and before TempDir
        // teardown) so cleanup doesn't itself fail.
        let mut perms = std::fs::metadata(&registry_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&registry_path, perms).unwrap();

        assert!(
            matches!(err, super::RemoveError::Unregister(_)),
            "a registry write failure after a successful delete must not be reported as a deletion failure: {err:?}"
        );
        assert!(!project.join(".ws").exists(), "the workspace itself was actually removed");
    }
}

pub fn config(cmd: ConfigCmd) -> Result<()> {
    let cfg = config::load();
    match cmd {
        ConfigCmd::List => {
            for (k, v) in config::list(&cfg) {
                println!("{k} = {v}");
            }
        }
        ConfigCmd::Get(key) => {
            println!("{}", config::get(&cfg, &key)?);
        }
        ConfigCmd::Set { key, value } => {
            config::set(&key, &value)?;
        }
    }
    Ok(())
}

pub fn launch(
    name: String,
    agent_override: Option<String>,
    fresh: bool,
    force: bool,
    handoff: bool,
) -> Result<()> {
    let cfg = config::load();

    // 1. Resolve agent id: --agent > workspace default > config default.
    //    Read the recorded default BEFORE open_or_create so a brand-new workspace
    //    (no recorded default yet) is never mistaken for an agent switch — the switch
    //    invariant must not depend on what contract::init happens to write.
    let recorded_default =
        crate::meta::read(&workspace::resolve(&name, &cfg).workspace_toml()).default_agent;
    let agent_id = agent_override
        .clone()
        .or_else(|| recorded_default.clone())
        .unwrap_or_else(|| cfg.default_agent.clone());
    let agent = agents::for_id(&agent_id)?;
    if !agent.is_installed() {
        anyhow::bail!(
            "{} is not installed or not on PATH (looked for `{}`). Install it, or set WS_CLAUDE_BIN.",
            agent.id(),
            agent.binary()
        );
    }

    // Whether this launch switches the workspace to a different agent than its
    // recorded default. None (first launch) means "not switching".
    let switching = recorded_default.as_deref().is_some_and(|d| d != agent.id());

    // 2. Lock, *then* create.
    //
    // These used to be the other way round, so two simultaneous `ws newproj` both
    // ran `contract::init` and both attempted the convenience commit in the same
    // repository. Taking the lock first makes creation single-writer.
    //
    // Acquiring the lock creates `.ws/local/`, so a refused creation (a corrupt
    // registry, an invalid name) would otherwise leave that skeleton behind in a
    // directory that never became a workspace. The guard removes the lock file on
    // drop; the empty directories are cleaned up here.
    let ws_path = workspace::resolve(&name, &cfg);

    // A workspace another terminal holds is a fork in the road, not a dead end.
    // Offer the ways forward before `lock::acquire` turns it into an error —
    // and only when there is someone at a terminal to answer.
    let mut force = force;
    if crate::collision::should_offer(force) {
        if let Ok(Some(pid)) = lock::live_pid_checked(&ws_path.lock_file()) {
            let all: Vec<String> = registry::all().into_iter().map(|(n, _)| n).collect();
            match crate::collision::prompt(&name, pid, &all)? {
                crate::collision::Choice::Force => force = true,
                // Recursion, not re-exec: no lock is held yet, and re-entering
                // `launch` reuses every step below rather than duplicating it.
                // The callee execs into the agent, so this frame never returns.
                crate::collision::Choice::Open(other) => {
                    return launch(other, agent_override, fresh, false, handoff)
                }
                crate::collision::Choice::New => {
                    let Some(feature) = crate::collision::ask_feature_name()? else {
                        return Ok(());
                    };
                    let spec = crate::worktree::parse_name(&format!("{name}@{feature}"))
                        .ok_or_else(|| anyhow::anyhow!("not a valid feature name: {feature}"))?;
                    let path = crate::worktree::create(&spec)?;
                    println!("created {} at {}", spec.workspace_name(), path.display());
                    return launch(spec.workspace_name(), agent_override, fresh, false, handoff);
                }
                crate::collision::Choice::Cancel => return Ok(()),
            }
        }
    }

    let guard = lock::acquire(&ws_path.lock_file(), force)?;
    let (ws, _created) = match workspace::open_or_create(&name, agent.id(), &cfg) {
        Ok(v) => v,
        Err(e) => {
            drop(guard);
            for dir in [ws_path.local_dir(), ws_path.ws_dir()] {
                // `remove_dir`, never `remove_dir_all`: only a directory that is
                // empty because we just made it may go. Anything else is the
                // user's.
                let _ = std::fs::remove_dir(&dir);
            }
            return Err(e);
        }
    };

    // 4. Regenerate context file, seeding a handoff pointer when requested or switching.
    let hint = if handoff || switching {
        crate::handoff::latest_handoff(&ws)
    } else {
        None
    };
    context::regenerate_with_handoff(&ws.root.join(agent.context_file()), &ws.name, hint.as_deref())?;

    if switching {
        let _ = std::fs::remove_file(ws.limit_guard());
        crate::meta::set_default_agent(&ws.workspace_toml(), agent.id())?;
        // Record `from` and the handoff actually seeded, not just `to`. A switch
        // event naming only its destination cannot be read as a chain — and the
        // handoff is the one piece of context that crossed, so it belongs in the
        // history rather than only in the context file that gets regenerated.
        let _ = crate::timeline::record(
            &ws.timeline(),
            "agent-switch",
            &crate::actors::actor_slug(),
            serde_json::json!({
                "from": recorded_default.as_deref().unwrap_or("?"),
                "to": agent.id(),
                "handoff": hint.as_deref(),
            }),
        );
    }

    // 5. Tab title + color. Workspaces created before colors existed have no
    // `color` key; give them one on first launch rather than leaving them the
    // only uncolored tabs. It is persisted, so the backfill happens once and the
    // workspace keeps that color from then on. A failed write is not worth
    // aborting a launch over — the color is cosmetic, and the next launch retries.
    let mut color = crate::meta::read(&ws.workspace_toml()).color;
    if color.is_none() {
        let picked = term::alloc_color();
        if crate::meta::set_color(&ws.workspace_toml(), Some(picked)).is_ok() {
            color = Some(picked.to_string());
        }
    }
    term::set_tab(&ws.name, color.as_deref());

    // 5b. Tell the user a newer ws exists, before the agent takes the terminal.
    // Cached for an hour and silent on every failure — see `update::notify`.
    crate::update::notify();

    // 6. Ask before resuming, when there is something to resume and someone to
    // ask. `y` resumes; the default (Enter) starts a fresh conversation. When
    // there is nobody to ask, resuming stays the unprompted behavior — a
    // scripted launch must not silently start over.
    let has_prior = crate::contract::read_session_id(&ws.state_toml(), agent.id()).is_some();
    let fresh = fresh
        || (should_ask_new(has_prior, fresh, cfg.resume_prompt, std::io::stdin().is_terminal())
            && !ask_resume(&ws.name));

    // 7. Build + run — the agent owns the fresh/resume decision and persists its own state.
    let ctx = LaunchCtx {
        fresh,
        sessions_root: config::sessions_root(&cfg),
    };
    let cmd = agent.launch(&ws, &ctx)?;

    // Keep the lock file in place; the launched agent inherits our PID.
    guard.keep();

    if std::env::var_os("WS_NO_EXEC").is_some() {
        let mut cmd = cmd;
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }

    exec(cmd)
}

#[cfg(unix)]
fn exec(mut cmd: std::process::Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Err(cmd.exec().into()) // exec only returns on failure
}

#[cfg(not(unix))]
fn exec(mut cmd: std::process::Command) -> Result<()> {
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(0));
}

/// Aggregate the timeline into "who did what".
///
/// `-who` used to rank actors by `git log --format=%ae -- .ws`, which answers a
/// different question: who *committed* metadata. That misses everyone whose work
/// was never committed, and it cannot say what anybody actually did. The timeline
/// records an actor on every event, so it can — and the git ranking stays as the
/// fallback for a workspace with no timeline yet.
pub fn who(name: Option<String>) -> Result<()> {
    let (_name, root) = current_or_named(name)?;
    let summaries = crate::timeline::by_actor(&root.join(".ws/timeline.jsonl"))?;

    if summaries.is_empty() {
        // No timeline yet: fall back to the commit ranking rather than claiming
        // nobody has worked here. The fallback *degrades* — a repo with no commits
        // yet, or no git at all, means there is simply nothing to report, and
        // surfacing git's complaint would be answering a question the user did not
        // ask. An unreadable timeline is different, and `by_actor` above already
        // refuses for that.
        let ranked = crate::actors::who(&root.join(".ws")).unwrap_or_default();
        if ranked.is_empty() {
            println!("no recorded activity yet");
            return Ok(());
        }
        println!("(no timeline yet — ranking by commits to .ws/)");
        for (actor, n) in ranked {
            println!("{actor}  {n}");
        }
        return Ok(());
    }

    for a in summaries {
        println!("{:<28} {:>4} event(s)  {} … {}", a.actor, a.events, a.first, a.last);
        if !a.kinds.is_empty() {
            println!("{:28} {}", "", a.kinds.join(", "));
        }
    }
    Ok(())
}

/// Write a handoff skeleton for whoever picks this workspace up next.
///
/// The `/ws:rotate` prompt already asks the agent to do this; having it as a
/// command means a human can too, and that the file's shape is one thing rather
/// than whatever the model felt like writing.
pub fn rotate(name: Option<String>) -> Result<()> {
    let (n, root) = current_or_named(name)?;
    contract::check_gate(&n, &root.join(".ws/workspace.toml"))?;

    let ws = crate::workspace::Workspace { name: n.clone(), root: root.clone() };
    let actor = crate::actors::actor_slug_in(&root);
    let ts = crate::now_iso();
    let cfg = config::load();
    let agent = crate::meta::read(&ws.workspace_toml())
        .default_agent
        .unwrap_or(cfg.default_agent);

    let objective = std::fs::read_to_string(ws.readme())
        .ok()
        .and_then(|r| crate::readme::objective_of(&r))
        .unwrap_or_else(|| "(none recorded)".to_string());
    let session = contract::read_session_id(&ws.state_toml(), &agent)
        .unwrap_or_else(|| "(none recorded)".to_string());

    let dir = ws.ws_dir().join("handoffs");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    // Colons are legal in a filename but awkward in a shell; the timestamp is
    // flattened rather than reformatted so it still sorts lexically.
    let stamp = ts.replace(':', "");
    let path = dir.join(format!("{stamp}-{actor}.md"));

    let body = format!(
        "# Handoff — {n}\n\n         - **Written:** {ts}\n         - **By:** {actor}\n         - **Agent:** {agent}\n         - **Session:** {session}\n         - **Objective:** {objective}\n\n         ## What is done\n\n         <!-- what actually landed, not what was attempted -->\n\n         ## What is next\n\n         <!-- the single next action, and anything that would block it -->\n\n         ## Watch out for\n\n         <!-- anything that would mislead someone reading only the code -->\n"
    );
    crate::atomic::atomic_write(&path, body)?;
    let _ = crate::timeline::record(
        &ws.timeline(),
        "handoff-written",
        &actor,
        serde_json::json!({ "file": path.file_name().and_then(|f| f.to_str()) }),
    );
    println!("wrote {}", path.display());
    println!("`ws {n} --handoff` will point the next session at it.");
    Ok(())
}

/// Show or validate the hook registration, built-in and user-defined.
pub fn hooks(cmd: crate::cli::HooksCmd) -> Result<()> {
    use crate::cli::HooksCmd;
    let path = crate::hooks_user::hooks_toml_path();
    let user = crate::hooks_user::load()?;

    match cmd {
        HooksCmd::Check => {
            // Deliberately writes nothing: the point is to review a hooks.toml
            // before `ws setup` acts on it, since every entry runs a command in
            // the agent's context.
            if user.is_empty() {
                println!("{} — no user hooks", path.display());
            } else {
                println!("{} — {} user hook(s), all valid:", path.display(), user.len());
            }
            for agent_id in ["claude", "codex"] {
                let agent = crate::agents::for_id(agent_id)?;
                let (applies, skipped) = crate::hooks_user::for_agent(&user, agent.as_ref());
                for h in applies {
                    let matcher = match h.scope {
                        crate::hooksetup::Scope::Always => "(every tool)".to_string(),
                        crate::hooksetup::Scope::Tool(k) => agent.tool_matcher(k).to_string(),
                    };
                    println!(
                        "  would register  {agent_id:<7} {:<20} {matcher:<32} {} ({}s)",
                        h.event,
                        h.command.display(),
                        h.timeout
                    );
                }
                for (h, _) in skipped {
                    println!(
                        "  would SKIP      {agent_id:<7} {:<20} — {agent_id} has no such event",
                        h.event
                    );
                }
            }
            println!("\nNothing was written. Run `ws setup` to apply.");
            Ok(())
        }
        HooksCmd::List => {
            for agent_id in ["claude", "codex"] {
                let agent = crate::agents::for_id(agent_id)?;
                println!("{agent_id} ({})", agent.hooks_config_path().display());
                for spec in crate::hooksetup::HOOKS {
                    let matcher = match spec.scope {
                        crate::hooksetup::Scope::Always => "(every tool)".to_string(),
                        crate::hooksetup::Scope::Tool(k) => agent.tool_matcher(k).to_string(),
                    };
                    println!("  built-in  {:<20} {matcher:<32} {}", spec.event, spec.handler);
                }
                let (applies, skipped) = crate::hooks_user::for_agent(&user, agent.as_ref());
                for h in applies {
                    let matcher = match h.scope {
                        crate::hooksetup::Scope::Always => "(every tool)".to_string(),
                        crate::hooksetup::Scope::Tool(k) => agent.tool_matcher(k).to_string(),
                    };
                    println!(
                        "  user      {:<20} {matcher:<32} {}",
                        h.event,
                        h.command.display()
                    );
                }
                for (h, _) in skipped {
                    println!("  user      {:<20} skipped — no such event on {agent_id}", h.event);
                }
                println!();
            }
            println!("user hooks: {}", path.display());
            Ok(())
        }
    }
}

pub fn whoami() -> Result<()> {
    let dir = std::env::current_dir()?;
    println!("{}", crate::actors::actor_slug_in(&dir));
    Ok(())
}


#[cfg(test)]
mod resume_prompt_tests {
    use super::{answer_resumes, should_ask_new};

    /// The prompt reads `[y/N]`, so only an explicit yes resumes: Enter, an
    /// unrecognised key and a stray blank line all start a fresh conversation.
    #[test]
    fn only_an_explicit_yes_resumes() {
        for yes in ["y\n", "Y\n", "yes\n", " y \n"] {
            assert!(answer_resumes(yes), "{yes:?} should resume");
        }
        for no in ["\n", "n\n", "N\n", "no\n", "q\n", ""] {
            assert!(!answer_resumes(no), "{no:?} should start fresh");
        }
    }

    /// The prompt only appears when all four conditions hold. Each of the four
    /// is a case where asking is wrong, not merely noisy — most importantly the
    /// no-TTY one, which would otherwise block a scripted `ws` on a read that
    /// never returns.
    #[test]
    fn the_prompt_needs_a_session_a_question_a_setting_and_a_terminal() {
        assert!(should_ask_new(true, false, true, true), "the one case that asks");

        assert!(!should_ask_new(false, false, true, true), "nothing recorded to resume");
        assert!(!should_ask_new(true, true, true, true), "--fresh already answered it");
        assert!(!should_ask_new(true, false, false, true), "resume_prompt = false");
        assert!(!should_ask_new(true, false, true, false), "no TTY: never block a script");
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod who_tests {
    use std::sync::Mutex;
    use tempfile::TempDir;

    // who() resolves through current_or_named(), which reads the process-global
    // WS_WORKSPACE env var, XDG_CONFIG_HOME-scoped registry, and current_dir.
    // Serialize explicitly rather than leaning on the RUST_TEST_THREADS pin
    // (see registry.rs, rows.rs, remove_tests above): this module changes the
    // process cwd, which no other module here does.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    // Discriminates the Important review finding: who(None) must resolve the
    // workspace the same way tag/status/archive do, i.e. via current_or_named
    // and therefore via $WS_WORKSPACE, not by treating the bare cwd as "the"
    // workspace. Before the fix, `who` built `Workspace { name: "here", root: cwd }`
    // directly, ignored $WS_WORKSPACE entirely, and this test failed with "not
    // in a workspace" because the cwd used here deliberately is not one.
    #[test]
    fn who_with_no_name_honours_ws_workspace_env_var() {
        let _g = lock_env();
        let config_dir = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", config_dir.path());

        // The registered workspace: a real git repo with a .ws dir, so
        // actors::who() has real history to read.
        let ws_dir = TempDir::new().unwrap();
        let root = ws_dir.path();
        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "someone@example.com"]);
        run_git(root, &["config", "user.name", "Someone"]);
        std::fs::create_dir_all(root.join(".ws")).unwrap();
        std::fs::write(root.join(".ws/marker.txt"), "x").unwrap();
        run_git(root, &["add", ".ws/marker.txt"]);
        run_git(root, &["commit", "-q", "-m", "seed"]);
        crate::registry::register("envres", root).unwrap();

        // The cwd: deliberately NOT the registered workspace and not a
        // workspace at all, so resolution can only succeed via $WS_WORKSPACE.
        let elsewhere = TempDir::new().unwrap();
        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(elsewhere.path()).unwrap();
        std::env::set_var("WS_WORKSPACE", "envres");

        let result = super::who(None);

        std::env::set_current_dir(&orig_cwd).unwrap();
        std::env::remove_var("WS_WORKSPACE");

        assert!(
            result.is_ok(),
            "who(None) must resolve via $WS_WORKSPACE like tag/status/archive do: {result:?}"
        );
    }

    #[test]
    fn who_with_no_name_and_no_env_var_in_a_non_workspace_cwd_errors() {
        let _g = lock_env();
        let config_dir = TempDir::new().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", config_dir.path());
        std::env::remove_var("WS_WORKSPACE");

        let elsewhere = TempDir::new().unwrap();
        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(elsewhere.path()).unwrap();

        let result = super::who(None);

        std::env::set_current_dir(&orig_cwd).unwrap();

        assert!(result.is_err(), "with no name and no env var, a non-workspace cwd must error, not print empty");
    }
}

pub fn search(query: String, include_archived: bool) -> Result<()> {
    let hits = crate::search::search_all(&query, include_archived)?;
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(());
    }
    // Group consecutive hits by workspace — search_all emits them workspace-by-workspace,
    // and each workspace's group may carry one extra hit past MAX_HITS_PER_WORKSPACE
    // (see search_dir) purely as a truncation signal, not an actual match to display.
    let mut groups: Vec<(&str, Vec<&crate::search::Hit>)> = Vec::new();
    for h in &hits {
        match groups.last_mut() {
            Some((name, g)) if *name == h.workspace => g.push(h),
            _ => groups.push((h.workspace.as_str(), vec![h])),
        }
    }

    let mut shown_total = 0usize;
    let mut any_truncated = false;
    for (name, g) in &groups {
        println!("\n{name}");
        let truncated = g.len() > crate::search::MAX_HITS_PER_WORKSPACE;
        let display_count = g.len().min(crate::search::MAX_HITS_PER_WORKSPACE);
        for h in g.iter().take(display_count) {
            // Show the path relative to the workspace root when we can — the absolute
            // prefix is noise once the workspace name is the heading.
            let shown = h
                .file
                .iter()
                .skip_while(|c| *c != ".ws")
                .collect::<std::path::PathBuf>();
            println!("  {}:{}: {}", shown.display(), h.line, h.text.trim());
        }
        if truncated {
            println!("  … more matches in this workspace — refine your query");
            any_truncated = true;
        }
        shown_total += display_count;
    }

    if any_truncated {
        println!("\n{shown_total} match(es) in {} workspace(s) (some results hidden)", groups.len());
    } else {
        println!("\n{shown_total} match(es) in {} workspace(s)", groups.len());
    }
    Ok(())
}

/// Capture a task without interrupting whatever the agent is doing.
///
/// This is the `/btw` shape: the point is to record something and get straight
/// back to work, so `add` never switches focus, never launches anything, and
/// defaults to the workspace you are already in.
pub fn task(cmd: crate::cli::TaskCmd) -> Result<()> {
    use crate::cli::TaskCmd;
    match cmd {
        TaskCmd::Add { name, text } => {
            let ws = resolve_named(name)?;
            // Adding is a mutating entry point; list/rm below resolve the same
            // way but only read, so they must not refuse on a newer contract.
            contract::check_gate(&ws.name, &ws.workspace_toml())?;
            let actor = crate::actors::actor_slug_in(&ws.root);
            let tasks_path = ws.queue_tasks();
            crate::queue::add(&tasks_path, &text, &actor)?;
            let pending = crate::queue::pending(&tasks_path)?.len();
            println!("noted for {} ({pending} open) — see `ws -task list`", ws.name);
            Ok(())
        }
        TaskCmd::List { name } => {
            let ws = resolve_named(name)?;
            let tasks = crate::queue::tasks(&ws.queue_tasks())?;
            if tasks.is_empty() {
                println!("no tasks");
                return Ok(());
            }
            for (i, t) in tasks.iter().enumerate() {
                let note = t.note.clone().map(|n| format!("  ({n})")).unwrap_or_default();
                println!("{:>3}  {:<8} {}{}", i + 1, t.state.as_str(), t.text, note);
            }
            Ok(())
        }
        TaskCmd::Rm { name, index } => {
            let ws = resolve_named(name)?;
            contract::check_gate(&ws.name, &ws.workspace_toml())?;
            let tasks_path = ws.queue_tasks();
            let tasks = crate::queue::tasks(&tasks_path)?;
            // 1-based, matching what `-task list` prints. An index the user
            // cannot see in the listing is a mistake worth refusing rather than
            // silently dropping the wrong task.
            let t = index
                .checked_sub(1)
                .and_then(|i| tasks.get(i))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no task {index} in {} ({} task(s) — see `ws -task list`)",
                        ws.name,
                        tasks.len()
                    )
                })?;
            crate::queue::remove(&tasks_path, &t.id)?;
            println!("dropped task {index} in {}", ws.name);
            Ok(())
        }
    }
}
