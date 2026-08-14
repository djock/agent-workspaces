use anyhow::{bail, Result};
use std::io::IsTerminal;

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Launch {
        name: String,
        agent: Option<String>,
        fresh: bool,
        force: bool,
        handoff: bool,
    },
    List {
        tag: Option<String>,
        archived: bool,
    },
    Adopt {
        name: Option<String>,
    },
    Rm {
        names: Vec<String>,
        force: bool,
    },
    Config(ConfigCmd),
    Version,
    Help,
    /// `ws <verb> -h` — the lines of `ws -h` that document one verb.
    VerbHelp(String),
    Setup,
    Internal(Vec<String>),
    Statusline,
    Limits,
    Doctor,
    Secrets(SecretsCmd),
    Tag(TagCmd),
    Status {
        name: Option<String>,
        text: Option<String>,
    },
    Color {
        name: Option<String>,
        color: Option<String>,
    },
    Archive {
        names: Vec<String>,
        archived: bool,
    },
    Search {
        query: String,
        include_archived: bool,
    },
    Update {
        check: bool,
        force: bool,
    },
    Uninstall {
        force: bool,
    },
    Pick,
    Whoami,
    Who {
        name: Option<String>,
    },
    Conversations {
        name: Option<String>,
    },
    Rotate {
        name: Option<String>,
    },
    Task(TaskCmd),
    Msg(MsgCmd),
    Hooks(HooksCmd),
    Worktree {
        spec: String,
        merge: bool,
    },
    /// `ws <base> -features` — the base's feature worktrees and what merging
    /// each would do.
    Features {
        base: String,
        porcelain: bool,
    },
}

/// Task capture. `add` defaults to the current workspace so `/ws:task` can call
/// it without knowing where it is; an explicit name is still accepted.
#[derive(Debug, PartialEq)]
pub enum TaskCmd {
    Add { name: Option<String>, text: String },
    List { name: Option<String> },
    Rm { name: Option<String>, index: usize },
}

/// Messages between workspaces.
///
/// `Read` is the bare `ws -msg`: asking for your mail is the common case, and
/// making it the default means a session can be told "check your mail" without
/// anyone remembering a subcommand.
#[derive(Debug, PartialEq)]
pub enum MsgCmd {
    Send { to: String, body: Option<String>, kind: String, reply_to: Option<String> },
    Read,
    Log,
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

/// The full command surface.
///
/// Lives here rather than in `main.rs` because it is not only printed: `-h` on
/// any individual verb is answered from these same lines (`verb_usage`), so the
/// help a verb gives and the help `ws -h` gives cannot disagree.
pub fn help_text() -> &'static str {
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
         \x20                                (without --force you are offered the\n\
         \x20                                choice: open a feature, force, new, cancel)\n\
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
         \x20 ws -color <color> | --clear  set the tab and status-line color\n\
         \n\
         Worktrees\n\
         \x20 ws <base>@<feature>          create a git worktree workspace off <base>,\n\
         \x20                                or open it once it exists\n\
         \x20 ws <base>@<feature> --merge  merge it back (--no-ff) and remove it\n\
         \x20 ws <base> -features          list its feature worktrees and whether\n\
         \x20                                each can merge (--porcelain)\n\
         \n\
         Coordinate\n\
         \x20 ws -whoami                   print your actor slug\n\
         \x20 ws -who [<name>]             who did what in a workspace, from the timeline\n\
         \x20 ws -conversations [<name>]   conversation lineage: rotations and agent switches\n\
         \x20 ws -rotate [<name>]          write a handoff skeleton for the next session\n\
         \x20 ws -task add [<name>] <text> capture a task without interrupting the agent\n\
         \x20 ws -task list|rm [<name>]    show or drop captured tasks\n\
         \x20 ws -msg <name> \"<body>\"      send a message to another workspace\n\
         \x20                                (--kind task queues it there; `-` reads\n\
         \x20                                the body from stdin; --reply <thread>)\n\
         \x20 ws -msg | ws -msg log        read your unread mail, or the whole history\n\
         \n\
         Inspect\n\
         \x20 ws -limits                   usage limits captured from the status line\n\
         \x20 ws -doctor                   check agents, hooks and shims\n\
         \n\
         Secrets\n\
         \x20 ws -secrets set|get|rm <name>\n\
         \x20 ws -secrets list|purge|export|backend\n\
         \x20 ws -secrets restore <file>   put stored values back into a redacted file\n\
         \x20 ws -secrets help             the subcommands, in full\n\
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

/// The lines of `ws -h` that document one verb.
///
/// Derived rather than written a second time. A verb's usage existed only inside
/// its own parser's `bail!` strings, which meant asking a verb for help got the
/// flag treated as data — `ws -tag --help` answered `unknown -tag subcommand:
/// --help`, pointing the reader at a subcommand problem when they had asked for
/// documentation. Deriving it also means the two surfaces cannot drift: a verb
/// whose help text changes changes here too, and `every_verb_answers_help` fails
/// if a verb appears in the parser with no line in the help at all.
///
/// A line is a match when the verb appears as a whole token in it, so
/// `ws -list | -ls` answers to both spellings and `ws -archive | -unarchive`
/// answers to either. Continuation lines (the wrapped half of a two-line entry)
/// carry no `ws ` of their own and are taken along with the entry above them.
pub fn verb_usage(verb: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut taking = false;
    for line in help_text().lines() {
        if line.trim().is_empty() {
            taking = false;
            continue;
        }
        if line.starts_with("  ws ") {
            taking = line_documents(line, verb);
        }
        if taking {
            out.push(line);
        }
    }
    if out.is_empty() {
        // Every verb should have a line — `help_covers_every_command` enforces
        // it — but printing the whole help is a better answer than printing
        // nothing if one ever slips through.
        return help_text().to_string();
    }
    out.join("\n")
}

/// Every verb `ws -h` documents, in the order it lists them.
///
/// One source for the vocabulary. cs shipped three copies of its own — two
/// completion scripts and an error message — each missing something different,
/// which is what a hand-maintained list does over time.
pub fn known_verbs() -> Vec<&'static str> {
    let mut out = Vec::new();
    for line in help_text().lines() {
        for verb in leading_verbs(line) {
            // `<name>` and `<base>@<feature>` are placeholders for what the user
            // types, not things to suggest typing.
            if !verb.starts_with('<') && !out.contains(&verb) {
                out.push(verb);
            }
        }
    }
    out
}

