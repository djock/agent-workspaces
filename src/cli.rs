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
    Limits,
    Doctor,
    Secrets(SecretsCmd),
    Tag(TagCmd),
    Status { name: Option<String>, text: Option<String> },
    Color { name: Option<String>, color: Option<String> },
    Archive { names: Vec<String>, archived: bool },
    Search { query: String, include_archived: bool },
    Update { check: bool, force: bool },
    Uninstall { force: bool },
    Pick,
    Whoami,
    Who { name: Option<String> },
    Conversations { name: Option<String> },
    Rotate { name: Option<String> },
    Task(TaskCmd),
    Hooks(HooksCmd),
    Worktree { spec: String, merge: bool },
}

/// Task capture. `add` defaults to the current workspace so `/ws:task` can call
/// it without knowing where it is; an explicit name is still accepted.
#[derive(Debug, PartialEq)]
pub enum TaskCmd {
    Add { name: Option<String>, text: String },
    List { name: Option<String> },
    Rm { name: Option<String>, index: usize },
}

#[derive(Debug, PartialEq)]
pub enum HooksCmd {
    /// Show what is registered for each agent, built-in and user-defined.
    List,
    /// Validate hooks.toml and print what would be written, writing nothing.
    Check,
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
    /// Put stored values back into a file the redaction hook rewrote.
    /// One positional path; the file is edited in place.
    Restore(String),
    Help,
}

pub const SECRETS_USAGE: &str = "\
usage: ws -secrets <subcommand>

  set <name>      store a secret; the value is read from stdin, never argv
  get <name>      print one secret's value
  list            print the stored names (never the values)
  rm <name>       remove one secret
  purge           remove every secret for this workspace (needs a TTY)
  export          print `export WS_SECRET_NAME='value'` lines for eval
  backend         print which store is in use (keyring or file)
  restore <file>  put stored values back into a redacted file, in place
  help            print this message";

#[derive(Debug, PartialEq)]
pub enum ConfigCmd {
    List,
    Get(String),
    Set { key: String, value: String },
}

/// Parse argv (excluding the program name) into a Cmd.
/// Top-level pseudo-subcommands use a single leading dash (`-list`) to match
/// the spec's CLI surface; `config` is a bare-word subcommand.
pub fn parse(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let first = match it.next() {
        // Bare `ws` offers the arrow-key picker interactively; piped or
        // redirected (scripts, `ws | grep`) it stays the plain list it has
        // always been.
        None => {
            return Ok(if std::io::stdout().is_terminal() {
                Cmd::Pick
            } else {
                Cmd::List { tag: None, archived: false }
            })
        }
        Some(a) => a,
    };

    match first.as_str() {
        "-V" | "--version" => Ok(Cmd::Version),
        "-h" | "--help" => Ok(Cmd::Help),
        "-pick" => Ok(Cmd::Pick),
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
        "-conversations" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -conversations [<name>]");
            }
            Ok(Cmd::Conversations { name })
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
        "-task" => parse_task(it.collect()),
        "-rotate" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -rotate [<name>]");
            }
            Ok(Cmd::Rotate { name })
        }
        "hooks" => {
            let sub = it.next().unwrap_or_default();
            if it.next().is_some() {
                bail!("usage: ws hooks list|check");
            }
            match sub.as_str() {
                "list" => Ok(Cmd::Hooks(HooksCmd::List)),
                "check" => Ok(Cmd::Hooks(HooksCmd::Check)),
                "" => bail!("usage: ws hooks list|check"),
                other => bail!("unknown hooks subcommand: {other} (want list|check)"),
            }
        }
        "-status" => parse_status(it.collect()),
        "-color" => parse_color(it.collect()),
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
            let query = query
                .ok_or_else(|| anyhow::anyhow!("usage: ws -search <query> [--include-archived]"))?;
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
                    // A typo'd flag used to become a workspace *name*: `ws -rm
                    // --forec myws` tried to delete a workspace literally called
                    // "--forec", reported "no such workspace", exited 0, and never
                    // touched myws. Every other parser here rejects unknown `--`
                    // tokens; this is the destructive command, so it must too.
                    other if other.starts_with("--") => {
                        bail!("unexpected argument: {other}\nusage: ws -rm <name>... [--force]")
                    }
                    _ => names.push(a),
                }
            }
            if names.is_empty() {
                bail!("usage: ws -rm <name>... [--force]");
            }
            Ok(Cmd::Rm { names, force })
        }
        "config" => parse_config(it.collect()),
        "setup" => Ok(Cmd::Setup),
        "internal" => Ok(Cmd::Internal(it.collect())),
        "statusline" => Ok(Cmd::Statusline),
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
            // launch: ws <name> [-claude|-codex] [--fresh|-fresh] [--agent X] [--force] [--handoff]
            //
            // `-resume` was accepted here and did nothing: resuming is already
            // the default, so the flag only existed to be mutually exclusive
            // with `-fresh`. Rather than keep a no-op that reads like a feature,
            // it is rejected with a message saying why — silently accepting it
            // would leave users believing they had opted into something.
            let mut agent = None;
            let mut fresh = false;
            let mut force = false;
            let mut handoff = false;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--agent" => agent = it.next(),
                    "-claude" => agent = Some("claude".into()),
                    "-codex" => agent = Some("codex".into()),
                    "--fresh" | "-fresh" => fresh = true,
                    "--force" => force = true,
                    "--handoff" => handoff = true,
                    "-resume" | "--resume" => bail!(
                        "-resume is not a flag: resuming is the default. \
                         Use `ws {name}` to resume, or `ws {name} --fresh` to start a new session."
                    ),
                    other => bail!("unexpected argument: {other}"),
                }
            }
            Ok(Cmd::Launch { name: name.to_string(), agent, fresh, force, handoff })
        }
    }
}

