//! Tab completion for imkr in zsh and bash: `imkr completions zsh|bash`
//! prints a script to eval in the shell's rc file. Commands, their
//! subcommands and options come from `COMMANDS`; names that change (sessions,
//! hosts, hives, …) are asked for when Tab is pressed, with `imkr complete
//! KIND`, so they are always the current ones.

use anyhow::{Result, bail};

/// What the words after a command (or subcommand) are.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Words {
    Nothing,
    Sessions,
    Hosts,
    Hives,
    Templates,
    Accounts,
    Plugins,
    Layouts,
    Contacts,
    Inbox,
    Addresses,
    Files,
    Shells,
    Tools,
}

impl Words {
    /// The KIND of `imkr complete KIND`, or a shell's own completion.
    fn kind(self) -> Option<&'static str> {
        Some(match self {
            Words::Nothing | Words::Files | Words::Shells | Words::Tools => return None,
            Words::Sessions => "sessions",
            Words::Hosts => "hosts",
            Words::Hives => "hives",
            Words::Templates => "templates",
            Words::Accounts => "accounts",
            Words::Plugins => "plugins",
            Words::Layouts => "layouts",
            Words::Contacts => "contacts",
            Words::Inbox => "inbox",
            Words::Addresses => "addresses",
        })
    }
}

struct Sub {
    name: &'static str,
    words: Words,
}

struct Command {
    name: &'static str,
    about: &'static str,
    options: &'static [&'static str],
    words: Words,
    subs: &'static [Sub],
}

const fn sub(name: &'static str, words: Words) -> Sub {
    Sub { name, words }
}

const fn cmd(
    name: &'static str,
    about: &'static str,
    options: &'static [&'static str],
    words: Words,
) -> Command {
    Command {
        name,
        about,
        options,
        words,
        subs: &[],
    }
}

const fn with_subs(
    name: &'static str,
    about: &'static str,
    options: &'static [&'static str],
    subs: &'static [Sub],
) -> Command {
    Command {
        name,
        about,
        options,
        words: Words::Nothing,
        subs,
    }
}

use Words::*;