/// The verb (or alternative spellings of it) a help line begins with.
///
/// Only the *leading* position, and only alternatives attached to it: `ws -list
/// | -ls` gives both spellings of one verb, while `ws <name> -claude | -codex`
/// gives `<name>` alone — the `| -codex` there alternates between two flags of
/// the launch verb, not between two verbs. Reading every `|` in the line made
/// `-codex` and `--clear` show up in the list of commands to try.
fn leading_verbs(line: &str) -> Vec<&str> {
    if !line.starts_with("  ws ") {
        return Vec::new();
    }
    // The description is separated from the command by a run of spaces; a line
    // with no description is all command.
    let cmd = line.trim_start();
    let cmd = match cmd.find("  ") {
        Some(i) => &cmd[..i],
        None => cmd,
    };
    let Some(rest) = cmd.strip_prefix("ws ") else { return Vec::new() };

    let mut toks = rest.split_whitespace();
    let mut out = Vec::new();
    if let Some(first) = toks.next() {
        out.push(first);
    }
    while toks.next() == Some("|") {
        match toks.next() {
            Some(alt) => out.push(alt),
            None => break,
        }
    }
    out
}

/// What to suggest after an unknown command.
///
/// Anything sharing a prefix with what was typed leads, since a typo is far
/// likelier than a wholesale invention; the full vocabulary follows either way,
/// because a user who typed something unrecognisable is exactly the one who
/// needs to see what exists.
fn suggestions(typed: &str) -> String {
    let stem = typed.trim_start_matches('-');
    let verbs = known_verbs();
    let near: Vec<&str> = verbs
        .iter()
        .copied()
        .filter(|v| {
            let vs = v.trim_start_matches('-');
            !stem.is_empty() && (vs.starts_with(stem) || stem.starts_with(vs))
        })
        .collect();

    let mut out = String::new();
    if !near.is_empty() {
        out.push_str(&format!("did you mean: {}\n\n", near.join(" | ")));
    }
    out.push_str(&format!("commands: {}\nws -h for the full surface", verbs.join(" ")));
    out
}

/// Does this help line document `verb`?
///
/// Only the verb the line leads with — see [`leading_verbs`]. Matching anywhere
/// in the line made `ws myproj --help` print the secrets line too, since
/// `ws -secrets set|get|rm <name>` mentions `<name>` in its arguments.
fn line_documents(line: &str, verb: &str) -> bool {
    leading_verbs(line).contains(&verb)
}

