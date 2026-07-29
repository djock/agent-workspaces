//! What to offer when a workspace is already open in another terminal.
//!
//! Launching a workspace another process holds used to be a dead end: an error
//! naming the holding pid, and nothing else. Every way forward from there —
//! force a second session, split off a feature worktree, jump into a feature
//! that already exists — was a command the user had to know and retype. This
//! module turns that error into the choice it was always standing in for.
//!
//! The menu is built purely (`options`) and rendered separately (`prompt`), so
//! which entries appear for a given workspace is testable without a terminal.
use anyhow::{Context, Result};
use std::io::{IsTerminal, Write};

/// What the user picked out of the collision menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// Launch this (already existing) workspace instead.
    Open(String),
    /// Take the lock anyway; two sessions then share one checkout.
    Force,
    /// Create a new feature worktree off this workspace and open it.
    New,
    /// Do nothing.
    Cancel,
}

/// Existing feature worktrees stay single-keypress addressable, so the fixed
/// actions below them never spill past 9.
const MAX_FEATURES: usize = 6;

/// The menu for a collision on `name`, given the workspaces that already exist.
///
/// `Cancel` is always last and is always the fallback, so an unrecognised key
/// does the harmless thing. A workspace that is itself a worktree (`base@feat`)
/// offers neither `Open` nor `New`: git worktrees do not nest, and a feature of
/// a feature is not a thing ws can create.
pub fn options(name: &str, all_workspaces: &[String]) -> Vec<Choice> {
    let mut out = Vec::new();
    let nested = name.contains('@');
    if !nested {
        let prefix = format!("{name}@");
        out.extend(
            all_workspaces
                .iter()
                .filter(|w| w.starts_with(&prefix))
                .take(MAX_FEATURES)
                .map(|w| Choice::Open(w.clone())),
        );
    }
    out.push(Choice::Force);
    if !nested {
        out.push(Choice::New);
    }
    out.push(Choice::Cancel);
    out
}

/// Label and consequence for one entry, as displayed.
fn describe(choice: &Choice, base: &str) -> (String, &'static str) {
    match choice {
        Choice::Open(w) => {
            let feature = w.strip_prefix(&format!("{base}@")).unwrap_or(w);
            (format!("@{feature}"), "resume")
        }
        Choice::Force => ("force start".into(), "two sessions share one checkout"),
        Choice::New => ("new feature".into(), "its own worktree + branch"),
        Choice::Cancel => ("cancel".into(), "default"),
    }
}

/// Render the menu and read a single keypress. Falls back to `Cancel` on
/// anything unrecognised, so a stray key never forces or creates.
pub fn prompt(name: &str, pid: u32, all_workspaces: &[String]) -> Result<Choice> {
    let opts = options(name, all_workspaces);
    let mut err = std::io::stderr();

    writeln!(err)?;
    writeln!(err, "  {name} is already open · PID {pid}")?;
    writeln!(err)?;

    let mut printed_header = false;
    for (i, opt) in opts.iter().enumerate() {
        if matches!(opt, Choice::Open(_)) && !printed_header {
            writeln!(err, "    open a feature")?;
            printed_header = true;
        }
        if matches!(opt, Choice::Force) && printed_header {
            writeln!(err)?;
            writeln!(err, "    or start here")?;
        }
        let (label, consequence) = describe(opt, name);
        writeln!(err, "    {}  {label:<16}{consequence}", i + 1)?;
    }
    writeln!(err)?;
    write!(err, "    › ")?;
    err.flush()?;

    let picked = read_digit()?;
    writeln!(err)?;
    Ok(picked
        .and_then(|d| opts.get(d.checked_sub(1)? as usize).cloned())
        .unwrap_or(Choice::Cancel))
}

/// One keypress, no Enter. Raw mode is entered only for the read and restored
/// immediately — a menu that left the terminal raw would wreck the agent that
/// launches straight after it.
fn read_digit() -> Result<Option<u8>> {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    crossterm::terminal::enable_raw_mode().context("cannot switch the terminal to raw mode")?;
    let read = (|| loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent { code: KeyCode::Char(c), .. })) => {
                return Ok(c.to_digit(10).map(|d| d as u8));
            }
            // Enter and Esc take the default rather than waiting for a digit.
            Ok(Event::Key(KeyEvent { code: KeyCode::Enter | KeyCode::Esc, .. })) => {
                return Ok(None)
            }
            Ok(_) => continue, // resize, focus, paste — not an answer
            Err(e) => return Err(e),
        }
    })();
    let _ = crossterm::terminal::disable_raw_mode();
    Ok(read?)
}

/// Read a feature name for `Choice::New`. Empty input cancels.
pub fn ask_feature_name() -> Result<Option<String>> {
    let mut err = std::io::stderr();
    write!(err, "    Feature name  › ")?;
    err.flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(None);
    }
    let name = line.trim().to_string();
    Ok((!name.is_empty()).then_some(name))
}

/// Whether a collision should raise the menu rather than the old error.
///
/// Without a terminal there is nobody to answer, and the launch must keep
/// failing loudly instead of rendering a menu into a pipe and blocking on a
/// keypress that never comes.
pub fn should_offer(force: bool) -> bool {
    !force && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn features_of_this_workspace_are_offered_first() {
        let all = names(&["api", "api@retry", "api@flaky", "web", "web@login"]);
        let opts = options("api", &all);
        assert_eq!(
            opts,
            vec![
                Choice::Open("api@retry".into()),
                Choice::Open("api@flaky".into()),
                Choice::Force,
                Choice::New,
                Choice::Cancel,
            ],
            "another workspace's features are not this workspace's business"
        );
    }

    #[test]
    fn a_workspace_with_no_features_still_offers_the_fixed_actions() {
        assert_eq!(
            options("api", &names(&["api", "web@login"])),
            vec![Choice::Force, Choice::New, Choice::Cancel]
        );
    }

    /// Git worktrees do not nest, so a feature workspace can only force or quit.
    #[test]
    fn a_feature_workspace_offers_neither_features_nor_new() {
        let all = names(&["api", "api@retry", "api@retry@deeper"]);
        assert_eq!(options("api@retry", &all), vec![Choice::Force, Choice::Cancel]);
    }

    /// Every entry has to stay reachable by one keypress, so the variable-length
    /// part of the menu is capped.
    #[test]
    fn the_feature_list_is_capped_so_the_menu_stays_single_keypress() {
        let mut all = vec!["api".to_string()];
        all.extend((0..20).map(|i| format!("api@f{i}")));
        let opts = options("api", &all);
        assert_eq!(opts.len(), MAX_FEATURES + 3, "6 features + force + new + cancel");
        assert!(opts.len() <= 9, "a 10th entry would need two keys: {opts:?}");
    }

    /// An unrecognised key must land on the harmless option, and `Cancel` is how
    /// that is guaranteed — so it has to be last in every shape of the menu.
    #[test]
    fn cancel_is_always_the_last_entry() {
        for name in ["api", "api@retry"] {
            let opts = options(name, &names(&["api", "api@retry"]));
            assert_eq!(opts.last(), Some(&Choice::Cancel), "for {name}");
        }
    }

    #[test]
    fn labels_strip_the_base_so_the_column_reads_as_features() {
        let (label, _) = describe(&Choice::Open("api@retry".into()), "api");
        assert_eq!(label, "@retry");
    }

    /// A launch that cannot be answered must not render a menu.
    #[test]
    fn force_never_offers_the_menu() {
        assert!(!should_offer(true), "--force already answered the question");
    }
}
