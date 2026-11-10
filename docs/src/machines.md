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

### Machines that find each other

You do not have to list every machine everywhere. imkr finds them in three ways, and shows a
machine only once imkr answered there:

- **NetBird.** Once a minute imkr reads `netbird status --json` and tries the peers that are
  online, a handful per round: first whether ssh's port is open (a phone or a laptop without sshd
  costs one refused connection and nothing in anyone's logs), then ssh without prompting and
  `imkr ls --json`. A machine where imkr answers becomes a host under the name it gives itself
  (`desktop`, not `archlinux-henneberg-desktop`). One where ssh gets in but imkr is missing shows
  as a suggestion in the group *NetBird* when you start a session elsewhere. Everything else (a
  colleague's laptop, no key there) is left alone for a day. `imkr hosts vpn` lists the peers of
  both networks and what came out for each (`imkr hosts tailnet` still works).
- **The machines the others know.** Once a minute every machine asked also tells the machines it
  knows, with their addresses. If A knows B and B knows C, A tries C and keeps it if imkr answers.
- **The way back.** Whoever reaches a machine is tried from there too, at the address its ssh
  connection came from. So a new laptop that reaches the desktop shows up on the desktop as well.

`imkr hosts rm NAME` takes a found machine out for good: finding it again does not bring it back,
`imkr hosts add NAME` does.

**Without `~/.ssh/config`.** imkr notes the address and login of every machine it found
(`~/.local/state/imkr/host-routes.json`; `imkr hosts` shows them, `imkr hosts add NAME ADDRESS
[--user USER]` sets one by hand). ssh is called with the name as long as ssh knows it (an entry in
`~/.ssh/config`, or a name that resolves), else with that address. Host keys: for an address of
your own VPN (100.64.0.0/10, where Tailscale and NetBird give theirs) imkr passes
`StrictHostKeyChecking=accept-new`, so a machine seen for the first time is taken and kept in
`~/.local/state/imkr/machines/known_hosts`; a key that **changed** still stops ssh. Any other
address keeps ssh's own rules. imkr never puts keys into another machine's `authorized_keys`:
ssh must already let you in.

`imkr settings sync` keeps the settings the same on all known machines: the config (without
what belongs to one machine: `name`, `hosts`, `web`, `web_listen`), session templates, saved
layouts, the list of accounts (without their logins) and your own plugins (not the ones installed with `imkr plugin add`, every machine gets
those itself). imkr remembers what both had after each sync, so whatever one side changed since
goes to the other: the config setting by setting, the other files whole. What both changed keeps
this machine's setting (a file: the newer one). On the first sync, only missing config keys
travel, in both directions. Existing differing values stay on their own machines and are listed
as conflicts, even with `--yes`. Align them manually and sync again to establish a baseline.
Until then, later runs still protect those differences; changes to existing keys do not propagate.
Missing files are
copied, the replaced file is kept in `.sync-backup/` next to the config, and running sessions
load the result. It lists what goes where and asks first (`--yes` does not); `imkr hosts sync`
does it too, after the update, with the same confirmation. Without a terminal it leaves
the settings as they were; use `imkr settings sync --yes` explicitly for unattended copying.
Files deleted on one machine are not deleted on the others.
Accounts are joined: each machine gets the ones it lacks, a subscription shows there as not
logged in until `imkr account login NAME`, and its default counts once it is logged in. Logins
never travel: two machines renewing one login would log each other out.

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

Images reach agents on other machines too. An agent pastes an image from the clipboard of the
machine it runs on, and in a session on another machine that is not the one in front of you.
So `Ctrl+V` there looks first into this machine's clipboard (`wl-paste`, else `xclip`): an image
in it goes over the connection as a file into `~/.cache/imkr/paste/` on the other machine, and
its path is pasted into the focused pane, which Claude Code takes as the image. Without an image
`Ctrl+V` goes through as always. Files there older than three days are removed.
