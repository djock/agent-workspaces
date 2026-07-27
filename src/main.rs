mod actors;
mod agents;
mod atomic;
mod cli;
mod commands;
mod config;
mod context;
mod contract;
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

fn print_help() {
    println!(
        "ws — agent workspace manager\n\n\
         ws <name>            create or resume a workspace (launch Claude)\n\
         ws                   open the workspace dashboard (TUI)\n\
         ws -tui              same, explicitly\n\
         ws -list | -ls       list workspaces (--tag <t>, --archived)\n\
         ws -adopt [<name>]   adopt the current directory\n\
         ws -rm <name>...     remove workspace(s)\n\
         ws -tag add|rm|list [--workspace <n>] <tag>...\n\
         ws -status \"<text>\" | --clear\n\
         ws -archive | -unarchive <name>...\n\
         ws -search <query>   search all workspaces (--include-archived)\n\
         ws -whoami              print your actor slug\n\
         ws -who [<name>]        actors who have worked in a workspace\n\
         ws -msg <name> <body>   send a message to another workspace\n\
         ws -msg log [<name>]    read the message history\n\
         ws -queue add <name> <text>   add a task to a workspace's queue\n\
         ws -queue list [<name>]       show the queue\n\
         ws -queue drain [<name>]      run pending tasks through the agent\n\
         ws -spawn <name>        open a workspace in a tmux window\n\
         ws -spawn <name> --task <text>   queue it, then drain the WHOLE queue there\n\
         ws -update           install the latest release (--check, --force)\n\
         ws -uninstall        remove ws integrations and binary (--force)\n\
         ws migrate-cs <name>...|--all   import cs sessions (--dry-run)\n\
         ws <base>@<feature>     create a git worktree workspace off <base>\n\
         ws <base>@<feature> --merge   merge it back (--no-ff) and remove it\n\
         ws config ...        get/set/list config\n\
         ws --version"
    );
}
