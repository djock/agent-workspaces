mod actors;
mod agents;
mod agentstate;
mod atomic;
mod autosave;
mod cli;
mod collision;
mod commands;
mod config;
mod context;
mod contract;
mod conversations;
mod detail;
mod git;
mod handoff;
mod hookio;
mod hooks_user;
mod hooksetup;
mod internal;
mod io_read;
mod limits;
mod lock;
mod mail;
mod meta;
mod picker;
mod prompts;
mod queue;
mod readme;
mod registry;
mod rewrite;
mod rows;
mod search;
mod secrets;
mod statusline;
mod term;
mod theme;
mod time;
mod timeline;
mod txn;
mod update;
mod workspace;
mod worktree;

use cli::Cmd;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(args) {
        eprintln!("ws: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> anyhow::Result<()> {
    match cli::parse(args)? {
        Cmd::Version => println!("ws {}", env!("CARGO_PKG_VERSION")),
        Cmd::Help => print_help(),
        Cmd::VerbHelp(verb) => println!("{}", cli::verb_usage(&verb)),
        Cmd::Config(c) => commands::config(c)?,
        Cmd::List { tag, archived } => commands::list(tag, archived)?,
        Cmd::Tag(c) => commands::tag(c)?,
        Cmd::Status { name, text } => commands::status(name, text)?,
        Cmd::Color { name, color } => commands::color(name, color)?,
        Cmd::Archive { names, archived } => commands::archive(names, archived)?,
        Cmd::Adopt { name } => commands::adopt(name)?,
        Cmd::Rm { names, force } => commands::rm(names, force)?,
        Cmd::Launch { name, agent, mode, fresh, force, handoff } => {
            commands::launch(name, agent, mode, fresh, force, handoff)?
        }
        Cmd::Setup => commands::setup()?,
        Cmd::Internal(args) => internal::run(args)?,
        Cmd::Statusline => statusline::run(),
        Cmd::Limits => commands::limits()?,
        Cmd::Doctor => commands::doctor()?,
        Cmd::Whoami => commands::whoami()?,
        Cmd::Who { name } => commands::who(name)?,
        Cmd::Conversations { name } => conversations::run(name)?,
        Cmd::Rotate { name } => commands::rotate(name)?,
        Cmd::Task(c) => commands::task(c)?,
        Cmd::Msg(c) => commands::msg(c)?,
        Cmd::Features { base, porcelain } => commands::features(base, porcelain)?,
        Cmd::Hooks(c) => commands::hooks(c)?,
        Cmd::Secrets(c) => commands::secrets(c)?,
        Cmd::Search { query, include_archived } => commands::search(query, include_archived)?,
        Cmd::Update { check, force } => update::run(check, force)?,
        Cmd::Uninstall { force } => commands::uninstall(force)?,
        Cmd::Pick => match picker::run()? {
            picker::Outcome::Quit => {}
            // run() has already restored the terminal; launch execs into the
            // agent from here, replacing this process.
            picker::Outcome::Launch(name) => {
                commands::launch(name, None, None, false, false, false)?
            }
        },
        Cmd::Worktree { spec, merge } => {
            let s = worktree::parse_name(&spec)
                .ok_or_else(|| anyhow::anyhow!("not a worktree spec: {spec}"))?;
            if merge {
                worktree::merge(&s)?
            } else if registry::lookup_checked(&s.workspace_name())?.is_some() {
                // Already created: open it. The parser cannot tell "make me a
                // worktree" from "open the worktree I made" — both are
                // `ws base@feature` — and it deliberately does not consult the
                // registry. Deciding here instead left an existing worktree
                // permanently unreachable: every launch after the first hit
                // `worktree::create`'s "already exists" and stopped.
                commands::launch(s.workspace_name(), None, None, false, false, false)?
            } else {
                let path = worktree::create(&s)?;
                println!("created {} at {}", s.workspace_name(), path.display());
            }
        }
    }
    Ok(())
}

/// ISO-8601 UTC timestamp, e.g. 2026-07-24T10:43:12Z.
///
/// Delegates to `time::now_iso`. This used to fork `/bin/date` per timestamp and
/// end in `.unwrap_or_default()`, so a failed fork silently produced `""` under
/// the timeline, the queue, lock bodies and the credential manifest — and
/// `conversations::parse` sorts on that field.
pub fn now_iso() -> String {
    time::now_iso()
}

/// The full command surface.
///
/// This used to omit `-limits`, `-doctor`, `-secrets`, `setup` and **every**
/// launch flag, while the README said it was "the complete command summary".
/// `help_covers_every_command` in the tests below now fails if a command exists
/// that this text does not mention, so the two cannot drift apart again.
fn print_help() {
    println!("{}", cli::help_text());
}

#[cfg(test)]
mod tests {
    use crate::cli::{self, Cmd};
    /// Every command token the parser accepts, read out of `cli.rs` itself.
    ///
    /// The list this replaces was written by hand, which meant it could only
    /// confirm that commands *someone had remembered* were documented — the one
    /// job it could not do was notice a new one. `ws -secrets help` was added,
    /// left out of the help text, and the test stayed green, while the README
    /// promised "a test fails if a command exists that the help text omits".
    /// Reading the match arms makes that promise true: a new arm is covered the
    /// moment it is written, with nothing to remember to update.
    ///
    /// `include_str!` is resolved at compile time relative to this file, so this
    /// reads the same source that produced the parser it is checking.
    fn parser_tokens() -> Vec<String> {
        let src = include_str!("cli.rs");
        let mut out = Vec::new();
        // Each dispatch is `match <expr> { "a" | "b" => ..., }`. Scope the
        // search to the function that owns it: `match sub.as_str()` appears
        // twice, once for `ws hooks` inside `parse` and once for `-secrets`.
        for (func, start) in [
            ("pub fn parse(", "match first.as_str() {"),
            ("fn parse_secrets(", "match sub.as_str() {"),
        ] {
            let Some(f) = src.find(func) else {
                panic!("{func:?} is gone from cli.rs — this test can no longer see the parser");
            };
            let Some(i) = src[f..].find(start).map(|i| f + i) else {
                panic!("{start:?} is gone from {func:?} — this test can no longer see the parser");
            };
            for line in src[i + start.len()..].lines() {
                // Arms sit at 8 spaces; the match closes at its own indent, so a
                // line starting with exactly four spaces and `}` ends it.
                if line.starts_with("    }") {
                    break;
                }
                let Some((pat, _)) = line.split_once("=>") else { continue };
                // Only pattern positions, and only quoted literals in them.
                if !pat.trim_start().starts_with('"') {
                    continue;
                }
                for lit in pat.split('|') {
                    let lit = lit.trim();
                    if let Some(t) = lit.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                        if !t.is_empty() {
                            out.push(t.to_string());
                        }
                    }
                }
            }
        }
        assert!(out.len() > 20, "only found {} tokens in cli.rs — extraction broke", out.len());
        out
    }

    /// Just the top-level verbs — the arms of `match first.as_str()` itself.
    ///
    /// `parser_tokens` deliberately reads every quoted arm it can see, which
    /// also picks up the nested matches inside a verb (`ws hooks list`, the
    /// `--check`/`--force` loops). Those are subcommands and flags: they are
    /// reached *through* a verb and have no top-level spelling, so `ws list
    /// --help` is not a thing anyone can ask for. Told apart by indentation,
    /// since the outer arms sit at eight spaces and everything nested is deeper.
    fn top_level_verbs() -> Vec<String> {
        let src = include_str!("cli.rs");
        let f = src.find("pub fn parse(").expect("cli.rs no longer has parse()");
        let start = "match first.as_str() {";
        let i = src[f..].find(start).map(|i| f + i).expect("parse() no longer has its dispatch");
        let mut out = Vec::new();
        for line in src[i + start.len()..].lines() {
            if line.starts_with("    }") {
                break;
            }
            if !line.starts_with("        \"") {
                continue;
            }
            let Some((pat, _)) = line.split_once("=>") else { continue };
            for lit in pat.split('|') {
                if let Some(t) = lit
                    .trim()
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .filter(|t| !t.is_empty())
                {
                    out.push(t.to_string());
                }
            }
        }
        assert!(out.len() > 15, "only found {} verbs — extraction broke", out.len());
        out
    }

    /// The help text is the only user-facing documentation of the command
    /// surface, and it silently fell a third of that surface behind while the
    /// README called it complete. Anything a user can type must appear in it; a
    /// new command that forgets to is a test failure, not a docs bug someone
    /// notices two releases later.
    #[test]
    fn help_covers_every_command() {
        let help = crate::cli::help_text();
        // Conventional aliases carried for muscle memory. Spelling every one of
        // them out would pad the help with rows that teach nothing, so they are
        // exempt *by name* — an exemption has to be added deliberately, which is
        // the property the hand-written list lacked.
        let aliases = ["-V", "-h", "--help", "--resume", "-resume", "internal", "statusline"];
        for token in parser_tokens() {
            if aliases.contains(&token.as_str()) {
                continue;
            }
            assert!(help.contains(&token), "`ws --help` never mentions {token:?}");
        }
    }

    /// The flags are not match arms on the dispatch, so they are still listed by
    /// hand — but a flag is only reachable through a command the test above
    /// already covers, so the exposure is much smaller.
    #[test]
    fn help_covers_every_launch_flag() {
        let help = crate::cli::help_text();
        for token in [
            "-claude",
            "-codex",
            "--agent",
            "--fresh",
            "--handoff",
            "--force",
            "--merge",
            "--include-archived",
            "--archived",
            "--tag",
            "--clear",
            "--check",
        ] {
            assert!(help.contains(token), "`ws --help` never mentions {token:?}");
        }
    }

    /// Every verb answers `-h`/`--help` with its own usage, derived from the
    /// same help text, and none of them treats the flag as data.
    ///
    /// Read from the dispatch rather than a list, so a verb added tomorrow is
    /// covered the moment it is written. That is the property the hand-written
    /// version of `parser_tokens`'s ancestor lacked: it could confirm the verbs
    /// somebody remembered and could not notice a new one.
    #[test]
    fn every_verb_answers_help_with_its_own_usage() {
        let aliases = ["-V", "-h", "--help", "--resume", "-resume", "internal", "statusline"];
        for token in top_level_verbs() {
            if aliases.contains(&token.as_str()) {
                continue;
            }
            // `-secrets` delegates to its own eleven-subcommand reference.
            if token == "-secrets" {
                let parsed = crate::cli::parse(vec!["-secrets".into(), "--help".into()]).unwrap();
                assert!(
                    matches!(parsed, Cmd::Secrets(cli::SecretsCmd::Help)),
                    "-secrets --help must reach the secrets reference, not the derived usage"
                );
                continue;
            }

            let parsed = crate::cli::parse(vec![token.clone(), "--help".into()])
                .unwrap_or_else(|e| panic!("`ws {token} --help` is an error: {e}"));
            let Cmd::VerbHelp(verb) = parsed else {
                panic!("`ws {token} --help` did not ask for help, it parsed as {parsed:?}");
            };
            let usage = crate::cli::verb_usage(&verb);
            assert!(
                usage.lines().count() < crate::cli::help_text().lines().count(),
                "`ws {token} --help` fell back to the whole help text — no line documents it"
            );
            assert!(
                usage.contains(&token),
                "`ws {token} --help` answered with usage that never mentions it:\n{usage}"
            );
        }
    }

    /// The suggestion list an unknown command prints is the dispatch's own
    /// vocabulary, not a copy of it.
    ///
    /// The list this replaces named five verbs of eighteen and could never learn
    /// about a nineteenth. Deriving it is only half the fix — this is the half
    /// that fails if the derivation ever stops seeing a verb the parser accepts.
    #[test]
    fn the_unknown_command_error_offers_every_verb() {
        let aliases = ["-V", "-h", "--help", "--resume", "-resume", "internal", "statusline"];
        let offered = crate::cli::known_verbs();
        for verb in top_level_verbs() {
            if aliases.contains(&verb.as_str()) {
                continue;
            }
            assert!(
                offered.contains(&verb.as_str()),
                "`ws {verb}` is accepted but never offered after a typo: {offered:?}"
            );
        }
    }

    /// A launch *flag* is not a command to suggest. `ws <name> -claude | -codex`
    /// alternates between two flags of one verb, and reading every `|` in the
    /// help put `-codex` and `--clear` in the list of commands to try.
    #[test]
    fn flags_are_not_offered_as_commands() {
        let offered = crate::cli::known_verbs();
        for flag in ["-claude", "-codex", "--clear", "--fresh", "--force", "--merge"] {
            assert!(!offered.contains(&flag), "{flag} is a flag, not a command: {offered:?}");
        }
    }

    /// An argument that merely contains `--help` is data. Scanning the whole
    /// argument list rather than the first token would make this queue nothing
    /// and print the help instead.
    #[test]
    fn help_inside_an_argument_is_not_a_request_for_help() {
        let parsed =
            crate::cli::parse(vec!["-task".into(), "add".into(), "fix --help handling".into()])
                .unwrap();
        assert!(matches!(parsed, Cmd::Task(_)), "parsed as {parsed:?}");
    }

    /// The surface this refocus removed must not creep back into the help text:
    /// a line here is what sends a user looking for a command that no longer
    /// exists.
    #[test]
    fn help_does_not_mention_removed_surface() {
        let help = crate::cli::help_text();
        // `-msg` is deliberately absent from this list: cross-workspace mail
        // was removed in the 0.3.0 refocus and brought back, in the maildir
        // shape rather than the append-to-a-shared-file one that made it fragile.
        for token in ["-tui", "migrate-cs", "-spawn", "-queue", "drain", "--dry-run"] {
            assert!(!help.contains(token), "`ws --help` still mentions removed {token:?}");
        }
    }
}
