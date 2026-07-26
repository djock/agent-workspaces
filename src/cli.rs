use anyhow::{bail, Result};
use std::io::IsTerminal;

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Launch { name: String, agent: Option<String>, fresh: bool, force: bool, handoff: bool },
    List { tag: Option<String>, archived: bool },
    Adopt { name: Option<String> },
    Rm { names: Vec<String>, force: bool },
    Config(ConfigCmd),
    Version,
    Help,
    Setup,
    Internal(Vec<String>),
    Statusline,
    SubagentStatusline,
    Limits,
    Doctor,
    Secrets(SecretsCmd),
    Tag(TagCmd),
    Status { name: Option<String>, text: Option<String> },
    Archive { names: Vec<String>, archived: bool },
    Search { query: String, include_archived: bool },
    MigrateCs { names: Vec<String>, all: bool, dry_run: bool },
    Update { check: bool, force: bool },
    Uninstall { force: bool },
    Tui,
    Whoami,
    Who { name: Option<String> },
    Msg(MsgCmd),
    Queue(QueueCmd),
    Spawn { name: String, task: Option<String> },
    Worktree { spec: String, merge: bool },
}

#[derive(Debug, PartialEq)]
pub enum QueueCmd {
    Add { name: String, text: String },
    List { name: Option<String> },
    Drain { name: Option<String>, reset: bool },
}

#[derive(Debug, PartialEq)]
pub enum MsgCmd {
    Send { to: String, body: String },
    Log { name: Option<String> },
}

#[derive(Debug, PartialEq)]
pub enum TagCmd {
    Add { name: Option<String>, tags: Vec<String> },
    Rm { name: Option<String>, tags: Vec<String> },
    List { name: Option<String> },
}

#[derive(Debug, PartialEq)]
pub enum SecretsCmd {
    Set(String),
    Get(String),
    List,
    Rm(String),
    Purge,
    Export,
    Backend,
}

#[derive(Debug, PartialEq)]
pub enum ConfigCmd {
    List,
    Get(String),
    Set { key: String, value: String, workspace: bool },
}

