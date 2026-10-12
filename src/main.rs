mod agent;
mod app;
mod bindings;
mod cli;
mod client;
mod clipboard;
mod config;
mod fdpass;
mod filetree;
mod graphics;
mod keys;
mod layout;
mod menu;
mod mouse;
mod pane;
mod persist;
mod protocol;
mod release;
mod render;
mod server;
mod sidebar;
mod theme;
mod upgrade;
mod workspace;

use anyhow::{Result, bail};

const USAGE: &str = "\
Usage: imkr [-s NAME] [command]

-s NAME picks a session other than `default`, e.g. `imkr -s work`.

Commands:
  (none)        attach to the session, or start it
  attach [-d]   attach to the session (`attach -t NAME` works too), -d detaches
                other terminals attached to it
  --host HOST   with or without attach: the session on HOST, through ssh. imkr
                runs there, this terminal stays the client (keys, clipboard,
                images). E.g. `imkr --host server -s work`
  proxy [--start]
                connect stdin/stdout to the session, used by --host
  ls [--json]   list the running sessions
  hive ls       list the hives: saved groups of sessions
  hive save [NAME]
                save every running session as a hive (default `last`)
  hive restore [NAME] [--from HOST] [--take MACHINE]
                bring a hive back on every machine in it: missing sessions
                start, machines that are off show as offline. --from fetches
                it first (a new machine), --take restores that machine's
                sessions here (one that replaces it)
  hosts [add NAME | rm NAME]
                the other machines shown, from the settings and remembered
  hive rm NAME  delete a hive, running sessions keep running
  hive rename OLD NEW
                rename a hive
  save, restore short for `hive save` and `hive restore`
  rename-session NAME
                rename the session (`-s OLD rename-session NEW` for another)
  kill-server   shut down the session's server and every pane in it
  update        move the running server to this binary, e.g. after
                `cargo install`; panes, programs and screens stay
  keys          list the key bindings
  upgrade       download the latest release, replace this binary and
                move a running session to it
  version       print the version

For scripts and agents:
  list [--json]                         list the panes with program and agent state
  status <working|blocked|idle|clear>   report this pane's agent state (for hooks)
  send [--pane N] [--no-enter] <text>   type text into a pane
  notify [--title T] [--pane N] <text>  show a notification (desktop, terminal or
                                        off, as set in the settings)
  new [--float] [--window N] [-- command args...]
                                        start a command in a new pane
  rename [--pane N | --window N] [--clear | <name>]
                                        name a pane or a window
  move-window [--window N] <session>    move a window with its running panes to
                                        another session, started if needed
  popup [--name N] [--title T] [-- command args...]
                                        show or hide a popup running the command,
                                        started the first time; Ctrl+B Space too
  read [--pane N] [--lines N]           print a pane's screen, or its last N lines
  wait [--pane N] --until <state> [--timeout S]
                                        wait for an agent: working, blocked, idle,
                                        done, or ready (idle or done)
  hooks [--install]                     print the Claude Code hooks for status, or
                                        install them into ~/.claude
  help          show this help

Inside imkr, Ctrl+B then q opens the quit menu: detach, close the pane or end the
session. Ctrl+B then d detaches right away, the shells keep running.";

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--host HOST`: the session on that machine, through ssh.
    let host = match args.iter().position(|a| a == "--host" || a == "-H") {
        Some(i) => {
            let Some(host) = args.get(i + 1).cloned() else {
                bail!("{} needs a host", args[i]);
            };
            args.drain(i..=i + 1);
            Some(host)
        }
        None => None,
    };
    let mut session = None;
    // `-s NAME` anywhere, or `attach -t NAME` as in tmux, picks the session.
    if let Some(i) = args.iter().position(|a| {
        a == "-s"
            || a == "--session"
            || (a == "-t" && args.first().is_some_and(|c| c == "attach" || c == "a"))
    }) {
        let Some(name) = args.get(i + 1).cloned() else {
            bail!("{} needs a session name", args[i]);
        };
        if !protocol::valid_session_name(&name) {
            bail!("{name:?} is no session name: letters, digits, - and _ only");
        }
        args.drain(i..=i + 1);
        session = Some(name.clone());
        // SAFETY: nothing else runs yet, no thread can read the
        // environment while it changes.
        unsafe {
            std::env::set_var("IMKR_SESSION", &name);
            std::env::remove_var("IMKR_SOCKET");
        }
    }
    // Plain `imkr` goes where it was last; a session or host given wins.
    let last = session.is_none() && host.is_none() && std::env::var_os("IMKR_SOCKET").is_none();
    let remote = host.map(|host| client::Remote { host, session });
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => client::run(true, false, remote, last),
        ["attach" | "a"] => client::run(false, false, remote, false),
        ["attach" | "a", "-d"] => client::run(false, true, remote, false),
        ["proxy"] => client::proxy(false),
        ["proxy", "--start"] => client::proxy(true),
        _ if remote.is_some() => bail!("--host works with attach only, run other commands there"),
        ["kill-server"] => client::kill_server(),
        ["ls", ..] => cli::ls(&args[1..]),
        ["keys"] => {
            print!("{}", bindings::reference());
            Ok(())
        }
        ["server"] => server::run(),
        ["status", ..] => cli::status(&args[1..]),
        ["list", ..] => cli::list(&args[1..]),
        ["send", ..] => cli::send(&args[1..]),
        ["notify", ..] => cli::notify(&args[1..]),
        ["new", ..] => cli::new(&args[1..]),
        ["rename", ..] => cli::rename(&args[1..]),
        ["move-window", ..] => cli::move_window(&args[1..]),
        ["popup", ..] => cli::popup(&args[1..]),
        ["hosts", ..] => cli::hosts(&args[1..]),
        ["read", ..] => cli::read(&args[1..]),
        ["wait", ..] => cli::wait(&args[1..]),
        ["hooks", ..] => cli::hooks(&args[1..]),
        ["update"] => cli::update(),
        ["upgrade"] => release::upgrade(),
        ["hive", ..] => cli::hive(&args[1..]),
        ["save", ..] => cli::save(&args[1..]),
        ["restore", ..] => cli::restore(&args[1..]),
        ["rename-session", ..] => cli::rename_session(&args[1..]),
        ["version" | "-V" | "--version"] => {
            println!("imkr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["help" | "-h" | "--help"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("unknown arguments: {}\n\n{USAGE}", args.join(" ")),
    }
}