/// Every command a person types, in the order of `imkr help`. The ones only
/// imkr itself calls (proxy, server, hosted, attach-pane, dirs, complete,
/// status) are left out.
const COMMANDS: &[Command] = &[
    cmd("attach", "attach to the session", &["-d", "-t"], Nothing),
    cmd("ls", "the running sessions", &["--json"], Nothing),
    with_subs(
        "hive",
        "saved groups of sessions",
        &["--from", "--take", "--yes"],
        &[
            sub("ls", Nothing),
            sub("save", Hives),
            sub("restore", Hives),
            sub("switch", Hives),
            sub("rm", Hives),
            sub("rename", Hives),
        ],
    ),
    cmd("save", "save every running session as a hive", &[], Hives),
    cmd("restore", "bring a hive back", &["--from", "--take"], Hives),
    cmd("plugins", "the plugins and broken files", &[], Nothing),
    with_subs(
        "plugin",
        "add, update or remove plugins",
        &["--yes"],
        &[
            sub("add", Nothing),
            sub("update", Plugins),
            sub("rm", Plugins),
            sub("search", Nothing),
            sub("market", Nothing),
        ],
    ),
    with_subs(
        "account",
        "several Claude Code and Codex accounts",
        &["--url", "--model", "--token-stdin", "--pane"],
        &[
            sub("ls", Nothing),
            sub("add", Tools),
            sub("login", Accounts),
            sub("rm", Accounts),
            sub("sync", Nothing),
            sub("env", Accounts),
            sub("run", Accounts),
            sub("use", Accounts),
            sub("switch", Accounts),
            sub("default", Accounts),
            sub("usage", Nothing),
        ],
    ),
    cmd(
        "revive",
        "bring back a stopped or closed session",
        &[],
        Sessions,
    ),
    cmd(
        "forget",
        "forget a stopped or closed session",
        &[],
        Sessions,
    ),
    with_subs(
        "template",
        "session templates",
        &[],
        &[
            sub("ls", Nothing),
            sub("save", Templates),
            sub("start", Templates),
        ],
    ),
    with_subs(
        "hosts",
        "the other machines",
        &[],
        &[
            sub("add", Hosts),
            sub("rm", Hosts),
            sub("sync", Nothing),
            sub("tailnet", Nothing),
        ],
    ),
    with_subs(
        "settings",
        "settings with the other machines",
        &["--yes"],
        &[sub("sync", Hosts)],
    ),
    with_subs(
        "web",
        "sessions and agents on the phone",
        &["--listen"],
        &[
            sub("pair", Nothing),
            sub("devices", Nothing),
            sub("forget", Nothing),
        ],
    ),
    cmd("rename-session", "rename the session", &[], Sessions),
    cmd(
        "kill-server",
        "end the session and every pane in it",
        &[],
        Nothing,
    ),
    cmd(
        "update",
        "move running sessions to this binary",
        &[],
        Nothing,
    ),
    cmd("keys", "the key bindings", &[], Nothing),
    cmd(
        "upgrade",
        "download the latest release and move to it",
        &[],
        Nothing,
    ),
    cmd("version", "the version", &[], Nothing),
    cmd(
        "list",
        "the panes with program and agent state",
        &["--json"],
        Nothing,
    ),
    cmd(
        "send",
        "type text into a pane",
        &["--pane", "--no-enter", "--stdin"],
        Nothing,
    ),
    cmd(
        "checkpoints",
        "the agent checkpoints of this repository",
        &[],
        Nothing,
    ),
    cmd(
        "run",
        "run a command in a pane's shell",
        &["--pane", "--timeout"],
        Nothing,
    ),
    cmd(
        "wait",
        "wait for a command or an agent",
        &["--pane", "--until", "--timeout"],
        Nothing,
    ),
    cmd(
        "shell-integration",
        "what to eval for run and wait",
        &[],
        Shells,
    ),
    cmd(
        "completions",
        "what to eval for Tab completion",
        &[],
        Shells,
    ),
    cmd("mcp", "imkr's commands as MCP tools", &[], Nothing),
    // `imkr layout NAME` gives the window a layout: names next to ls and save.
    Command {
        name: "layout",
        about: "window layouts",
        options: &["--window"],
        words: Layouts,
        subs: &[sub("ls", Nothing), sub("save", Layouts)],
    },
    cmd(
        "explain",
        "why a pane counts as the agent it is",
        &["--pane", "--json"],
        Nothing,
    ),
    Command {
        name: "archive",
        about: "ended sessions older than the ten in `last`",
        options: &[],
        words: Nothing,
        subs: &[
            sub("ls", Nothing),
            sub("export", Nothing),
            sub("drop", Nothing),
        ],
    },
    cmd(
        "operator",
        "the machine the user sits at",
        &["--json"],
        Nothing,
    ),
    cmd("cp", "copy files, also to and from panes", &[], Files),
    cmd(
        "fleet",
        "several agents on one prompt",
        &["-n", "--agent", "--diff"],
        Nothing,
    ),
    cmd(
        "worktree",
        "a window in a new git worktree",
        &["ls", "merge", "rm", "--force", "--json"],
        Nothing,
    ),
    cmd(
        "subscribe",
        "what changes, as JSON lines",
        &["--snapshot"],
        Nothing,
    ),
    cmd(
        "agents",
        "every agent with its address",
        &["--all", "--json"],
        Nothing,
    ),
    cmd("msg", "message an agent", &["--wait", "--from"], Addresses),
    cmd("push", "send a test push to the phone", &["test"], Nothing),
    cmd(
        "skill",
        "the guide for agents driving imkr",
        &["--install"],
        Nothing,
    ),
    cmd(
        "notify",
        "show a notification",
        &["--title", "--pane"],
        Nothing,
    ),
    cmd(
        "new",
        "start a command in a new pane",
        &["--float", "--window", "--dir", "--focus", "--name"],
        Nothing,
    ),
    cmd(
        "rename",
        "name a pane or a window",
        &["--pane", "--window", "--clear"],
        Nothing,
    ),
    cmd(
        "move-window",
        "move a window to another session",
        &["--window"],
        Sessions,
    ),
    cmd(
        "popup",
        "show or hide a popup",
        &["--name", "--title"],
        Nothing,
    ),
    with_subs(
        "connect",
        "other people's imkrs",
        &["--as", "--name"],
        &[
            sub("invite", Nothing),
            sub("join", Nothing),
            sub("brain", Nothing),
            sub("fetch", Nothing),
            sub("inbox", Nothing),
            sub("accept", Inbox),
            sub("decline", Inbox),
            sub("trust", Contacts),
            sub("untrust", Contacts),
            sub("rm", Contacts),
            sub("ls", Nothing),
        ],
    ),
    cmd(
        "note",
        "put a thought on a pane's notepad",
        &["--pane"],
        Nothing,
    ),
    cmd(
        "outbox",
        "messages waiting for machines out of reach",
        &[],
        Nothing,
    ),
    cmd(
        "read",
        "print a pane's screen",
        &["--pane", "--lines"],
        Addresses,
    ),
    cmd(
        "hooks",
        "the Claude Code hooks for agent states",
        &["--install"],
        Nothing,
    ),
    cmd("help", "show the help", &[], Nothing),
];

