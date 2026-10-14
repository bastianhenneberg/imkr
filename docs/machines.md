# Other machines

The machine you sit at is the **operator**, the beekeeper if you like (the *Imker* imkr is named
after): it tends every hive, every machine with its sessions and agents, and every connection
starts there, with your ssh keys. Any of your machines can be the operator, whichever you sit at.

One imkr for the sessions here and on other machines, without an imkr inside an imkr: the
sidebar and the session list (`Ctrl+B S`) show every machine's sessions, and picking one switches
this terminal over to it. imkr runs on that machine and keeps its shells and agents going when the
connection drops; this terminal stays the client, so keys, clipboard, images and the window size
are local. On the other machine the list offers this one's sessions to come back to. Machines
are marked from where you sit: `⌂` is the one your terminal runs on, `⇄` any other, and the
control bar says `⇄ machine` while you work on one you do not sit at. Every machine shows them
all: on another one, your terminal tells its imkr what runs on yours and on every machine yours
knows, so you go from there straight to a third, connecting from your own machine with your own
ssh keys.

Machines come from the settings and from `imkr --host`:

```toml
hosts = ["codingmachine"]   # as ssh knows them, e.g. from ~/.ssh/config
```

`imkr --host server` goes straight to a session there (`-s NAME` picks one), and remembers the
host. Plain `imkr` goes back to the session used last, wherever it was; with *Session list on
start* in the settings it opens the session list there first. If the machine is out of reach, it
starts here and says why.

It runs `ssh -C server imkr proxy` through a login shell, with `~/.cargo/bin` and `~/.local/bin`
added to the PATH, so ssh should log in without a password (keys, an agent or `ControlMaster`).
The sessions there are listed every 5 seconds with `imkr ls --json`, which needs this version
of imkr there too; the sidebar says if it is missing or the machine does not answer. If imkr is
missing on a machine, `imkr --host` offers to install the latest release there (into
`~/.local/bin`), and `imkr hosts sync` brings every known machine up to date at once: installed
where it is missing, upgraded with all its running sessions everywhere else. With
Tailscale, the machines of your tailnet that are online and not used yet show as suggestions when
you start a session on another machine (`Ctrl+B s n`), by their MagicDNS name.

`imkr settings sync` keeps the settings the same on all known machines: the config (without
what belongs to one machine: `name`, `hosts`, `web`, `web_listen`), session templates, saved
layouts and your own plugins (not the ones installed with `imkr plugin add`, every machine gets
those itself). imkr remembers what both had after each sync, so whatever one side changed since
goes to the other: the config setting by setting, the other files whole. What both changed keeps
this machine's setting (a file: the newer one); so does the very first sync. Missing files are
copied, the replaced file is kept in `.sync-backup/` next to the config, and running sessions
load the result. It lists what goes where and asks first (`--yes` does not); `imkr hosts sync`
does it too, after the update. Files deleted on one machine are not deleted on the others.

A single window can run on another machine too, in the session you are in: `Ctrl+B w h` asks
for the machine, or `imkr --host server new [--dir D] [-- command]` from a pane. The program
runs in a new window of the session there (started if needed), and this window shows it, at its
size, typing into it. If the connection drops, it connects again on its own; if this machine
restarts, the window comes back with the session and finds the program still running there.
It closes when the program there ends; closing it here leaves the program running, in its window
there. An agent in it counts as one of this session's: its state shows on the window, in the
sidebar, the inbox and the agent list, and answering it from the inbox types there. Under the hood it is `imkr hosted server PANE` here and `imkr attach-pane PANE` there.

Agents of the other sessions, here and on other machines, are listed under *Agents* below this
session's own, grouped by session (`⇄ codingmachine · brain`), those that need you first. A click
goes to the session and shows the agent's pane. When one elsewhere comes to need you or finishes,
imkr tells you as it does for its own: in the bar, with a bell, and a notification.