/// Parse argv (excluding the program name) into a Cmd.
/// Top-level pseudo-subcommands use a single leading dash (`-list`) to match
/// the spec's CLI surface; `config` is a bare-word subcommand.
pub fn parse(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let first = match it.next() {
        // Bare `ws` opens the dashboard interactively; piped or redirected
        // (scripts, `ws | grep`) it stays the plain list it has always been.
        None => {
            return Ok(if std::io::stdout().is_terminal() {
                Cmd::Tui
            } else {
                Cmd::List { tag: None, archived: false }
            })
        }
        Some(a) => a,
    };

    match first.as_str() {
        "-V" | "--version" => Ok(Cmd::Version),
        "-h" | "--help" => Ok(Cmd::Help),
        "-tui" => Ok(Cmd::Tui),
        "-list" | "-ls" => parse_list(it.collect()),
        "-limits" => Ok(Cmd::Limits),
        "-doctor" => Ok(Cmd::Doctor),
        "-whoami" => {
            if it.next().is_some() {
                bail!("usage: ws -whoami");
            }
            Ok(Cmd::Whoami)
        }
        "-who" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -who [<name>]");
            }
            Ok(Cmd::Who { name })
        }
        "-update" => {
            let mut check = false;
            let mut force = false;
            for a in it {
                match a.as_str() {
                    "--check" => check = true,
                    "--force" => force = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            if check && force {
                bail!("ws -update: --check and --force are mutually exclusive");
            }
            Ok(Cmd::Update { check, force })
        }
        "-uninstall" => {
            let mut force = false;
            for a in it {
                match a.as_str() {
                    "--force" => force = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Uninstall { force })
        }
        "-secrets" => parse_secrets(it.collect()),
        "-tag" => parse_tag(it.collect()),
        "-msg" => parse_msg(it.collect()),
        "-queue" => parse_queue(it.collect()),
        "-spawn" => {
            let mut it = it;
            let name = it
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: ws -spawn <name> [--task <text>]"))?;
            let mut task = None;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--task" => {
                        let rest: Vec<String> = it.by_ref().collect();
                        if rest.is_empty() {
                            bail!("usage: ws -spawn <name> --task <text>");
                        }
                        task = Some(rest.join(" "));
                    }
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Spawn { name, task })
        }
        "-status" => parse_status(it.collect()),
        "-archive" => parse_archive(it.collect(), true),
        "-unarchive" => parse_archive(it.collect(), false),
        "-search" => {
            let mut query = None;
            let mut include_archived = false;
            for a in it {
                match a.as_str() {
                    "--include-archived" => include_archived = true,
                    other if other.starts_with("--") => bail!("unexpected argument: {other}"),
                    other if query.is_none() => query = Some(other.to_string()),
                    other => bail!("unexpected argument: {other}"),
                }
            }
            let query = query.ok_or_else(|| anyhow::anyhow!("usage: ws -search <query> [--include-archived]"))?;
            Ok(Cmd::Search { query, include_archived })
        }
        "-adopt" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -adopt [<name>]");
            }
            Ok(Cmd::Adopt { name })
        }
        "-rm" => {
            let mut names = Vec::new();
            let mut force = false;
            for a in it {
                match a.as_str() {
                    "--force" => force = true,
                    _ => names.push(a),
                }
            }
            if names.is_empty() {
                bail!("usage: ws -rm <name>... [--force]");
            }
            Ok(Cmd::Rm { names, force })
        }
        "config" => parse_config(it.collect()),
        "migrate-cs" => {
            let mut names = Vec::new();
            let mut all = false;
            let mut dry_run = false;
            for a in it {
                match a.as_str() {
                    "--all" => all = true,
                    "--dry-run" => dry_run = true,
                    other if other.starts_with("--") => bail!("unexpected argument: {other}"),
                    other => names.push(other.to_string()),
                }
            }
            if names.is_empty() && !all {
                bail!("usage: ws migrate-cs <name>... | --all [--dry-run]");
            }
            if all && !names.is_empty() {
                bail!("ws migrate-cs: give session names or --all, not both");
            }
            Ok(Cmd::MigrateCs { names, all, dry_run })
        }
        "setup" => Ok(Cmd::Setup),
        "internal" => Ok(Cmd::Internal(it.collect())),
        "statusline" => Ok(Cmd::Statusline),
        "subagent-statusline" => Ok(Cmd::SubagentStatusline),
        other if other.starts_with('-') => {
            bail!("unknown command: {other}\ntry: ws -list | ws -adopt | ws -rm | ws config | ws <name>");
        }
        // Known limitation (M1): this arm claims *every* bare name with an
        // inner `@`, so an adopted workspace literally named `client@acme`
        // routes here instead of to the launch arm below and cannot be
        // launched from the CLI. It fails safe — `worktree::create` bails
        // "already exists" via `lookup_checked` rather than touching
        // anything — so the workspace is unreachable, never damaged.
        // Disambiguating would mean consulting the registry from the parser.
        name if crate::worktree::parse_name(name).is_some() => {
            let mut merge = false;
            for a in it {
                match a.as_str() {
                    "--merge" => merge = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Worktree { spec: name.to_string(), merge })
        }
        name => {
            // launch: ws <name> [-claude|-codex] [-resume|-fresh|--fresh] [--agent X] [--force] [--handoff]
            let mut agent = None;
            let mut fresh = false;
            let mut resume = false;
            let mut force = false;
            let mut handoff = false;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--agent" => agent = it.next(),
                    "-claude" => agent = Some("claude".into()),
                    "-codex" => agent = Some("codex".into()),
                    "--fresh" | "-fresh" => fresh = true,
                    "-resume" => resume = true,
                    "--force" => force = true,
                    "--handoff" => handoff = true,
                    other => bail!("unexpected argument: {other}"),
                }
            }
            if fresh && resume {
                bail!("-fresh and -resume are mutually exclusive");
            }
            Ok(Cmd::Launch { name: name.to_string(), agent, fresh, force, handoff })
        }
    }
}