fn parse_secrets(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it.next().unwrap_or_default();
    let cmd = match sub.as_str() {
        "set" => SecretsCmd::Set(
            it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets set <name>"))?,
        ),
        "get" => SecretsCmd::Get(
            it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets get <name>"))?,
        ),
        "rm" => SecretsCmd::Rm(
            it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets rm <name>"))?,
        ),
        "list" => SecretsCmd::List,
        "purge" => SecretsCmd::Purge,
        "export" => SecretsCmd::Export,
        "backend" => SecretsCmd::Backend,
        "restore" => SecretsCmd::Restore(
            it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -secrets restore <file>"))?,
        ),
        // Bare `ws -secrets` is a request to be told what the subcommands are,
        // not a mistake worth an error.
        "help" | "--help" | "-h" | "" => SecretsCmd::Help,
        other => bail!("unknown -secrets subcommand: {other}\n\n{SECRETS_USAGE}"),
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
            "--tag" => {
                tag =
                    Some(it.next().ok_or_else(|| {
                        anyhow::anyhow!("usage: ws -list [--tag <tag>] [--archived]")
                    })?)
            }
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

/// `ws -task add [<name>] <text>` / `list [<name>]` / `rm [<name>] <index>`.
///
/// `add` takes the name **optionally** and before the text, which is ambiguous
/// on its own — "is the first word a workspace or the start of the task?" It is
/// resolved by the registry: a first word that names a registered workspace is
/// the target, anything else starts the text. That keeps `/ws:task` usable
/// without the agent having to know or pass its own workspace name, which is the
/// whole point of the command.
fn parse_task(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter();
    let sub = it.next().ok_or_else(|| anyhow::anyhow!("usage: ws -task add|list|rm ..."))?;
    match sub.as_str() {
        "add" => {
            let words: Vec<String> = it.collect();
            if words.is_empty() {
                bail!("usage: ws -task add [<name>] <text>");
            }
            let (name, rest) = split_optional_workspace(words);
            if rest.is_empty() {
                bail!("usage: ws -task add [<name>] <text>");
            }
            Ok(Cmd::Task(TaskCmd::Add { name, text: rest.join(" ") }))
        }
        "list" => {
            let name = it.next();
            if it.next().is_some() {
                bail!("usage: ws -task list [<name>]");
            }
            Ok(Cmd::Task(TaskCmd::List { name }))
        }
        "rm" => {
            let words: Vec<String> = it.collect();
            if words.is_empty() {
                bail!("usage: ws -task rm [<name>] <index>");
            }
            let (name, rest) = split_optional_workspace(words);
            let idx = rest
                .first()
                .ok_or_else(|| anyhow::anyhow!("usage: ws -task rm [<name>] <index>"))?;
            let index: usize = idx
                .parse()
                .map_err(|_| anyhow::anyhow!("task index must be a number, got {idx:?}"))?;
            if rest.len() > 1 {
                bail!("usage: ws -task rm [<name>] <index>");
            }
            Ok(Cmd::Task(TaskCmd::Rm { name, index }))
        }
        other => bail!("unknown task subcommand: {other} (want add|list|rm)"),
    }
}

/// Peel a leading workspace name off `words` when it names one that is actually
/// registered. Parsing must not consult the registry for *flags*, but for this
/// one positional ambiguity it is the only honest disambiguator available.
fn split_optional_workspace(words: Vec<String>) -> (Option<String>, Vec<String>) {
    if words.len() > 1 && crate::registry::lookup(&words[0]).is_some() {
        let mut it = words.into_iter();
        let name = it.next();
        return (name, it.collect());
    }
    (None, words)
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
            if sub == "add" {
                TagCmd::Add { name, tags }
            } else {
                TagCmd::Rm { name, tags }
            }
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
            if a == "--clear" {
                clear = true;
                false
            } else {
                true
            }
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

/// `ws -color <name>` sets the workspace's tab and status-line color;
/// `--clear` removes it, and the next launch allocates a fresh one.
fn parse_color(args: Vec<String>) -> Result<Cmd> {
    let mut clear = false;
    let args: Vec<String> = args
        .into_iter()
        .filter(|a| {
            if a == "--clear" {
                clear = true;
                false
            } else {
                true
            }
        })
        .collect();
    let (name, rest) = take_workspace(args)?;
    if clear {
        if !rest.is_empty() {
            bail!("ws -color: --clear takes no color");
        }
        return Ok(Cmd::Color { name, color: None });
    }
    match rest.len() {
        1 => {
            let color = rest[0].to_ascii_lowercase();
            if crate::term::rgb(&color).is_none() {
                bail!(
                    "ws -color: unknown color: {color} (want {})",
                    crate::term::PALETTE.join("|")
                );
            }
            Ok(Cmd::Color { name, color: Some(color) })
        }
        _ => bail!(
            "usage: ws -color [--workspace <name>] <{}> | --clear",
            crate::term::PALETTE.join("|")
        ),
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
            let mut rest: Vec<String> = Vec::new();
            for a in it {
                // No flags here. `--workspace` used to be accepted, threaded a
                // bool through the whole call chain, and then always errored
                // "per-workspace config is added in a later task" — an accepted
                // flag with no reachable success path. Unknown flags are now
                // rejected rather than silently becoming a config *key*.
                if a.starts_with("--") {
                    bail!("unexpected argument: {a}\nusage: ws config set <key> <value>");
                }
                rest.push(a);
            }
            if rest.len() != 2 {
                bail!("usage: ws config set <key> <value>");
            }
            Ok(Cmd::Config(ConfigCmd::Set { key: rest[0].clone(), value: rest[1].clone() }))
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
        assert_eq!(
            p(&["api@retry", "--merge"]),
            Cmd::Worktree { spec: "api@retry".into(), merge: true }
        );
    }

    // ---- refocus: the new surface, and proof the old surface is gone ----

    /// `-task add` must work without a workspace name, because `/ws:task` calls
    /// it from inside a session that does not pass one. An unregistered first
    /// word is therefore the start of the task text, not a target.
    #[test]
    fn task_add_without_a_name_takes_everything_as_text() {
        assert_eq!(
            p(&["-task", "add", "write", "the", "docs"]),
            Cmd::Task(TaskCmd::Add { name: None, text: "write the docs".into() })
        );
    }

    #[test]
    fn task_add_requires_text() {
        assert!(super::parse(vec!["-task".into(), "add".into()]).is_err());
    }

    #[test]
    fn task_list_and_rm_parse() {
        assert_eq!(p(&["-task", "list"]), Cmd::Task(TaskCmd::List { name: None }));
        assert_eq!(
            p(&["-task", "list", "proj"]),
            Cmd::Task(TaskCmd::List { name: Some("proj".into()) })
        );
        assert_eq!(p(&["-task", "rm", "2"]), Cmd::Task(TaskCmd::Rm { name: None, index: 2 }));
    }

    #[test]
    fn task_rm_rejects_a_non_numeric_index() {
        let err = super::parse(vec!["-task".into(), "rm".into(), "second".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("must be a number"), "{err:#}");
    }

    #[test]
    fn an_unknown_task_subcommand_is_rejected() {
        assert!(super::parse(vec!["-task".into(), "drain".into()]).is_err());
    }

    #[test]
    fn rotate_parses_with_and_without_a_name() {
        assert_eq!(p(&["-rotate"]), Cmd::Rotate { name: None });
        assert_eq!(p(&["-rotate", "proj"]), Cmd::Rotate { name: Some("proj".into()) });
        assert!(super::parse(vec!["-rotate".into(), "a".into(), "b".into()]).is_err());
    }

    #[test]
    fn hooks_subcommands_parse() {
        assert_eq!(p(&["hooks", "list"]), Cmd::Hooks(HooksCmd::List));
        assert_eq!(p(&["hooks", "check"]), Cmd::Hooks(HooksCmd::Check));
        assert!(super::parse(vec!["hooks".into()]).is_err());
        assert!(super::parse(vec!["hooks".into(), "install".into()]).is_err());
    }

    #[test]
    fn pick_parses_explicitly() {
        assert_eq!(p(&["-pick"]), Cmd::Pick);
    }

    /// The destructive command must not turn a flag typo into a workspace name.
    /// `ws -rm --forec myws` used to try to delete a workspace literally called
    /// "--forec", report "no such workspace", exit 0, and leave myws untouched.
    #[test]
    fn rm_rejects_an_unknown_flag_instead_of_treating_it_as_a_name() {
        let err = super::parse(vec!["-rm".into(), "--forec".into(), "myws".into()]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--forec"), "the error must name the typo: {msg}");
        assert!(msg.contains("usage:"), "and show the usage: {msg}");
    }

    #[test]
    fn rm_still_accepts_names_and_force() {
        assert_eq!(
            p(&["-rm", "a", "b", "--force"]),
            Cmd::Rm { names: vec!["a".into(), "b".into()], force: true }
        );
    }

    /// Every dash-prefixed command this refocus removed must now be an error,
    /// not a silent no-op and not a stale alias.
    #[test]
    fn removed_dash_commands_are_rejected() {
        for argv in [
            vec!["-tui"],
            vec!["-msg", "proj", "hello"],
            vec!["-spawn", "proj"],
            vec!["-queue", "add", "proj", "x"],
            vec!["-queue", "drain", "proj"],
        ] {
            let owned: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            assert!(super::parse(owned).is_err(), "removed command {argv:?} must be rejected");
        }
    }

    /// The two removed *bare-word* commands cannot be rejected by the parser:
    /// every bare word is a workspace name, and the parser does not consult the
    /// registry to decide otherwise. They now parse as a launch and fail at
    /// runtime with "no such workspace", which is the honest outcome — pinned
    /// here so nobody later mistakes it for the alias still working.
    #[test]
    fn removed_bare_word_commands_parse_as_workspace_names() {
        match p(&["migrate-cs"]) {
            Cmd::Launch { name, .. } => assert_eq!(name, "migrate-cs"),
            other => panic!("expected a launch attempt, got {other:?}"),
        }
        match p(&["subagent-statusline"]) {
            Cmd::Launch { name, .. } => assert_eq!(name, "subagent-statusline"),
            other => panic!("expected a launch attempt, got {other:?}"),
        }
        // With their flags they are rejected outright, since a launch takes none.
        assert!(super::parse(vec!["migrate-cs".into(), "--all".into()]).is_err());
    }

    /// `config set --workspace` parsed, threaded a bool through the whole call
    /// chain, and then always errored "added in a later task". Gone.
    #[test]
    fn config_set_rejects_the_workspace_flag() {
        assert!(super::parse(vec![
            "config".into(),
            "set".into(),
            "--workspace".into(),
            "k".into(),
            "v".into()
        ])
        .is_err());
    }

    #[test]
    fn a_plain_name_still_launches() {
        assert_eq!(
            p(&["api"]),
            Cmd::Launch {
                name: "api".into(),
                agent: None,
                fresh: false,
                force: false,
                handoff: false
            }
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
            Cmd::Launch {
                name: "mywork".into(),
                agent: None,
                fresh: false,
                force: false,
                handoff: false
            }
        );
    }

    #[test]
    fn launch_flags() {
        assert_eq!(
            p(&["mywork", "--agent", "claude", "--fresh", "--force"]),
            Cmd::Launch {
                name: "mywork".into(),
                agent: Some("claude".into()),
                fresh: true,
                force: true,
                handoff: false
            }
        );
    }

    #[test]
    fn agent_shorthand_flags() {
        assert_eq!(p(&["proj", "-codex"]), p(&["proj", "--agent", "codex"]));
        assert_eq!(p(&["proj", "-claude"]), p(&["proj", "--agent", "claude"]));
    }

    #[test]
    fn agent_fresh_and_handoff_parse() {
        match p(&["proj", "-codex"]) {
            Cmd::Launch { name, agent, fresh, handoff, .. } => {
                assert_eq!(name, "proj");
                assert_eq!(agent.as_deref(), Some("codex"));
                assert!(!fresh, "resuming is the default");
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

    /// `-resume` parsed and did nothing — resuming is already the default, so the
    /// flag existed only to be mutually exclusive with `-fresh`. Accepting a
    /// no-op silently tells the user they opted into something they did not, so
    /// it is now an error that explains the default.
    #[test]
    fn resume_is_rejected_with_an_explanation_rather_than_silently_ignored() {
        for args in
            [vec!["proj", "-resume"], vec!["proj", "--resume"], vec!["proj", "-resume", "-codex"]]
        {
            let err = parse(args.iter().map(|x| x.to_string()).collect())
                .expect_err("-resume must not be silently accepted")
                .to_string();
            assert!(err.contains("default"), "the error must say resuming is the default: {err}");
            assert!(err.contains("--fresh"), "and point at the flag that does something: {err}");
        }
    }

    #[test]
    fn rm_collects_names_and_force() {
        assert_eq!(
            p(&["-rm", "a", "b", "--force"]),
            Cmd::Rm { names: vec!["a".into(), "b".into()], force: true }
        );
    }

    #[test]
    fn unknown_dash() {
        assert!(parse(vec!["-nope".into()]).is_err());
    }

    #[test]
    fn adopt_rejects_extra_args() {
        assert!(parse(vec!["-adopt".into(), "a".into(), "b".into()]).is_err());
    }

    #[test]
    fn parses_setup_and_internal() {
        assert_eq!(p(&["setup"]), Cmd::Setup);
        assert_eq!(p(&["internal", "session-start"]), Cmd::Internal(vec!["session-start".into()]));
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
    fn color_set_clear_and_validation() {
        assert_eq!(p(&["-color", "green"]), Cmd::Color { name: None, color: Some("green".into()) });
        assert_eq!(p(&["-color", "--clear"]), Cmd::Color { name: None, color: None });
        assert_eq!(
            p(&["-color", "--workspace", "proj", "cyan"]),
            Cmd::Color { name: Some("proj".into()), color: Some("cyan".into()) }
        );
        // Case is a typing convenience, not a different color.
        assert_eq!(p(&["-color", "GREEN"]), Cmd::Color { name: None, color: Some("green".into()) });
        // A name with no RGB behind it would silently produce an uncolored tab,
        // so it is rejected at the door rather than written to workspace.toml.
        assert!(parse(vec!["-color".into(), "chartreuse-plaid".into()]).is_err());
        assert!(parse(vec!["-color".into()]).is_err(), "no argument is ambiguous");
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
}
