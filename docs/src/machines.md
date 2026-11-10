# Other machines

The machine you sit at is the **operator**, the beekeeper if you like (the *Imker* imkr is named
after): it tends every hive, every machine with its sessions and agents, and every connection
starts there, with your ssh keys. Any of your machines can be the operator, whichever you sit at.

One imkr for the sessions here and on other machines, without an imkr inside an imkr: the
sidebar and the session list (`Ctrl+B S`) show every machine's sessions, and picking one switches
this terminal over to it. Every machine of yours is in both lists, also one where nothing runs,
one with only closed sessions, one that does not answer and one only the register knows: as
*+ new session…* with why (*nothing runs there yet*, *not reachable*, *in the register, not
reached yet*), and in `Ctrl+B s n` with the same words. imkr runs on that machine and keeps its shells and agents going when the
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

### Machines that know of each other

You add a machine once, by hand, on any of your machines (`imkr hosts add NAME [ADDRESS]
[--user USER]`, or `imkr --host NAME`), and the others learn of it. Every machine keeps a
register of your machines: name, machine id, address, login, and when that was last true. Once a
minute, when the machines ask each other for their sessions anyway, each sends its register and
gets the other's back; both join them, and for one machine the newer entry wins. So a machine that
got a new name or a new address in your VPN is known by it everywhere, and if A knows B and B
knows C, A learns of C. Whoever reaches a machine is in its register too, at the address its ssh
connection came from, so a new laptop that reaches the desktop shows up on the desktop as well.

A machine from the register shows as a host once ssh got in from here, without prompting, and
imkr answered there; until then `imkr hosts` lists it as *in the register, not reached yet*, and
it is tried again every ten minutes. imkr does not search your network: it never knocks at a
peer of your NetBird or Tailscale network that no machine of yours named, and it never lists the
peers of a NetBird network at all. Such a network is often shared with other people's machines,
and nobody who installs imkr is to see them all.

`imkr hosts rm NAME` takes a machine out everywhere: it leaves a removal with its time in the
register, which travels like any entry, so the machine does not come back over the others.
`imkr hosts add NAME`, on any machine, takes it back.

### Pairing a new machine

A machine imkr finds in the register still needs ssh to let it in. Instead of copying keys into
`authorized_keys` by hand, pair the new machine with **one** of yours, with a code:

```bash
# on a machine you already have (it shows the code and waits 10 minutes)
imkr hosts pair
# on the new machine, with what the first one showed
imkr hosts join 100.96.1.2 ABCD-EFGH
```

`hosts pair` listens on this machine's VPN address (100.64.0.0/10, Tailscale or NetBird), or on
127.0.0.1 without one, on port 4658 (`--listen ADDR[:PORT]` for another); it never listens
anywhere else, so no one outside your own network sees it. The code is good for ten minutes and
five wrong tries. Typing it on the new machine is the confirmation: whoever knows it sits at both.
Both sides then let each other in with their own machine key `~/.ssh/imkr-<machine id>` (ed25519,
comment `imkr@NAME`, made by imkr on that machine where it is missing, so it was never anywhere
else), take each other's register and ssh host keys (into
`~/.local/state/imkr/machines/known_hosts`, so the first ssh does not take a key unseen), and
become hosts of each other. Nothing else asks first: there is no searching of the VPN, and no key
gets written without a code.

**Once is enough.** The register entry of a machine carries its key, and every machine that gets
the entry lets that machine in. Within a minute or two, as the machines ask each other for their
sessions, the new one's key reaches all your other machines and theirs reach it. Only a **paired** machine takes keys at all: one with a key of its own
from `hosts pair`, `hosts join` or `hosts key`. A machine you only reach by a login you set up
yourself (a server you use with `--host`, a machine that is never paired) never stores the keys
of others and never touches its `authorized_keys`, not even for a backup, however often your
machines ask it for its sessions. Keys are taken
only from a machine that may hand them on: on the machine that is asked, the asking one has just
logged in (by a key you set by hand it has a shell there anyway; by a key of imkr's it was paired);
on the asking side, what comes back loses the keys it passes on for others, because answering does
not prove those may get in. The answering machine's own key stays: ssh has just checked its host
key, so a machine that runs no session (and so never asks anyone) still gets its key to the others. `imkr hosts` says for each machine *paired* (imkr lets it in) or *key by hand* (a line you
wrote lets it in).

**One key per machine.** imkr pairs with `~/.ssh/imkr-<machine id>`, not with `~/.ssh/imkr`: that
one may be the same file on several machines (copied by a dotfile manager such as chezmoi), and a
key that several machines hold cannot be taken from one of them. A key that the register shows
for more than one machine lets in none of them (imkr removes its lines for it). ssh is offered the
own key first and `~/.ssh/imkr` after it, so lines you set up by hand keep working.

**Revoking.** `imkr hosts rm NAME` takes the machine's key out of `authorized_keys` here, and the
removal travels with the register, so every other machine takes it out too. A machine whose key
imkr removed cannot bring itself back: `imkr gate` knows which machine logged in, and that one's
word does not undo its own removal. A key you also put in by hand stays; imkr says so.

**Only its own lines.** imkr writes and removes only lines that end in `imkr:<machine id> NAME`.
Every other line of `authorized_keys` stays byte for byte. Before the first change the file is
copied to `~/.ssh/authorized_keys.imkr-backup`; it is written whole through a file beside it, mode
0600. If a machine's key is already in a line of yours, imkr adds none (ssh takes the first line
with a key, so a second, restricted one would change nothing).