fn parse_secrets(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it.next().unwrap_or_default();
    let cmd = match sub.as_str() {
        "set" => SecretsCmd::Set(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets set <name>"))?),
        "get" => SecretsCmd::Get(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets get <name>"))?),
        "rm" => SecretsCmd::Rm(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets rm <name>"))?),
        "list" => SecretsCmd::List,
        "purge" => SecretsCmd::Purge,
        "export" => SecretsCmd::Export,
        "backend" => SecretsCmd::Backend,
        other => bail!("unknown -secrets subcommand: {other}"),
    };
    Ok(Cmd::Secrets(cmd))
}

fn parse_list(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let mut tag = None;
    let mut archived = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--archived" => archived = true,
            "--tag" => tag = Some(it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -list [--tag <tag>] [--archived]"))?),
            other => bail!("unexpected argument: {other}"),
        }
    }
    Ok(Cmd::List { tag, archived })
}

/// Pull an optional `--workspace <name>` out of `args`, returning it plus the rest.
/// Any other residual token starting with `--` is rejected as a usage error —
/// a one-character typo in `--workspace` (e.g. `--workspac`) must not be
/// silently swallowed as a tag/status value, or worse, mutate the current
/// workspace instead of the intended one. A tag/value that merely *contains*
/// dashes (`foo-bar`) is unaffected — only a leading `--` is rejected.
fn take_workspace(args: Vec<String>) -> Result<(Option<String>, Vec<String>)> {
    let mut it = args.into_iter();
    let mut name = None;
    let mut rest = Vec::new();
    while let Some(a) = it.next() {
        if a == "--workspace" {
            name = Some(it.next().ok_or_else(|| anyhow::anyhow!("--workspace needs a name"))?);
        } else if a.starts_with("--") {
            bail!("unexpected argument: {a}");
        } else {
            rest.push(a);
        }
    }
    Ok((name, rest))
}

fn parse_msg(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let first = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ws -msg <name> <body> | ws -msg log [<name>]"))?;
    if first == "log" {
        let name = it.next();
        if it.next().is_some() {
            bail!("usage: ws -msg log [<name>]");
        }
        return Ok(Cmd::Msg(MsgCmd::Log { name }));
    }
    let rest: Vec<String> = it.collect();
    if rest.is_empty() {
        bail!("usage: ws -msg <name> <body>");
    }
    Ok(Cmd::Msg(MsgCmd::Send { to: first, body: rest.join(" ") }))
}

fn parse_queue(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ws -queue add|list|drain ..."))?;
    match sub.as_str() {
        "add" => {
            let name = it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -queue add <name> <text>"))?;
            let rest: Vec<String> = it.collect();
            if rest.is_empty() {
                bail!("usage: ws -queue add <name> <text>");
            }
            Ok(Cmd::Queue(QueueCmd::Add { name, text: rest.join(" ") }))
        }
        "list" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -queue list [<name>]");
            }
            Ok(Cmd::Queue(QueueCmd::List { name }))
        }
        "drain" => {
            let mut name = None;
            let mut reset = false;
            for a in it {
                match a.as_str() {
                    "--reset" => reset = true,
                    other if other.starts_with("--") => bail!("unexpected argument: {other}"),
                    other if name.is_none() => name = Some(other.to_string()),
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Queue(QueueCmd::Drain { name, reset }))
        }
        other => bail!("unknown queue subcommand: {other} (try add, list, drain)"),
    }
}

fn parse_tag(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it.next().unwrap_or_default();
    let (name, tags) = take_workspace(it.collect())?;
    let cmd = match sub.as_str() {
        "add" | "rm" => {
            if tags.is_empty() {
                bail!("usage: ws -tag {sub} [--workspace <name>] <tag>...");
            }
            if sub == "add" { TagCmd::Add { name, tags } } else { TagCmd::Rm { name, tags } }
        }
        "list" => {
            if !tags.is_empty() {
                bail!("usage: ws -tag list [--workspace <name>]");
            }
            TagCmd::List { name }
        }
        other => bail!("unknown -tag subcommand: {other} (want add|rm|list)"),
    };
    Ok(Cmd::Tag(cmd))
}

fn parse_status(args: Vec<String>) -> Result<Cmd> {
    let mut clear = false;
    let args: Vec<String> = args
        .into_iter()
        .filter(|a| {
            if a == "--clear" { clear = true; false } else { true }
        })
        .collect();
    let (name, rest) = take_workspace(args)?;
    if clear {
        if !rest.is_empty() {
            bail!("ws -status: --clear takes no text");
        }
        return Ok(Cmd::Status { name, text: None });
    }
    match rest.len() {
        1 => Ok(Cmd::Status { name, text: Some(rest[0].clone()) }),
        _ => bail!("usage: ws -status [--workspace <name>] \"<text>\" | --clear"),
    }
}

fn parse_archive(args: Vec<String>, archived: bool) -> Result<Cmd> {
    if args.is_empty() {
        bail!("usage: ws {} <name>...", if archived { "-archive" } else { "-unarchive" });
    }
    Ok(Cmd::Archive { names: args, archived })
}

fn parse_config(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    match it.next().as_deref() {
        None | Some("list") => Ok(Cmd::Config(ConfigCmd::List)),
        Some("get") => {
            let key = it.next().ok_or_else(|| anyhow::anyhow!("usage: ws config get <key>"))?;
            Ok(Cmd::Config(ConfigCmd::Get(key)))
        }
        Some("set") => {
            let mut workspace = false;
            let mut rest: Vec<String> = Vec::new();
            for a in it {
                if a == "--workspace" {
                    workspace = true;
                } else {
                    rest.push(a);
                }
            }
            if rest.len() != 2 {
                bail!("usage: ws config set [--workspace] <key> <value>");
            }
            Ok(Cmd::Config(ConfigCmd::Set {
                key: rest[0].clone(),
                value: rest[1].clone(),
                workspace,
            }))
        }
        Some(other) => bail!("unknown config subcommand: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &[&str]) -> Cmd {
        parse(s.iter().map(|x| x.to_string()).collect()).unwrap()
    }

    #[test]
    fn a_name_with_an_at_parses_as_a_worktree_not_a_launch() {
        assert_eq!(p(&["api@retry"]), Cmd::Worktree { spec: "api@retry".into(), merge: false });
        assert_eq!(p(&["api@retry", "--merge"]), Cmd::Worktree { spec: "api@retry".into(), merge: true });
    }

    #[test]
    fn a_plain_name_still_launches() {
        assert_eq!(
            p(&["api"]),
            Cmd::Launch { name: "api".into(), agent: None, fresh: false, force: false, handoff: false }
        );
    }

    #[test]
    fn a_malformed_worktree_spec_is_treated_as_an_ordinary_name() {
        // "api@" is not a worktree spec; it must not silently become one.
        match p(&["api@"]) {
            Cmd::Launch { name, .. } => assert_eq!(name, "api@"),
            other => panic!("expected a launch, got {other:?}"),
        }
    }

    #[test]
    fn launch_defaults() {
        assert_eq!(
            p(&["mywork"]),
            Cmd::Launch { name: "mywork".into(), agent: None, fresh: false, force: false, handoff: false }
        );
    }

    #[test]
    fn launch_flags() {
        assert_eq!(
            p(&["mywork", "--agent", "claude", "--fresh", "--force"]),
            Cmd::Launch { name: "mywork".into(), agent: Some("claude".into()), fresh: true, force: true, handoff: false }
        );
    }

    #[test]
    fn agent_shorthand_flags() {
        assert_eq!(p(&["proj", "-codex"]), p(&["proj", "--agent", "codex"]));
        assert_eq!(p(&["proj", "-claude"]), p(&["proj", "--agent", "claude"]));
    }

    #[test]
    fn resume_and_fresh_and_handoff() {
        match p(&["proj", "-resume", "-codex"]) {
            Cmd::Launch { name, agent, fresh, handoff, .. } => {
                assert_eq!(name, "proj");
                assert_eq!(agent.as_deref(), Some("codex"));
                assert!(!fresh);
                assert!(!handoff);
            }
            _ => panic!(),
        }
        match p(&["proj", "-fresh", "--handoff"]) {
            Cmd::Launch { fresh, handoff, .. } => {
                assert!(fresh);
                assert!(handoff);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn resume_and_fresh_conflict_errors() {
        assert!(parse(vec!["proj".into(), "-resume".into(), "-fresh".into()]).is_err());
    }

    #[test]
    fn rm_collects_names_and_force() {
        assert_eq!(
            p(&["-rm", "a", "b", "--force"]),
            Cmd::Rm { names: vec!["a".into(), "b".into()], force: true }
        );
    }

    #[test]
    fn config_set_workspace() {
        assert_eq!(
            p(&["config", "set", "--workspace", "default_agent", "codex"]),
            Cmd::Config(ConfigCmd::Set { key: "default_agent".into(), value: "codex".into(), workspace: true })
        );
    }

    #[test]
    fn unknown_dash() {
        assert!(parse(vec!["-nope".into()]).is_err());
    }

    #[test]
    fn adopt_rejects_extra_args() {
        assert!(parse(vec!["-adopt".into(),"a".into(),"b".into()]).is_err());
    }

    #[test]
    fn parses_setup_and_internal() {
        assert_eq!(p(&["setup"]), Cmd::Setup);
        assert_eq!(
            p(&["internal", "session-start"]),
            Cmd::Internal(vec!["session-start".into()])
        );
        assert_eq!(
            p(&["internal", "hook-payload", "source"]),
            Cmd::Internal(vec!["hook-payload".into(), "source".into()])
        );
    }

    #[test]
    fn list_filters() {
        assert_eq!(p(&["-list"]), Cmd::List { tag: None, archived: false });
        assert_eq!(p(&["-ls", "--archived"]), Cmd::List { tag: None, archived: true });
        assert_eq!(
            p(&["-list", "--tag", "rust"]),
            Cmd::List { tag: Some("rust".into()), archived: false }
        );
        assert_eq!(p(&[]), Cmd::List { tag: None, archived: false });
    }

    #[test]
    fn tag_subcommands() {
        assert_eq!(
            p(&["-tag", "add", "rust", "cli"]),
            Cmd::Tag(TagCmd::Add { name: None, tags: vec!["rust".into(), "cli".into()] })
        );
        assert_eq!(
            p(&["-tag", "rm", "--workspace", "proj", "rust"]),
            Cmd::Tag(TagCmd::Rm { name: Some("proj".into()), tags: vec!["rust".into()] })
        );
        assert_eq!(p(&["-tag", "list"]), Cmd::Tag(TagCmd::List { name: None }));
        assert_eq!(
            p(&["-tag", "list", "--workspace", "proj"]),
            Cmd::Tag(TagCmd::List { name: Some("proj".into()) })
        );
        // add/rm need at least one tag
        assert!(parse(vec!["-tag".into(), "add".into()]).is_err());
        assert!(parse(vec!["-tag".into(), "bogus".into()]).is_err());
    }

    #[test]
    fn tag_rejects_unknown_leading_dash_dash_flags() {
        // A one-character typo in --workspace must not be swallowed as a tag
        // value, and must not silently mutate the current workspace instead.
        assert!(parse(vec!["-tag".into(), "add".into(), "--typo".into(), "rust".into()]).is_err());
        assert!(parse(vec![
            "-tag".into(),
            "add".into(),
            "--workspac".into(),
            "proj".into(),
            "rust".into(),
        ])
        .is_err());
        // A legitimate tag that merely contains dashes is still accepted.
        assert_eq!(
            p(&["-tag", "add", "foo-bar"]),
            Cmd::Tag(TagCmd::Add { name: None, tags: vec!["foo-bar".into()] })
        );
    }

    #[test]
    fn status_set_and_clear() {
        assert_eq!(
            p(&["-status", "waiting on review"]),
            Cmd::Status { name: None, text: Some("waiting on review".into()) }
        );
        assert_eq!(p(&["-status", "--clear"]), Cmd::Status { name: None, text: None });
        assert_eq!(
            p(&["-status", "--workspace", "proj", "busy"]),
            Cmd::Status { name: Some("proj".into()), text: Some("busy".into()) }
        );
        // `-status` with no argument is ambiguous — require --clear to clear.
        assert!(parse(vec!["-status".into()]).is_err());
    }

    #[test]
    fn search_parses_query_and_flag() {
        assert_eq!(
            p(&["-search", "kraken"]),
            Cmd::Search { query: "kraken".into(), include_archived: false }
        );
        assert_eq!(
            p(&["-search", "kraken", "--include-archived"]),
            Cmd::Search { query: "kraken".into(), include_archived: true }
        );
        assert!(parse(vec!["-search".into()]).is_err());
        // a stray unrecognized flag must not be swallowed as the literal query
        assert!(parse(vec!["-search".into(), "--typo".into()]).is_err());
        // a legitimate query that merely contains dashes is still accepted
        assert_eq!(
            p(&["-search", "foo--bar"]),
            Cmd::Search { query: "foo--bar".into(), include_archived: false }
        );
    }

    #[test]
    fn migrate_cs_parses() {
        assert_eq!(
            p(&["migrate-cs", "--all"]),
            Cmd::MigrateCs { names: vec![], all: true, dry_run: false }
        );
        assert_eq!(
            p(&["migrate-cs", "alpha", "beta"]),
            Cmd::MigrateCs { names: vec!["alpha".into(), "beta".into()], all: false, dry_run: false }
        );
        assert_eq!(
            p(&["migrate-cs", "--all", "--dry-run"]),
            Cmd::MigrateCs { names: vec![], all: true, dry_run: true }
        );
        // neither names nor --all is a usage error
        assert!(parse(vec!["migrate-cs".into()]).is_err());
        // names combined with --all is rejected, not silently reduced to --all
        assert!(parse(vec!["migrate-cs".into(), "alpha".into(), "--all".into()]).is_err());
    }

    #[test]
    fn update_and_uninstall_parse() {
        assert_eq!(p(&["-update"]), Cmd::Update { check: false, force: false });
        assert_eq!(p(&["-update", "--check"]), Cmd::Update { check: true, force: false });
        assert_eq!(p(&["-update", "--force"]), Cmd::Update { check: false, force: true });
        assert!(parse(vec!["-update".into(), "--check".into(), "--force".into()]).is_err());
        assert!(parse(vec!["-update".into(), "--unknown".into()]).is_err());

        assert_eq!(p(&["-uninstall"]), Cmd::Uninstall { force: false });
        assert_eq!(p(&["-uninstall", "--force"]), Cmd::Uninstall { force: true });
        assert!(parse(vec!["-uninstall".into(), "--unknown".into()]).is_err());
    }

    #[test]
    fn parses_whoami_and_who() {
        assert_eq!(p(&["-whoami"]), Cmd::Whoami);
        assert_eq!(p(&["-who"]), Cmd::Who { name: None });
        assert_eq!(p(&["-who", "proj"]), Cmd::Who { name: Some("proj".into()) });
    }

    #[test]
    fn who_rejects_a_second_name() {
        assert!(parse(vec!["-who".into(), "a".into(), "b".into()]).is_err());
    }

    #[test]
    fn archive_and_unarchive() {
        assert_eq!(
            p(&["-archive", "a", "b"]),
            Cmd::Archive { names: vec!["a".into(), "b".into()], archived: true }
        );
        assert_eq!(
            p(&["-unarchive", "a"]),
            Cmd::Archive { names: vec!["a".into()], archived: false }
        );
        assert!(parse(vec!["-archive".into()]).is_err());
    }

    #[test]
    fn parses_msg_send_and_log() {
        assert_eq!(
            p(&["-msg", "proj", "ship it"]),
            Cmd::Msg(MsgCmd::Send { to: "proj".into(), body: "ship it".into() })
        );
        assert_eq!(p(&["-msg", "log"]), Cmd::Msg(MsgCmd::Log { name: None }));
        assert_eq!(p(&["-msg", "log", "proj"]), Cmd::Msg(MsgCmd::Log { name: Some("proj".into()) }));
    }

    #[test]
    fn msg_send_requires_a_body() {
        assert!(parse(vec!["-msg".into(), "proj".into()]).is_err());
    }

    #[test]
    fn parses_queue_subcommands() {
        assert_eq!(
            p(&["-queue", "add", "proj", "write the docs"]),
            Cmd::Queue(QueueCmd::Add { name: "proj".into(), text: "write the docs".into() })
        );
        assert_eq!(p(&["-queue", "list", "proj"]), Cmd::Queue(QueueCmd::List { name: Some("proj".into()) }));
        assert_eq!(
            p(&["-queue", "drain", "proj"]),
            Cmd::Queue(QueueCmd::Drain { name: Some("proj".into()), reset: false })
        );
        assert_eq!(
            p(&["-queue", "drain", "proj", "--reset"]),
            Cmd::Queue(QueueCmd::Drain { name: Some("proj".into()), reset: true })
        );
    }

    #[test]
    fn queue_add_requires_a_target_and_text() {
        assert!(parse(vec!["-queue".into(), "add".into()]).is_err());
        assert!(parse(vec!["-queue".into(), "add".into(), "proj".into()]).is_err());
    }

    #[test]
    fn an_unknown_queue_subcommand_is_rejected() {
        assert!(parse(vec!["-queue".into(), "flush".into()]).is_err());
    }

    #[test]
    fn parses_spawn_with_and_without_a_task() {
        assert_eq!(p(&["-spawn", "proj"]), Cmd::Spawn { name: "proj".into(), task: None });
        assert_eq!(
            p(&["-spawn", "proj", "--task", "write the docs"]),
            Cmd::Spawn { name: "proj".into(), task: Some("write the docs".into()) }
        );
    }

    #[test]
    fn spawn_requires_a_name_and_a_task_body() {
        assert!(parse(vec!["-spawn".into()]).is_err());
        assert!(parse(vec!["-spawn".into(), "proj".into(), "--task".into()]).is_err());
    }
}