/// Is this verb being asked for its own documentation?
///
/// Only the **first** token after the verb counts. Scanning the whole argument
/// list would make `ws -task add "fix --help handling"` print help instead of
/// queueing the task — an argument that merely contains the word is data, not a
/// request.
fn asks_for_help(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("-h") | Some("--help"))
}

/// Parse argv (excluding the program name) into a Cmd.
/// Top-level pseudo-subcommands use a single leading dash (`-list`) to match
/// the spec's CLI surface; `config` is a bare-word subcommand.
pub fn parse(args: Vec<String>) -> Result<Cmd> {
    // Asking any verb for help is answered here, ahead of that verb's own
    // argument parsing and before any workspace name is resolved. Downstream,
    // `--help` is just another token: it became a subcommand to reject, a
    // workspace name to look up, or a status string to store.
    //
    // `-secrets` is exempt and forwards as before. It is a delegation to eleven
    // subcommands of its own, with a reference (`SECRETS_USAGE`) that names all
    // of them; answering it from `ws -h`'s four secrets lines would be a
    // downgrade, and it already handles `--help` itself.
    if let (Some(verb), rest) = (args.first(), &args[1.min(args.len())..]) {
        if verb != "-secrets" && asks_for_help(rest) {
            // A bare word that is not one of ws's own subcommands is a workspace
            // name, and `ws myproj --help` is a request for the launch flags —
            // which the help documents against `ws <name>`, not against the name
            // the user happened to type.
            let key = match verb.as_str() {
                v if v.starts_with('-') => v.to_string(),
                v @ ("hooks" | "config" | "setup" | "statusline" | "internal") => v.to_string(),
                v if crate::worktree::parse_name(v).is_some() => "<base>@<feature>".to_string(),
                _ => "<name>".to_string(),
            };
            return Ok(Cmd::VerbHelp(key));
        }
    }

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
        "-msg" => parse_msg(it.collect()),
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
            // Derived from the help text, not written out again here. The list
            // this replaces named five of eighteen verbs and had no way to learn
            // about a nineteenth: a vocabulary kept in more than one place falls
            // behind in exactly the copy nobody is looking at. `ws -h` is the
            // one that has a test keeping it complete, so it is the one to
            // quote. Anything close to what was typed comes first.
            bail!("unknown command: {other}\n\n{}", suggestions(other));
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
            let mut features = false;
            let mut porcelain = false;
            while let Some(a) = it.next() {
                match a.as_str() {
                    // Not a launch at all: `-features` asks about a workspace
                    // rather than opening it. It lives in this arm because the
                    // thing it asks about is named the same way a launch names
                    // it — `ws <base> -features`, not `ws -features <base>`.
                    "-features" => features = true,
                    "--porcelain" => porcelain = true,
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
            if features {
                return Ok(Cmd::Features { base: name.to_string(), porcelain });
            }
            if porcelain {
                bail!("--porcelain only applies to `ws <base> -features`");
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
/// `ws -msg [<workspace> [<body>]] [--kind text|task] [--reply <thread>]`
///
/// A bare `ws -msg` reads; a target makes it a send. `-` as the body reads it
/// from stdin, which is what makes a multi-KB handoff practical: a body that
/// size does not belong in argv, where it is visible to every `ps` on the
/// machine and capped by the platform.
fn parse_msg(args: Vec<String>) -> Result<Cmd> {
    let mut positional: Vec<String> = Vec::new();
    let mut kind = "text".to_string();
    let mut reply_to = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--kind" => kind = it.next().ok_or_else(|| anyhow::anyhow!("--kind needs a value"))?,
            "--reply" => {
                reply_to =
                    Some(it.next().ok_or_else(|| anyhow::anyhow!("--reply needs a thread id"))?)
            }
            other if other.starts_with("--") => bail!("unexpected argument: {other}"),
            _ => positional.push(a),
        }
    }
    match positional.len() {
        0 => Ok(Cmd::Msg(MsgCmd::Read)),
        _ if positional[0] == "log" && positional.len() == 1 => Ok(Cmd::Msg(MsgCmd::Log)),
        1 => Ok(Cmd::Msg(MsgCmd::Send { to: positional.remove(0), body: None, kind, reply_to })),
        2 => {
            let body = positional.pop();
            Ok(Cmd::Msg(MsgCmd::Send { to: positional.remove(0), body, kind, reply_to }))
        }
        _ => bail!("usage: ws -msg <workspace> \"<body>\" [--kind text|task] [--reply <thread>]"),
    }
}

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