**What the restriction protects, and what not.** imkr's lines look like

```
restrict,pty,command="/home/you/.local/bin/imkr gate MACHINE-ID" ssh-ed25519 AAAA… imkr:MACHINE-ID laptop
```

`restrict` turns off port, agent and X11 forwarding and the user's `~/.ssh/rc`; `pty` is allowed
again because hosted windows (`imkr --host H new`) attach with a terminal. `imkr gate` runs a
command only if it is exactly in the form imkr itself sends over ssh (a login shell running
`imkr …` with the variables imkr sets), checked by building it again, and refuses the few
commands that change who gets in or hand out logins (`hosts` other than `add`, `account`,
`connect`, `web pair|devices|forget`, `screen allow|refuse`, `gate`). So a paired machine gets no
shell, no `scp`/`sftp`, no tunnel into your network through this one, and cannot pair further
machines here. It is **not** a sandbox: `imkr send`, `imkr run` and `imkr new -- CMD` type into
panes and start programs, which is what imkr is for, so a paired machine can do what you can do
in a pane. Pair only machines that are yours. A machine that was taken over before you revoked it
may have handed on keys of its own in the meantime: check `imkr hosts` afterwards.

**Machines set up by hand before.** They keep working as they are. To move them to keys of their
own, so each can be revoked alone:

1. On each machine that is to take part, once: `imkr hosts key` makes `~/.ssh/imkr-<machine id>`
   and puts it in the register; from then on it is paired. (A pairing does the same.) Over the
   next minute or two every paired machine that already reaches it by the old key takes the new
   key in, as an imkr line. Machines you leave out (servers) stay as they are.
2. Wait until `imkr hosts` on every machine says *paired* for all the others.
3. Then remove the lines with the old, copied key from `~/.ssh/authorized_keys` on each machine,
   take `~/.ssh/imkr` out of the dotfile manager and delete it. From then on `imkr hosts rm NAME`
   shuts that one machine out everywhere.

Until step 3 a removed machine still gets in by the copied key: imkr does not touch lines it did not
write, and it cannot tell which machines hold a copied key.

Over a paired key, everything imkr does between machines works: sessions, switching, hosted
windows, messages, hives, archive, `settings sync`, `hosts sync` upgrading imkr. Installing imkr
on a machine that does not have it (`hosts sync`, `imkr --host`) needs your own login there; a
paired machine always has imkr. Older versions of imkr ignore the keys in the register; they need
`imkr update` to take part.

**NetBird and Tailscale.** The machines of your tailnet that are online and not used yet show as
suggestions when you start a session on another machine (`Ctrl+B s n`, group *Tailscale*);
`imkr hosts vpn` lists them and what imkr knows of each (`imkr hosts tailnet` still works). A
NetBird network is offered nowhere, not in `Ctrl+B s n`, not in `imkr hosts vpn`: imkr does not
even ask `netbird` for its peers. Your own machines there come in the ways above (`imkr hosts
add NAME ADDRESS`, the register, `imkr hosts pair`), and imkr then reaches them at their NetBird
address like at any other.

**Without `~/.ssh/config`.** imkr notes the address and login of every machine it reached
(`~/.local/state/imkr/host-routes.json`; `imkr hosts` shows them, `imkr hosts add NAME ADDRESS
[--user USER]` sets one by hand). ssh is called with the name as long as ssh knows it (an entry in
`~/.ssh/config`, or a name that resolves), else with that address. Host keys: for an address of
your own VPN (100.64.0.0/10, where Tailscale and NetBird give theirs) imkr passes
`StrictHostKeyChecking=accept-new`, so a machine seen for the first time is taken and kept in
`~/.local/state/imkr/machines/known_hosts`; a key that **changed** still stops ssh. Any other
address keeps ssh's own rules. imkr puts keys into `authorized_keys` only after a pairing with a
code (see *Pairing a new machine*); otherwise ssh must already let you in.

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
