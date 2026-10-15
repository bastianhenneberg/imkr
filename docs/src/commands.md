# Commands

Every command, as `imkr --help` prints it. Generated from `src/main.rs`.

```text
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
  account [ls] | add claude|codex NAME [--url URL --model M --token-stdin]
          | login NAME | rm NAME | sync | env NAME | run NAME -- CMD
          | use NAME (new shells here) | switch NAME [--pane N] (agent now)
          | default [NAME] (new shells everywhere here start in it)
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
  cp SOURCE… DEST
                copy files like scp: machine:path, or @pane (@3, @api/claude,
                @server:api/claude) for that pane's directory, @pane::sub/path
                below it
  fleet NAME [-n N] [--agent A] PROMPT
                N agents (3, claude) on the same prompt, each in a worktree
                window NAME-1 …; `fleet NAME --diff` compares what they did
  worktree NAME [-- command]
                a new window in git worktree <repo>--NAME on branch NAME,
                next to the repository here, running command or a shell
  subscribe [--snapshot]
                what changes, as JSON lines: agent states, panes opened,
                closed and moved, in every session here (-s: only that one)
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
  new [--float] [--window N] [--dir D] [--focus] [--name NAME] [-- command args...]
                                        start a command in a new pane (window 0:
                                        a new window); --focus goes there,
                                        --name names a new window
  --host H new [--dir D] [-- command]   a window here whose program runs on H
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
                through AI Brain (`brain`, needs ai-brain-cli), then
                `msg NAME:session/pane` both ways; their messages wait for you
  outbox        messages waiting here for machines out of reach (they
                pick them up when they next ask this machine)
  read [ADDRESS|--pane N] [--lines N]   print a pane's screen (here, another session or machine)
  wait [--pane N] --until <state> [--timeout S]
                                        wait for an agent: working, blocked, idle,
                                        done, or ready (idle or done)
  hooks [--install]                     print the Claude Code hooks for status, or
                                        install them into ~/.claude
  help          show this help

Inside imkr, Ctrl+B then q opens the quit menu: detach, close the pane or end the
session. Ctrl+B then d detaches right away, the shells keep running.
```
