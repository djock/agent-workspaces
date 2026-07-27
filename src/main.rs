mod actors;
mod agents;
mod atomic;
mod cli;
mod commands;
mod config;
mod context;
mod contract;
mod conversations;
mod drain;
mod handoff;
mod hookio;
mod hooksetup;
mod internal;
mod limits;
mod lock;
mod mail;
mod meta;
mod migrate;
mod prompts;
mod queue;
mod readme;
mod registry;
mod rows;
mod search;
mod secrets;
mod spawn;
mod statusline;
mod term;
mod timeline;
mod tui;
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
        Cmd::Config(c) => commands::config(c)?,
        Cmd::List { tag, archived } => commands::list(tag, archived)?,
        Cmd::Tag(c) => commands::tag(c)?,
        Cmd::Status { name, text } => commands::status(name, text)?,
        Cmd::Archive { names, archived } => commands::archive(names, archived)?,
        Cmd::Adopt { name } => commands::adopt(name)?,
        Cmd::Rm { names, force } => commands::rm(names, force)?,
        Cmd::Launch { name, agent, fresh, force, handoff } => {
            commands::launch(name, agent, fresh, force, handoff)?
        }
        Cmd::Setup => commands::setup()?,
        Cmd::Internal(args) => internal::run(args)?,
        Cmd::Statusline => statusline::run(),
        Cmd::SubagentStatusline => statusline::run_subagent(),
        Cmd::Limits => commands::limits()?,
        Cmd::Doctor => commands::doctor()?,
        Cmd::Whoami => commands::whoami()?,
        Cmd::Who { name } => commands::who(name)?,
        Cmd::Conversations { name } => conversations::run(name)?,
        Cmd::Msg(c) => commands::msg(c)?,
        Cmd::Queue(c) => commands::queue(c)?,
        Cmd::Spawn { name, task } => spawn::run(name, task)?,
        Cmd::Secrets(c) => commands::secrets(c)?,
        Cmd::Search { query, include_archived } => commands::search(query, include_archived)?,
        Cmd::MigrateCs { names, all, dry_run } => commands::migrate_cs(names, all, dry_run)?,
        Cmd::Update { check, force } => update::run(check, force)?,
        Cmd::Uninstall { force } => commands::uninstall(force)?,
        Cmd::Tui => match tui::run()? {
            tui::Outcome::Quit => {}
            // run() has already restored the terminal; launch execs into the
            // agent from here, replacing this process.
            tui::Outcome::Launch(name) => commands::launch(name, None, false, false, false)?,
        },
        Cmd::Worktree { spec, merge } => {
            let s = worktree::parse_name(&spec)
                .ok_or_else(|| anyhow::anyhow!("not a worktree spec: {spec}"))?;
            if merge {
                worktree::merge(&s)?
            } else {
                let path = worktree::create(&s)?;
                println!("created {} at {}", s.workspace_name(), path.display());
            }
        }
    }
    Ok(())
}

/// ISO-8601 UTC timestamp, e.g. 2026-07-24T10:43:12Z. Shells out to `date`.
pub fn now_iso() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The full command surface.
///
/// This used to omit `-limits`, `-doctor`, `-secrets`, `setup` and **every**
/// launch flag, while the README said it was "the complete command summary".
/// `help_covers_every_command` in the tests below now fails if a command exists
/// that this text does not mention, so the two cannot drift apart again.
fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> &'static str {
    "ws — agent workspace manager\n\
         \n\
         Launch\n\
         \x20 ws <name>                    create or resume a workspace\n\
         \x20 ws <name> -claude | -codex   choose the agent for this launch\n\
         \x20 ws <name> --agent <id>       same, by id\n\
         \x20 ws <name> --fresh            start a new agent session, not a resume\n\
         \x20 ws <name> --handoff          point the agent at the latest handoff\n\
         \x20 ws <name> --force            take over a workspace another process holds\n\
         \n\
         Browse\n\
         \x20 ws                           open the workspace dashboard (TUI)\n\
         \x20 ws -tui                      same, explicitly\n\
         \x20 ws -list | -ls               list workspaces (--tag <t>, --archived)\n\
         \x20 ws -search <query>           search all workspaces (--include-archived)\n\
         \n\
         Manage\n\
         \x20 ws -adopt [<name>]           adopt the current directory\n\
         \x20 ws -rm <name>...             remove workspace(s) (--force)\n\
         \x20 ws -archive | -unarchive <name>...\n\
         \x20 ws -tag add|rm|list [--workspace <n>] <tag>...\n\
         \x20 ws -status \"<text>\" | --clear\n\
         \n\
         Worktrees\n\
         \x20 ws <base>@<feature>          create a git worktree workspace off <base>\n\
         \x20 ws <base>@<feature> --merge  merge it back (--no-ff) and remove it\n\
         \n\
         Coordinate\n\
         \x20 ws -whoami                   print your actor slug\n\
         \x20 ws -who [<name>]             actors who have worked in a workspace\n\
         \x20 ws -conversations [<name>]   conversation lineage: rotations and agent switches\n\
         \x20 ws -msg <name> <body>        send a message to another workspace\n\
         \x20 ws -msg log [<name>]         read the message history\n\
         \x20 ws -queue add <name> <text>  add a task to a workspace's queue\n\
         \x20 ws -queue list [<name>]      show the queue\n\
         \x20 ws -queue drain [<name>]     run pending tasks unattended (--reset)\n\
         \x20 ws -spawn <name>             open a workspace in a tmux window\n\
         \x20 ws -spawn <name> --task <text>  queue it, then drain the WHOLE queue there\n\
         \n\
         Inspect\n\
         \x20 ws -limits                   usage limits captured from the status line\n\
         \x20 ws -doctor                   check agents, hooks and shims\n\
         \n\
         Secrets\n\
         \x20 ws -secrets set|get|rm <name>\n\
         \x20 ws -secrets list|purge|export|backend\n\
         \n\
         Setup\n\
         \x20 ws setup                     install hooks, prompts and status lines\n\
         \x20 ws config list|get|set       read or change configuration\n\
         \x20 ws migrate-cs <name>...|--all   import cs sessions (--dry-run)\n\
         \x20 ws -update                   install the latest release (--check, --force)\n\
         \x20 ws -uninstall                remove ws integrations and binary (--force)\n\
         \x20 ws --version"
}

#[cfg(test)]
mod tests {
    /// The help text is the only user-facing documentation of the command
    /// surface, and it silently fell a third of that surface behind while the
    /// README called it complete. Anything a user can type must appear in it; a
    /// new command that forgets to is a test failure, not a docs bug someone
    /// notices two releases later.
    #[test]
    fn help_covers_every_command() {
        let help = super::help_text();
        for token in [
            "-tui", "-list", "-ls", "-adopt", "-rm", "-tag", "-status", "-archive", "-unarchive",
            "-search", "-limits", "-doctor", "-whoami", "-who", "-conversations", "-msg", "-queue", "-spawn",
            "-secrets", "-update", "-uninstall", "setup", "config", "migrate-cs",
            "--version", "-claude", "-codex", "--agent", "--fresh", "--handoff", "--force",
            "--merge", "--reset", "--dry-run", "--include-archived", "--archived",
        ] {
            assert!(help.contains(token), "`ws --help` never mentions {token:?}");
        }
    }
}
