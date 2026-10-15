# Hives and templates

## Hives

A hive is a group of sessions saved together under a name: their windows, splits, floating
panes and directories. `Ctrl+B H` opens the hives, and every key it takes is listed in it:

| Key | Action |
|---|---|
| `Enter` or digit | Bring the hive back: start each of its sessions that does not run |
| `s` | Save the running sessions as a new hive |
| `u` | Update the selected hive with the sessions running now (asks first) |
| `r` | Rename the hive |
| `x` | Delete the hive (asks first), running sessions keep running |

Every session that ends on purpose (`Ctrl+B Q`, `x` in the sessions menu, `kill-server`) puts
its layout into the hive `last`, so bringing `last` back reopens everything that was open, each
session as its own server again. Sessions already running are left alone, and a running
session is marked with ● in the list.
`last` keeps the ten sessions ended most recently; for longer, save them as a hive of their own.
Sessions stopped by a crash or a reboot are not in `last` and always come back.

```bash
imkr hive save work       # the running sessions as the hive `work`
imkr hive ls              # every hive with its sessions and age
imkr hive restore work    # start what of `work` does not run
imkr hive switch work     # only `work`: bring it back, end the rest (asks, naming them)
imkr hive rename work api
imkr hive rm api
```

Switching (`w` in `Ctrl+B H`) changes context, say from one customer to another: the hive comes
back, then every running session it does not have ends, on its machines too, and goes into
`last` as any ended session does. It asks first and names them, because work can end with them.

`imkr save` and `imkr restore` are short for `hive save` and `hive restore`. Hives live in
`~/.local/state/imkr/saves/`, plain JSON, easy to copy or back up.

### Hives across machines

A hive is a map of every machine: saving it takes the sessions here and, through ssh, those of
every other machine known (see [Other machines](machines.md)); one that is off keeps what the hive had of it.
The hive is then given to every machine that answers, and every imkr fetches the hives of the
others once a minute, the newest of a name winning. So a hive saved on the laptop is there on
the desktop at home and on the server, and `last`, each machine's own ended sessions, stays
where it is.

Bringing it back works from any machine in it: this machine's missing sessions start here, the
other machines are made known, start their missing sessions if they answer, and show as
*offline* with their last sessions greyed out if they do not. Each of them learns of this
machine too, so a new one joins by restoring a hive:

```bash
imkr hive restore work --from codingmachine               # a new machine: fetch it from there first
imkr hive restore work --from codingmachine --take laptop # and bring the broken laptop's sessions here
imkr hosts                                               # the other machines this one shows
imkr hosts add desktop                                   # or forget one with `rm`
imkr hosts sync                                          # the latest release on all of them
imkr settings sync                                       # settings with all of them, what changed wins
imkr hosts tailnet                                       # the machines of your Tailscale network
```

Machines are told apart by `/etc/machine-id`, so two machines set up alike may share a hostname
(both `omarchy`, say); give each a `name` in its settings to tell them apart on screen too.

ssh between the machines has to work without a password (keys, an agent) and they have to reach
each other, e.g. over a VPN like NetBird or Tailscale; imkr only uses it.

## Templates

A template is what a new session starts with: its windows, their splits, the directories and
the commands typed into them. When there are templates, `Ctrl+B s n` asks for one after the
name, *empty* first. Each is a TOML file in `~/.config/imkr/templates/`:

```toml
title = "Coding"
root = "~/Development/{session}"   # {session} is the new session's name

[[window]]
name = "claude"
command = "claude"

[[window]]
name = "terminal"
[[window.pane]]
command = "nvim"
[[window.pane]]
split = "right"   # or "down": of the pane before, or of pane `of = 1`, 2, …
size = 40         # percent the new pane gets
cwd = "src"       # relative to root
```

After the template, `Ctrl+B s n` asks for the session's directory: suggested is the template's
`root`, without one the directory of the focused pane. The directories that fit what is typed
are listed under it, on the machine the session starts on (another one through ssh): `↑`/`↓`
choose one, Tab takes it or completes like a shell, `Ctrl+W` goes up one; relative `cwd`s of the template start there. Commands are typed
into a fresh shell, so the shell stays when they end. Templates come along to other machines
like plugins, and `~` is that machine's home.

```bash
imkr template save coding          # this session's windows, splits and agents as `coding`
imkr template ls
imkr template start coding api     # start session `api` from it, without a terminal
```

`template save` puts the directory the panes share into `root`, as `{session}` when it is
named like the session, and writes agents as their command; other programs are left out, as
the template should not run what happened to run.