/// Options before the command, and the words their values complete with.
const GLOBAL: &[(&str, Words)] = &[("-s", Sessions), ("--host", Hosts)];

/// `imkr completions zsh|bash`.
pub fn script(args: &[String]) -> Result<()> {
    match args {
        [shell] if shell == "zsh" => print!("{}", zsh()),
        [shell] if shell == "bash" => print!("{}", bash()),
        _ => bail!(
            "usage: imkr completions zsh|bash, e.g. in ~/.zshrc:\n  eval \"$(imkr completions zsh)\""
        ),
    }
    Ok(())
}

/// `imkr complete KIND`: the current names of one kind, one per line, for the
/// completion scripts. Quiet when there are none or something fails: Tab
/// should never print errors into the line being typed.
pub fn complete(args: &[String]) -> Result<()> {
    let names: Vec<String> = match args.first().map(String::as_str) {
        Some("sessions") => crate::client::local_sessions()
            .into_iter()
            .map(|s| s.name)
            .collect(),
        Some("hosts") => crate::client::known_hosts(),
        Some("hives") => crate::persist::saves(),
        Some("templates") => crate::template::load()
            .0
            .into_iter()
            .map(|t| t.name)
            .collect(),
        Some("accounts") => crate::account::Store::here()
            .map(|store| {
                crate::account::TOOLS
                    .iter()
                    .flat_map(|tool| store.of_tool(tool))
                    .flat_map(|a| {
                        // A subscription also by its login (`stelljes`).
                        let shown = a.url.is_none().then(|| crate::account::shown(&store, &a));
                        std::iter::once(a.name).chain(shown)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("plugins") => crate::plugin::load()
            .0
            .into_iter()
            .map(|p| p.name)
            .collect(),
        Some("layouts") => crate::shapes::PRESETS
            .iter()
            .map(|(name, _)| name.to_string())
            .chain(crate::shapes::saved().into_keys())
            .collect(),
        Some("contacts") => crate::contacts::load()
            .into_iter()
            .map(|c| c.name)
            .collect(),
        Some("inbox") => crate::contacts::waiting()
            .into_iter()
            .map(|m| m.id.to_string())
            .collect(),
        Some("addresses") => addresses(),
        _ => Vec::new(),
    };
    let mut names = names;
    names.sort();
    names.dedup();
    for name in names {
        println!("{name}");
    }
    Ok(())
}

/// Agent addresses as `imkr msg` takes them: the agents of the sessions here
/// (`session/pane`), and the contacts (`NAME:`) to go on typing after.
fn addresses() -> Vec<String> {
    let mut out = Vec::new();
    for session in crate::client::local_sessions() {
        if session.dormant.is_some() {
            continue;
        }
        for pane in &session.agent_panes {
            out.push(format!("{}/{}", session.name, pane.pane));
        }
    }
    for contact in crate::contacts::load() {
        out.push(format!("{}:", contact.name));
    }
    out
}

/// The shell code that offers `words`: `imkr complete KIND`, or the shell's
/// own for files and fixed lists.
fn zsh_offer(words: Words) -> String {
    match words {
        Words::Files => "_files".into(),
        Words::Shells => "compadd zsh bash".into(),
        Words::Tools => "compadd claude codex".into(),
        Words::Nothing => String::new(),
        other => format!(
            "compadd -- ${{(f)\"$(imkr complete {} 2>/dev/null)\"}}",
            other.kind().unwrap_or_default()
        ),
    }
}

fn bash_offer(words: Words) -> String {
    match words {
        Words::Files => "COMPREPLY=($(compgen -f -- \"$cur\"))".into(),
        Words::Shells => "COMPREPLY=($(compgen -W 'zsh bash' -- \"$cur\"))".into(),
        Words::Tools => "COMPREPLY=($(compgen -W 'claude codex' -- \"$cur\"))".into(),
        Words::Nothing => String::new(),
        other => format!(
            "COMPREPLY=($(compgen -W \"$(imkr complete {} 2>/dev/null)\" -- \"$cur\"))",
            other.kind().unwrap_or_default()
        ),
    }
}

/// `:` (do nothing) where the shell needs a command and there is none.
fn if_empty(code: String) -> String {
    if code.is_empty() { ":".into() } else { code }
}

fn quote_zsh(text: &str) -> String {
    text.replace('\'', "'\\''").replace(':', "\\:")
}

fn zsh() -> String {
    let mut out = String::from(
        "# imkr completion for zsh: eval \"$(imkr completions zsh)\" in ~/.zshrc\n\
         _imkr() {\n\
         \x20 local prev=${words[CURRENT-1]} cur=${words[CURRENT]}\n\
         \x20 case $prev in\n",
    );
    for (option, words) in GLOBAL {
        out.push_str(&format!("    {option}) {}; return;;\n", zsh_offer(*words)));
    }
    out.push_str(
        "    -t) compadd -- ${(f)\"$(imkr complete sessions 2>/dev/null)\"}; return;;\n\
         \x20   --dir) _directories; return;;\n\
         \x20   --window|--pane|--lines|--timeout|--title|--name|--listen|--url|--model|--token|-n|--agent|--from|--take|--as|--until) return;;\n\
         \x20 esac\n\
         \x20 local i=2 command=\n\
         \x20 while (( i < CURRENT )); do\n\
         \x20   case ${words[i]} in\n\
         \x20     -s|--host|-t) (( i += 2 ));;\n\
         \x20     -*) (( i++ ));;\n\
         \x20     *) command=${words[i]}; break;;\n\
         \x20   esac\n\
         \x20 done\n\
         \x20 if [[ -z $command ]]; then\n\
         \x20   if [[ $cur == -* ]]; then compadd -- -s --host; return; fi\n\
         \x20   local -a commands=(\n",
    );
    for command in COMMANDS {
        out.push_str(&format!(
            "      '{}:{}'\n",
            command.name,
            quote_zsh(command.about)
        ));
    }
    out.push_str(
        "    )\n\
         \x20   _describe 'imkr command' commands\n\
         \x20   return\n\
         \x20 fi\n\
         \x20 local sub=${words[i+1]}\n\
         \x20 case $command in\n",
    );
    for command in COMMANDS {
        let options = command.options.join(" ");
        out.push_str(&format!("    {})\n", command.name));
        if !options.is_empty() {
            out.push_str(&format!(
                "      if [[ $cur == -* ]]; then compadd -- {options}; return; fi\n"
            ));
        }
        if command.subs.is_empty() {
            let offer = zsh_offer(command.words);
            if !offer.is_empty() {
                out.push_str(&format!("      {offer}\n"));
            }
        } else {
            let names: Vec<&str> = command.subs.iter().map(|s| s.name).collect();
            out.push_str(&format!(
                "      if (( CURRENT == i + 1 )); then compadd -- {}; {}; return; fi\n      case $sub in\n",
                names.join(" "),
                if_empty(zsh_offer(command.words))
            ));
            for sub in command.subs {
                let offer = zsh_offer(sub.words);
                if !offer.is_empty() {
                    out.push_str(&format!("        {}) {offer};;\n", sub.name));
                }
            }
            out.push_str("      esac\n");
        }
        out.push_str("      ;;\n");
    }
    out.push_str("  esac\n}\ncompdef _imkr imkr\n");
    out
}

fn bash() -> String {
    let mut out = String::from(
        "# imkr completion for bash: eval \"$(imkr completions bash)\" in ~/.bashrc\n\
         _imkr() {\n\
         \x20 local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]}\n\
         \x20 COMPREPLY=()\n\
         \x20 case $prev in\n",
    );
    for (option, words) in GLOBAL {
        out.push_str(&format!("    {option}) {}; return;;\n", bash_offer(*words)));
    }
    out.push_str(
        "    -t) COMPREPLY=($(compgen -W \"$(imkr complete sessions 2>/dev/null)\" -- \"$cur\")); return;;\n\
         \x20   --dir) COMPREPLY=($(compgen -d -- \"$cur\")); return;;\n\
         \x20   --window|--pane|--lines|--timeout|--title|--name|--listen|--url|--model|--token|-n|--agent|--from|--take|--as|--until) return;;\n\
         \x20 esac\n\
         \x20 local i=1 command=\n\
         \x20 while (( i < COMP_CWORD )); do\n\
         \x20   case ${COMP_WORDS[i]} in\n\
         \x20     -s|--host|-t) (( i += 2 ));;\n\
         \x20     -*) (( i++ ));;\n\
         \x20     *) command=${COMP_WORDS[i]}; break;;\n\
         \x20   esac\n\
         \x20 done\n\
         \x20 if [[ -z $command ]]; then\n\
         \x20   if [[ $cur == -* ]]; then COMPREPLY=($(compgen -W '-s --host' -- \"$cur\")); return; fi\n",
    );
    let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
    out.push_str(&format!(
        "    COMPREPLY=($(compgen -W '{}' -- \"$cur\"))\n    return\n  fi\n  local sub=${{COMP_WORDS[i+1]}}\n  case $command in\n",
        names.join(" ")
    ));
    for command in COMMANDS {
        let options = command.options.join(" ");
        out.push_str(&format!("    {})\n", command.name));
        if !options.is_empty() {
            out.push_str(&format!(
                "      if [[ $cur == -* ]]; then COMPREPLY=($(compgen -W '{options}' -- \"$cur\")); return; fi\n"
            ));
        }
        if command.subs.is_empty() {
            let offer = bash_offer(command.words);
            if !offer.is_empty() {
                out.push_str(&format!("      {offer}\n"));
            }
        } else {
            let names: Vec<&str> = command.subs.iter().map(|s| s.name).collect();
            out.push_str(&format!(
                "      if (( COMP_CWORD == i + 1 )); then COMPREPLY=($(compgen -W '{}' -- \"$cur\")); {}; return; fi\n      case $sub in\n",
                names.join(" "),
                if_empty(bash_offer(command.words).replace("COMPREPLY=(", "COMPREPLY+=("))
            ));
            for sub in command.subs {
                let offer = bash_offer(sub.words);
                if !offer.is_empty() {
                    out.push_str(&format!("        {}) {offer};;\n", sub.name));
                }
            }
            out.push_str("      esac\n");
        }
        out.push_str("      ;;\n");
    }
    out.push_str("  esac\n}\ncomplete -F _imkr imkr\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Commands only imkr itself calls, not offered on Tab.
    const INTERNAL: &[&str] = &[
        "proxy",
        "server",
        "hosted",
        "attach-pane",
        "dirs",
        "complete",
        "status",
        "--skill",
    ];

    #[test]
    fn every_command_is_offered() {
        let main = include_str!("main.rs");
        let mut missing = Vec::new();
        for line in main.lines() {
            let line = line.trim_start();
            let Some(rest) = line.strip_prefix("[\"") else {
                continue;
            };
            let Some(end) = rest.find('"') else { continue };
            let name = &rest[..end];
            if INTERNAL.contains(&name) || COMMANDS.iter().any(|c| c.name == name) {
                continue;
            }
            missing.push(name.to_owned());
        }
        assert!(
            missing.is_empty(),
            "not in completions::COMMANDS: {missing:?}"
        );
    }

    #[test]
    fn scripts_are_valid_shell() {
        for (shell, text) in [("zsh", zsh()), ("bash", bash())] {
            if std::process::Command::new(shell)
                .arg("--version")
                .output()
                .is_err()
            {
                continue;
            }
            let out = std::process::Command::new(shell)
                .args(["-n", "-c", &text])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
