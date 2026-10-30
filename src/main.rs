mod account;
mod agent;
mod app;
mod bindings;
mod brain;
mod checkpoint;
mod cli;
mod client;
mod clipboard;
mod completions;
mod config;
mod contacts;
mod fdpass;
mod filetree;
mod graphics;
mod keeper;
mod keys;
mod layout;
mod links;
mod login;
mod mcp;
mod menu;
mod mouse;
mod pane;
mod persist;
mod plugin;
mod protocol;
mod push;
mod release;
mod render;
mod review;
mod screen;
mod server;
mod settings;
mod shapes;
mod share;
mod sidebar;
mod template;
mod term;
mod theme;
mod tmux;
mod upgrade;
mod usage;
mod web;
mod workspace;
mod worktree;

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
  ls [--json]   list the running sessions, with who each was started for
  dirs [DIR]    the directories in DIR (or the one on stdin) as JSON, for
                completing a session's directory from another machine
  hive ls       list the hives: saved groups of sessions
  hive save [NAME]
                save every running session as a hive (default `last`)
  hive restore [NAME] [--from HOST] [--take MACHINE]
                bring a hive back on every machine in it: missing sessions
                start, machines that are off show as offline. --from fetches
                it first (a new machine), --take restores that machine's
                sessions here (one that replaces it)
  hive switch NAME [--yes]
                only that hive: bring it back, end every other running
                session on its machines (into `last`), after asking
  hive rm NAME  delete a hive, running sessions keep running
  hive rename OLD NEW
                rename a hive
  save, restore short for `hive save` and `hive restore`
  plugins       list the plugins (~/.config/imkr/plugins/*.toml) and broken files
  plugin add SOURCE|NAME [--yes] | update [NAME] | rm NAME
                plugins from a git repository (URL or GitHub owner/repo[/dir]),
                a directory, or by name from a catalog; shown before they are added
  plugin search [WORD] | market
                what the plugin catalogs offer (`plugin_catalogs`)
  account [ls [--json]] | add claude|codex NAME [--url URL --model M --token-stdin]
          | login NAME | rm NAME | sync | env NAME | run NAME -- CMD
          | use NAME (new shells here) | switch NAME [--pane N] (agent now)
          | default [NAME] (new shells everywhere here start in it)
          | usage [--json] (each account's limits and the tokens used)
                several Claude Code and Codex accounts on this machine,
                subscriptions or providers (OpenRouter, Ollama)
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
  hosts sync    the latest release on every one of them: installed where
                imkr is missing, upgraded with its sessions elsewhere
  hosts tailnet the machines of your Tailscale network, and which imkr uses
  settings sync [--yes] [HOST…]
                settings with the other machines: config (without name,
                hosts, web), templates, layouts, own plugins; what one
                side changed since the last sync wins, backed up first
  web [--listen ADDR[:PORT]]
                this machine's sessions and agents in a phone's browser,
                on the VPN address (Tailscale, NetBird) or 127.0.0.1
  web pair | devices | forget NAME|all
                pair a phone (a code for ten minutes), list or remove them
  web page | state | send
                the page, what it shows (JSON), type into a pane (JSON on
                stdin): for ai-brain-harness, which shows it in AI Brain
  rename-session NAME
                rename the session (`-s OLD rename-session NEW` for another)
  kill-server   shut down the session's server and every pane in it
  update        move every running session (or the one of -s) to this
                binary, e.g. after `cargo install`; panes, programs and
                screens stay
  keys          list the key bindings
  upgrade       download the latest release, replace this binary and
                move every running session (or the one of -s) to it
  version       print the version

For scripts and agents:
  list [--json]                         list the panes with program and agent state
  status <working|blocked|idle|clear>   report this pane's agent state (for hooks)
  send [--pane N] [--no-enter] [--stdin] <text>   type text into a pane
  checkpoints   the checkpoints of the repository here: its state after
                each agent turn, with the setting on
  run [--pane N] [--timeout S] -- command
                type command into the pane's shell, wait until it ends,
                print its output and exit with its code
  wait [--pane N] [--timeout S]
                without --until: wait for the command running in the
                pane's shell, print its output and exit with its code
  shell-integration zsh|bash
                what to eval in ~/.zshrc or ~/.bashrc for run and wait
  completions zsh|bash
                Tab completion for imkr: commands, options and the names of
                sessions, hosts, hives, accounts …; eval in ~/.zshrc or ~/.bashrc
  mcp           serve imkr's commands as MCP tools on stdin/stdout, for
                agents: claude mcp add imkr -- imkr mcp
  layout ls | save NAME | NAME
                the layouts (presets as in tmux, and saved ones), or save
                the active window's, or give it one; its programs run on
  explain [--pane N] [--json]
                why a pane counts as the agent it is, in its state
  operator [--json]
                the machine the user sits at (and the name it reaches this one by)
  screen status [--json]
                whether an agent may see and drive this machine's screen:
                the backend, the screens with their logical size, the
                tools, whether the session is locked, the setting
                (`computer_use`) and the permission in force
  screen shot [--monitor M] [--width W] [--out PATH] [--json]
                a PNG of the screen, scaled down, with how its pixels map
                onto the coordinates `click` takes
  screen move X Y | click [X Y] [--button left|right|middle]
                | type TEXT | key KEYS…
                drive the screen, in logical coordinates per screen
  screen log [--lines N] | allow | refuse
                every action taken on this screen; the user's own answer
                when `computer_use` is `ask`
  share [--json] | setup [--yes] | set PATH | unset
        | elsewhere HOST PATH | elsewhere HOST --rm
                the folder where imkr puts files for you to look at, e.g.
                `screen shot --save`: an SMB or NFS mount, Syncthing,
                Nextcloud or a plain folder. `setup` offers the ones this
                machine has. It lives in ~/.config/share-dir.toml (`dir`,
                and `[elsewhere]` for the same folder on your other
                machines), which other programs read and write too
  --host H screen …
                the screen of machine H, through the same ssh way
  archive [ls | export | drop NAME SAVED_AT]
                ended sessions older than the ten in `last`, by hive, on every machine
  cp SOURCE… DEST
                copy files like scp: machine:path, or @pane (@3, @api/claude,
                @server:api/claude) for that pane's directory, @pane::sub/path
                below it
  keeper [NAME] [--host H] [--dir DIR]
                a keeper session (`keeper`, else `keeper-2` …): its agent
                only steers, starting with the `keeper` skill (from AI
                Brain) and its folder <state>/keeper/NAME (briefings/,
                keeper-stand.md); goes there when typed in a terminal.
                Keepers' sessions are marked ⚑ and listed first everywhere
  keeper --mark [SESSION] | --unmark [SESSION]
                mark a session (this one without a name) as a keeper's
                afterwards, or take the mark off
  spawn NAME --brief FILE|- [--host H] [--dir REPO] [--worktree] [--task N]
        [--keeper K] [--json]
                a bee: session NAME with the default agent (in a new git
                worktree NAME with --worktree), which gets the briefing as
                its first message from this pane once it is ready; noted
                in the keeper's bees.json (this session's, or K's) and
                shown in Ctrl+B O. Prints its address. An agent that asks
                something first (trust the folder?) is left asking: exit 1
  fleet NAME [-n N] [--agent A] PROMPT
                N agents (3, claude) on the same prompt, each in a worktree
                window NAME-1 …; `fleet NAME --diff` compares what they did
  worktree NAME [-- command]
                a new window in git worktree <repo>--NAME on branch NAME,
                next to the repository here, running command or a shell;
                [worktree] in the config and .imkr.toml copy files over
  worktree ls [--json] | merge NAME | rm NAME [--force]
                the worktrees with what is not merged; merge into the
                branch checked out here, then remove; remove worktree,
                window and branch (refused with work not merged)
  subscribe [--snapshot]
                what changes, as JSON lines: agent states, panes opened,
                closed and moved, in every session here (-s: only that one)
  agents [--all] [--json] [--by A]      every agent in every session here, with
                                        --all also on the other machines, and
                                        its address for msg; --by only those in
                                        sessions started for that pane
  push test                             send a test push the way [push] in the
                                        config says (ntfy, webhook): agents that
                                        need you, fail or run out reach the phone
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
  new [--float] [--window N] [--dir D] [--focus] [--name NAME] [--start]
      [--by ADDRESS] [-- command args...]
                                        start a command in a new pane (window 0:
                                        a new window); --focus goes there,
                                        --name names a new window; --start
                                        starts the session first if it does not
                                        run, and with window 0 the command then
                                        takes the place of the empty shell it
                                        starts with, in window 1. --by ADDRESS
                                        marks the session as started for that
                                        pane (`machine:session/pane`): the
                                        lists show it under its orchestrator.
                                        From a pane, --start sets it by itself
  --host H new [--dir D] [-- command]   a window here whose program runs on H
  note [--pane N] [<text> | ls]          put a thought on a pane's notepad
                                        (Ctrl+B t), or list it; its agent
                                        is not told
  rename [--pane N | --window N] [--clear | <name>]
                                        name a pane or a window
  move-window [--window N] <session>    move a window with its running panes to
                                        another session, started if needed
  popup [--name N] [--title T] [-- command args...]
                                        show or hide a popup running the command,
                                        started the first time; Ctrl+B Space too
  connect [invite | join URL CODE | brain PERSON | fetch | inbox | accept ID
          | decline ID | trust NAME | untrust NAME | rm NAME]
                other people's imkrs: pair once with a code (web view in a VPN) or
                through AI Brain (`brain`, needs ai-brain-harness), then
                `msg NAME:session/pane` both ways; their messages wait for you
  outbox        messages waiting here for machines out of reach (they
                pick them up when they next ask this machine)
  read [ADDRESS|--pane N] [--lines N]   print a pane's screen (here, another session or machine)
  tap ADDRESS [--every MS] [--max BYTES]
                                        show a pane to a program: JSON lines out
                                        (screen, out, gap, exit), input as JSON lines in
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
    // Run as `tmux` by programs in panes, with the tmux shim on.
    if std::env::args()
        .next()
        .is_some_and(|argv0| argv0.rsplit('/').next() == Some("tmux"))
    {
        std::process::exit(tmux::run(&std::env::args().skip(1).collect::<Vec<_>>()));
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
    // The session asked from, before `-s` changes it: a hosted window
    // shows up there, and `imkr new` marks the session it starts as that
    // pane's (#9087).
    let here = protocol::socket_path().ok();
    protocol::remember_caller();
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
    let chosen = session.clone();
    let remote = host.map(|host| client::Remote { host, session });
    // Rust ignores SIGPIPE, so `imkr account | head` died with a panic when
    // head closed the pipe. Commands that print end quietly instead, like
    // other tools; the ones that live on connections (client, proxy, server,
    // web, hosted windows) keep handling a closed one themselves.
    // `imkr web page|state|send` only print, for ai-brain-harness.
    let prints = matches!(
        args.get(1).map(String::as_str),
        Some("page" | "state" | "send")
    );
    let lives_on_connections = match args.first().map(String::as_str) {
        Some("web") => !prints,
        first => matches!(
            first,
            None | Some("attach" | "a" | "proxy" | "server" | "hosted" | "attach-pane" | "tap")
        ),
    };
    if !lives_on_connections {
        // SAFETY: no other thread runs yet; restoring the default action.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
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
        // A keeper or a bee on another machine: started there through ssh.
        ["keeper", ..] => keeper::keeper(&args[1..], remote.map(|r| r.host)),
        ["spawn", ..] => keeper::spawn(&args[1..], remote.map(|r| r.host)),
        ["new", ..] if remote.is_some() => match (&here, &remote) {
            (Some(here), Some(remote)) => cli::new_hosted(here, remote, &args[1..]),
            _ => bail!("no session here to show the window in"),
        },
        // The screen of another machine: the command runs there, a
        // screenshot comes back as bytes.
        ["screen", ..] if remote.is_some() => {
            cli::screen(&args[1..], remote.as_ref().map(|r| r.host.as_str()))
        }
        _ if remote.is_some() => {
            bail!(
                "--host works with attach, new, screen, keeper and spawn only, run other commands there"
            )
        }
        ["kill-server"] => client::kill_server(),
        ["ls", ..] => cli::ls(&args[1..]),
        ["dirs"] => cli::dirs(None),
        ["dirs", parent] => cli::dirs(Some(parent)),
        ["keys"] => {
            print!("{}", bindings::reference());
            Ok(())
        }
        ["server"] => server::run(),
        ["status", ..] => cli::status(&args[1..]),
        ["list", ..] => cli::list(&args[1..]),
        ["send", ..] => cli::send(&args[1..]),
        ["msg", ..] => cli::msg(&args[1..]),
        ["note", ..] => cli::note(&args[1..]),
        ["connect", ..] => cli::connect(&args[1..]),
        ["agents", ..] => cli::agents(&args[1..]),
        ["push", ..] => cli::push(&args[1..]),
        ["subscribe", ..] => cli::subscribe(&args[1..], chosen),
        ["worktree", ..] => cli::worktree(&args[1..]),
        ["fleet", ..] => cli::fleet(&args[1..]),
        ["cp", ..] => cli::cp(&args[1..]),
        ["explain", ..] => cli::explain(&args[1..]),
        ["operator", ..] => cli::operator(&args[1..]),
        ["archive", ..] => cli::archive(&args[1..]),
        ["layout", ..] => cli::layout(&args[1..]),
        ["screen", ..] => cli::screen(&args[1..], None),
        ["share", ..] => share::run(&args[1..]),
        ["mcp"] => mcp::serve(),
        ["run", ..] => cli::run(&args[1..]),
        ["shell-integration", ..] => cli::shell_integration(&args[1..]),
        ["completions", ..] => completions::script(&args[1..]),
        ["complete", ..] => completions::complete(&args[1..]),
        ["checkpoints"] => {
            print!("{}", checkpoint::list(&std::env::current_dir()?)?);
            Ok(())
        }
        ["skill", ..] => cli::skill(&args[1..]),
        ["--skill"] => cli::skill(&[]),
        ["notify", ..] => cli::notify(&args[1..]),
        ["new", ..] => cli::new(&args[1..]),
        ["hosted", ..] => cli::hosted(&args[1..]),
        ["attach-pane", ..] => cli::attach_pane(&args[1..]),
        ["tap", ..] => cli::tap(&args[1..]),
        ["web", ..] => web::main(&args[1..]),
        ["rename", ..] => cli::rename(&args[1..]),
        ["move-window", ..] => cli::move_window(&args[1..]),
        ["popup", ..] => cli::popup(&args[1..]),
        ["hosts", ..] => cli::hosts(&args[1..]),
        ["settings", ..] => cli::settings(&args[1..]),
        ["plugins"] | ["plugin"] | ["plugin", "ls"] => cli::plugins(),
        ["plugin", ..] => cli::plugin(&args[1..]),
        ["account" | "accounts", ..] => cli::account(&args[1..]),
        ["outbox", ..] => cli::outbox(&args[1..]),
        ["template", ..] => cli::template(&args[1..]),
        ["revive", name] => cli::revive(name),
        ["forget", name] => cli::forget(name),
        ["read", ..] => cli::read(&args[1..]),
        ["wait", ..] => cli::wait(&args[1..]),
        ["hooks", ..] => cli::hooks(&args[1..]),
        ["update"] => cli::update(chosen),
        ["upgrade"] => release::upgrade(chosen),
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

#[cfg(test)]
mod tests {
    /// docs/src/commands.md is `imkr --help` as a page; written anew with
    /// `IMKR_WRITE_DOCS=1 cargo test docs_commands`.
    #[test]
    fn docs_commands_are_current() {
        let want = format!(
            "# Commands\n\nEvery command, as `imkr --help` prints it. Generated from `src/main.rs`.\n\n```text\n{}\n```\n",
            super::USAGE.trim_end()
        );
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/src/commands.md");
        if std::env::var_os("IMKR_WRITE_DOCS").is_some() {
            std::fs::write(path, &want).unwrap();
        }
        let have = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            have == want,
            "docs/src/commands.md is out of date: IMKR_WRITE_DOCS=1 cargo test docs_commands"
        );
    }
}
