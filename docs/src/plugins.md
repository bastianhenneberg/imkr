# Plugins

A plugin is a program of yours at hand in the sidebar and on a key, without a pane or window
of its own: a TUI like a task manager, a dashboard, notes. Each is a file in
`~/.config/imkr/plugins/`, e.g. `peppermint.toml`:

```toml
title = "Peppermint"
command = "peppermint-tui"        # run with sh -c, as a popup
key = "p"                         # Ctrl+B p p opens it; optional
mode = "popup"                    # or "fullscreen"
status = "peppermint-tui status"  # its first lines show in the sidebar; optional
interval = 30                     # seconds between them
bar = true                        # its first status line in the bar too; optional
enabled = true                    # false keeps the file but leaves it out
```

The sidebar lists every plugin under *Plugins*, `◇` until it runs and `◆` after, with its status
lines below. A click or its key starts it the first time and shows it, the same again hides it;
it runs on hidden, keeps its state, and has every popup key (`Ctrl+B z`, `!`, `Space`).
`Ctrl+B p` lists the plugins' keys. Since a plugin is just a program, it can be written in
any language and cannot take imkr down; a file with a mistake shows in the sidebar with what is
wrong, `imkr plugins` lists them all, and `Ctrl+B R` reads them again.

With `bar = true` the first of those lines also shows in the bar, next to the path, so it is
there with the sidebar closed. Several plugins stand side by side, from the same run of the
`status` command the sidebar uses. A line starting with `●` is green and one starting with `✗`
is red, the rest is plain text, so keep it short: the bar is one line and stays one. When it
gets tight a line is shortened, and when there is no room left it is left out rather than
pushing the path away. `Plugin status in the bar` in the settings turns it off (`plugin_status`
under `[bars]`). An imkr older than 0.21.77 does not know the field and lists such a plugin as
broken, so put it in once every machine that reads the file runs 0.21.77 or newer.

Your plugins come along: working on another machine, your terminal tells its imkr which plugins
the machine you sit at has, and they show in its sidebar too, marked `⌂`, next to its own (one of
its own of the same name wins). They run where you open them, so their program has to be
installed there; if it is not, imkr says so instead of starting nothing.

A plugin can bring more commands and react to what happens:

```toml
[[actions]]                       # Ctrl+B p d; in the background, or popup = true to watch it
key = "d"
title = "Deploy"
run = "peppermint-tui deploy"

[[hooks]]                         # the same events as [[hooks]] in the config
on = "done"
run = "peppermint-tui notify"
```

An action in the background says in the control bar when it is done or why it failed. Hooks run
only on the machine the plugin's file is on, not where it was brought along to.

### Plugins from a repository

```bash
imkr plugin add someone/imkr-plugins        # GitHub owner/repo, any git URL, or a directory
imkr plugin                                 # the plugins, and where each came from
imkr plugin update [NAME]                   # fetch new versions
imkr plugin rm NAME
```

A repository brings the plugin files in its `plugins/` directory (or at its top, without one),
and anything they need besides: `{dir}` in a command is the directory the plugin's file is in, e.g.
`command = "{dir}/../bin/deploy"`. Before adding, and before taking a new version, imkr shows
what each plugin will run, including hooks that run by themselves, and asks (`--yes` answers for
scripts). The clone stays in `plugins/.sources/`, its files are linked into `plugins/`; a plugin
of your own with the same name is left alone. Running sessions pick the changes up at once. A
directory is used where it is, so its changes count right away. `owner/repo/dir` (or `URL#dir`)
takes the plugins of one directory of a repository.

### Marketplace

Catalogs list plugins to add by name. A catalog is a git repository (private ones work through
ssh, `git@github.com:you/imkr-plugins.git`) or a directory, with the plugins in directories of
their own, at its top or in `plugins/`, so catalog and plugins can share one repository:

```text
imkr-plugins/
  plugins/brain/brain.toml       # listed as "brain", described by its title
  plugins/tui/tui.toml
  catalog.toml                   # optional: names, descriptions, plugins from elsewhere
```

```toml
# catalog.toml
[[plugin]]
name = "tui"
description = "The Peppermint TUI in a popup"
source = "plugins/tui"           # a directory here, or anything `imkr plugin add` takes
```

```toml
# config.toml
plugin_catalogs = ["git@github.com:you/imkr-plugins.git"]
```

```bash
imkr plugin search [WORD]    # what the catalogs offer, ✓ marks what is added
imkr plugin add brain        # by name, from a catalog, with the same preview and question
```

`Ctrl+B p +` opens the marketplace in a popup: the list, a number or name adds that plugin.
Catalogs are cloned into `plugins/.catalogs/` and brought up to date whenever they are listed.
