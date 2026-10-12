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
mod plugin;
mod protocol;
mod release;
mod render;
mod server;
mod sidebar;
mod template;
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
  hive rm NAME  delete a hive, running sessions keep running
  hive rename OLD NEW
                rename a hive
  save, restore short for `hive save` and `hive restore`
  plugins       list the plugins (~/.config/imkr/plugins/*.toml) and broken files
  revive NAME   bring back a session that stopped (reboot, crash) or was
                closed, without attaching; `ls` lists them with ◌
  forget NAME   forget such a session: its saved layout is deleted
  template ls   list the session templates (~/.config/imkr/templates/*.toml)
  template save NAME
                save this session's windows, splits and agents as template NAME
  template start NAME [SESSION]
                start session SESSION (default: the template's name) from it
  hosts [add NAME | rm NAME]
                the other machines shown, from the settings and remembered
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
  send [--pane N] [--no-enter] [--stdin] <text>   type text into a pane
  agents [--all] [--json]               every agent in every session here, with
                                        --all also on the other machines, and
                                        its address for msg
  msg [--wait] <to> <text>              message an agent: <to> is a pane number,
                                        name or program, `session/<to>` or
                                        `machine:session/<to>`. Typed in once it
                                        is idle, never into a question; --wait
                                        waits for it to finish and prints its
                                        screen
  skill [--install]                     the guide for agents driving imkr, or
                                        install it as a Claude Code skill
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
    // A client that started itself again after an update goes back to where
    // it was.
    if std::env::var_os("IMKR_REEXEC").is_some() {
        let remote = client::reexec_target();
        return client::run(true, false, remote, false);
    }
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
        ["msg", ..] => cli::msg(&args[1..]),
        ["agents", ..] => cli::agents(&args[1..]),
        ["skill", ..] => cli::skill(&args[1..]),
        ["--skill"] => cli::skill(&[]),
        ["notify", ..] => cli::notify(&args[1..]),
        ["new", ..] => cli::new(&args[1..]),
        ["rename", ..] => cli::rename(&args[1..]),
        ["move-window", ..] => cli::move_window(&args[1..]),
        ["popup", ..] => cli::popup(&args[1..]),
        ["hosts", ..] => cli::hosts(&args[1..]),
        ["plugins"] => cli::plugins(),
        ["template", ..] => cli::template(&args[1..]),
        ["revive", name] => cli::revive(name),
        ["forget", name] => cli::forget(name),
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
