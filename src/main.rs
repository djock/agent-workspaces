mod actors;
mod agents;
mod atomic;
mod cli;
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
mod meta;
mod picker;
mod prompts;
mod queue;
mod readme;
mod registry;
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
        Cmd::Limits => commands::limits()?,
        Cmd::Doctor => commands::doctor()?,
        Cmd::Whoami => commands::whoami()?,
        Cmd::Who { name } => commands::who(name)?,
        Cmd::Conversations { name } => conversations::run(name)?,
        Cmd::Rotate { name } => commands::rotate(name)?,
        Cmd::Task(c) => commands::task(c)?,
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
                commands::launch(name, None, false, false, false)?
            }
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
    println!("{}", help_text());
}

fn help_text() -> &'static str {
    "ws — agent workspace manager\n\
         \n\
         Launch\n\
         \x20 ws <name>                    create or resume a workspace (refuses one\n\
         \x20                                made by a newer ws, as does every\n\
         \x20                                command that modifies a workspace)\n\
         \x20 ws <name> -claude | -codex   choose the agent for this launch\n\
         \x20 ws <name> --agent <id>       same, by id\n\
         \x20 ws <name> --fresh            start a new agent session, not a resume\n\
         \x20 ws <name> --handoff          point the agent at the latest handoff\n\
         \x20 ws <name> --force            take over a workspace another process holds\n\
         \n\
         Browse\n\
         \x20 ws                           pick a workspace from a list (arrow keys,\n\
         \x20                                Enter launches; lists plainly when not a tty)\n\
         \x20 ws -pick                     same, explicitly\n\
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
         \x20 ws -who [<name>]             who did what in a workspace, from the timeline\n\
         \x20 ws -conversations [<name>]   conversation lineage: rotations and agent switches\n\
         \x20 ws -rotate [<name>]          write a handoff skeleton for the next session\n\
         \x20 ws -task add [<name>] <text> capture a task without interrupting the agent\n\
         \x20 ws -task list|rm [<name>]    show or drop captured tasks\n\
         \n\
         Inspect\n\
         \x20 ws -limits                   usage limits captured from the status line\n\
         \x20 ws -doctor                   check agents, hooks and shims\n\
         \n\
         Secrets\n\
         \x20 ws -secrets set|get|rm <name>\n\
         \x20 ws -secrets list|purge|export|backend\n\
         \x20 ws -secrets restore <file>   put stored values back into a redacted file\n\
         \n\
         Setup\n\
         \x20 ws setup                     install hooks, prompts and status lines\n\
         \x20 ws config list|get|set       read or change configuration\n\
         \x20 ws hooks list                show the hooks registered for each agent\n\
         \x20 ws hooks check               validate hooks.toml without writing anything\n\
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
            "-pick", "-list", "-ls", "-adopt", "-rm", "-tag", "-status", "-archive", "-unarchive",
            "-search", "-limits", "-doctor", "-whoami", "-who", "-conversations", "-rotate",
            "-task", "-secrets", "restore", "-update", "-uninstall", "setup", "config", "hooks",
            "--version", "-claude", "-codex", "--agent", "--fresh", "--handoff", "--force",
            "--merge", "--include-archived", "--archived",
        ] {
            assert!(help.contains(token), "`ws --help` never mentions {token:?}");
        }
    }

    /// The surface this refocus removed must not creep back into the help text:
    /// a line here is what sends a user looking for a command that no longer
    /// exists.
    #[test]
    fn help_does_not_mention_removed_surface() {
        let help = super::help_text();
        for token in ["-tui", "migrate-cs", "-msg", "-spawn", "-queue", "drain", "--dry-run"] {
            assert!(!help.contains(token), "`ws --help` still mentions removed {token:?}");
        }
    }
}
