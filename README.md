# imkr 🐝

A terminal multiplexer and window manager for humans and their coding agents, written in Rust.
Say it like *Imker*, German for beekeeper: it tends the hives your agents work in.

imkr combines tiling and floating panes with a tmux-style prefix, and keeps an eye on
the agents running in its panes: working, blocked, or idle.

The names follow tmux: a session holds windows, a window holds panes. A *hive*, a saved group
of sessions, is imkr's own, as tmux has nothing like it.

> **Status:** early prototype. imkr runs shells in split panes, including full-screen
> programs like nvim, spread over windows, and keeps them running when you detach.

## Ideas

- **tmux-style prefix keys** (`Ctrl+B` by default)
- **Tiling and floating panes**: a BSP layout tree with a floating layer on top, plus windows
- **Server/client architecture**: detach and reattach without stopping work
- **Agent-aware**: every pane is marked working, blocked, or idle
- **Socket API** so agents and scripts can drive imkr
- **Hives**: save several sessions together under a name and bring the whole group back later

Inspired by [TUIOS](https://github.com/Gaurav-Gosain/tuios) and [herdr](https://github.com/herdrdev/herdr).

## Roadmap

1. ~~One shell in one pane (PTY → VT emulation → rendering)~~ ✓
2. ~~Splits with a BSP tree, focus, resize, prefix keys~~ ✓
3. ~~Server/client split with detach and reattach~~ ✓
4. ~~Floating panes and windows~~ ✓
5. ~~Copy mode, scrollback, mouse~~ ✓
6. ~~Agent status, socket API~~ ✓
7. ~~Layout persistence across restarts~~ ✓

## Install

Linux on x86_64 or aarch64, no toolchain needed:

```bash
curl -fsSL https://raw.githubusercontent.com/bastianhenneberg/imkr/main/install.sh | sh
```

That puts the latest release into `~/.local/bin`. `PREFIX` picks another root and
`IMKR_VERSION` another tag: `curl -fsSL … | IMKR_VERSION=v0.1.0 PREFIX=/usr/local sh`.

With a Rust toolchain, from crates.io or straight from the repository:

```bash
cargo install imkr --locked   # once imkr is on crates.io
cargo install --git https://github.com/bastianhenneberg/imkr --locked
```

imkr is Linux only for now: it reads `/proc` to tell what a pane is running and where.
Everything else is portable, so a port is mostly a matter of replacing those two reads.

```bash
imkr              # attach to the session, or start one
imkr attach       # attach, fail if there is no session
imkr kill-server  # stop the server and every shell in it
imkr keys         # list the key bindings
imkr version      # what is installed
imkr -s work      # the session `work`, started if needed
imkr ls           # the running sessions with panes and agents
```

The first `imkr` starts a server in the background. It owns the shells and keeps them
running after you detach or close the terminal. The socket lives in
`$XDG_RUNTIME_DIR/imkr/`, set `IMKR_SOCKET` to use another one. Several terminals can attach
to the same session, as in tmux: they all show it and type into it, at the size of the smallest
one (the rest of a larger terminal is filled with dots), and the control bar says how many are
attached. `Ctrl+B d` detaches only the terminal it was pressed in, `imkr attach -d` detaches all
others.

Sessions are separate servers with their own panes, socket and saved layout. `imkr -s NAME`
(or `attach -t NAME`) picks one, `Ctrl+B S` switches between them without leaving the terminal,
and named sessions show their name in the control bar. In that menu `n` starts a new session,
`x` ends the selected one, `r` renames it (like `imkr rename-session NAME`, `-s OLD` for
another one), `s` saves all running sessions as a hive and `h` opens the hives.

`Ctrl+B w m` moves the active window to another session: pick a running one, or `n` for a new
one, started for it. E.g. to gather the windows of one project in a session of their own
(`imkr move-window --window N NAME` from a script). Its programs keep running, nothing restarts: the ptys are passed to the other
server over its socket. The window keeps its name and, if free, its number. Agents in it keep
reporting to the session they came from, which passes it on while it runs; after that their
state comes from what imkr sees on the screen.

| Key | Action |
|---|---|
| `Ctrl+B` `%` | Split the pane side by side |
| `Ctrl+B` `"` | Split the pane top and bottom |
| `Ctrl+B` `x` | Close the pane (asks first) |
| `Ctrl+B` `r` | Name the pane (`Ctrl+B` `w` `r` names the window) |
| `Ctrl+B` `z` | Zoom: the pane alone over the whole window, again to go back |
| `Ctrl+B` `{` / `}` | Swap the pane with the previous / next one |
| `Ctrl+B` `=` | Give all panes equal sizes |
| `Ctrl+B` `f` … | Floating panes menu: `f` float/tile, `n` new, `o` next, arrows move |
| `Ctrl+B` `Shift+←↑↓→` | Move a floating pane |
| `Ctrl+B` `n` | New window, asks for its name right away (Esc closes it again) |
| `Ctrl+B` `w` … | Windows menu: `1`–`9`, `c` new, `n`/`p` next/previous, `m` move to another session, `=` renumber 1, 2, 3 … |
| `Ctrl+B` `1`–`9` | Go to window, an empty one starts a shell |
| `Ctrl+B` `←↑↓→` | Focus the pane in that direction |
| `Ctrl+B` `o` | Focus the next pane |
| `Ctrl+B` `;` | Back to the pane focused before, across windows |
| `Ctrl+B` `g` | Go to any pane: type to filter by window, name, program, state or directory |
| `Ctrl+B` `a` | Jump to the agent that has waited longest for you, then to finished ones |
| `Ctrl+B` `[` | Copy mode: scroll back with vim keys (`Ctrl-U`/`Ctrl-D`, `Ctrl-B`/`Ctrl-F`, `g`/`G`), `/` and `?` search down and up, `n`/`N` next, `v` select, `y` copy, `q` quit |
| `Ctrl+B` `u` | Copy mode, one page up right away (`PgUp` works too) |
| `Ctrl+B` `E` | The pane's whole history in `$EDITOR` |
| `Ctrl+B` `c` … | Your commands from the config, e.g. `g` for lazygit |
| `Ctrl+B` `Ctrl+←↑↓→` | Resize by one cell (`Alt` for five), floating panes too |
| `Ctrl+B` `q` | Quit menu: detach, close the pane or end the session |
| `Ctrl+B` `d` | Detach, the shells keep running |
| `Ctrl+B` `Q` | Quit: end the session and every shell in it (asks first) |
| `Ctrl+B` `b` | Show or hide the sidebar |
| `Ctrl+B` `e` | Into the sidebar: `j`/`k` move, `Enter` go, `r` name, `x` close, `Esc` back |
| `Ctrl+B` `F` | Focus mode: only the focused pane, no bars, sidebar or borders; again to leave |
| `Ctrl+B` `s` | Session menu: switch, new, rename or end this session, detach, hives, save all as a hive |
| `Ctrl+B` `S` | Session list straight away: switch, end, rename, save as a hive |
| `Ctrl+B` `H` | Hives straight away: bring back, save, update, rename or delete saved groups of sessions |
| `Ctrl+B` `,` | Settings |
| `Ctrl+B` `R` | Reload the config file |
| `Ctrl+B` `?` | Show all keys |
| `Ctrl+B` `Ctrl+B` | Send `Ctrl+B` to the shell |

Pressing `Ctrl+B` opens a menu with every key, like which-key in Neovim. `f` and `w` open
submenus, `⌫` goes back.
Focus, move and resize repeat: for 600 ms after one of them, arrow keys work without the prefix.
A pane closes when its shell exits, imkr exits with the last pane.

## Sidebar

A sidebar on the right, like the rail in TUIOS, lists every window with its project (the
focused pane's directory) and what its agents are doing, every agent across all windows with
its window, directory and how long it has been in its state, and the git branch of the
focused pane with its changed files. Agents waiting for you come first. Enter or a click on a
changed file shows its diff in a floating pane, `o` in the sidebar opens it in `$EDITOR`. Click a row to go there. `Ctrl+B b` shows or
hides it (as in TUIOS), `Ctrl+B e` moves the keyboard into it, the settings put it left or right.

Below that is the file tree of the focused pane's project (its repository, or else its
directory), as in TUIOS: click a folder or press `Enter` on it to open it, `h` closes it again,
a file opens in `$EDITOR` in a floating pane. Changed files are coloured, `f` in the sidebar
jumps to the tree, the mouse wheel scrolls the sidebar. `Sidebar files` in the settings turns it
off.

## Agents

Every pane running a coding agent shows what it is doing: `● working`, `◆ blocked` (it waits
for you), `✦ done` (finished while you were elsewhere) or `✓ idle`. A window tab turns red when an agent there waits, the bell rings, and
`Ctrl+B a` takes you to it. You also get a desktop notification when an agent waits or finishes
while you look elsewhere or are detached: through `notify-send`, or as OSC 9 for terminals like
Ghostty and WezTerm, or not at all (setting `notifications = "system" | "terminal" | "off"`).

imkr recognises claude, codex, opencode, aider, gemini, crush, goose, amp, cursor-agent, qwen
and kilo. It guesses from their output and from questions on the screen. For exact states, let
the agent report them with hooks. For Claude Code:

```bash
cargo install --path .     # puts imkr on your PATH
imkr hooks              # prints the hooks for ~/.claude/settings.json
```

The hooks call `imkr status working|blocked|idle|clear`, which knows its pane from
`$IMKR_PANE` and does nothing outside imkr. It reads the hook's JSON from stdin: a subagent
finishing does not mark the agent done, and the session id is kept to resume the agent later.

Scripts and agents can drive imkr too:

```bash
imkr list [--json]                        # panes with program, state and directory
imkr send --pane 3 "run the tests"        # type into a pane, then Enter
imkr new --window 2 -- claude --resume    # start a command in a new pane
imkr new --float                          # a floating shell
imkr rename "tests"                       # name the pane this runs in
imkr wait --pane 3 --until ready          # wait until an agent is idle or done
imkr read --pane 3 --lines 20             # what it wrote last
imkr rename --window 2 api                # name a window
imkr notify "tests are green"             # a notification, titled after this pane
imkr notify --title build "done in 42 s"  # with a title of its own
```

`notify` goes out the way the settings say (a desktop notification through `notify-send`,
OSC 9 to the terminal, or none) and shows up in the control bar as well.

## Other machines

One imkr for the sessions here and on other machines, without an imkr inside an imkr: the
sidebar and the session list (`Ctrl+B S`) show every machine's sessions, and picking one switches
this terminal over to it. imkr runs on that machine and keeps its shells and agents going when the
connection drops; this terminal stays the client, so keys, clipboard, images and the window size
are local. On the other machine the list offers this one's sessions to come back to.

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
The sessions there are listed every 15 seconds with `imkr ls --json`, which needs this version
of imkr there too; the sidebar says if it is missing or the machine does not answer.

## Updates

A new version does not need a restart:

```bash
cargo install --path . --locked && imkr update   # from a checkout
curl -fsSL https://raw.githubusercontent.com/bastianhenneberg/imkr/main/install.sh | sh
imkr update                                      # from the latest release
```

`imkr update` tells the running server to become the new binary. It execs it in the same
process, which keeps every pane's pty, the socket and the attached clients' connections open, and
hands over layout, names and screens (history included) in a file. Shells and agents keep
running, the attached clients just redraw. Only the server is updated: a client started before
keeps its old code until it reattaches, fine as long as the protocol stays compatible.

## Restarts

imkr saves the layout to `~/.local/state/imkr/session.json` while it runs: windows,
splits, floating panes and each pane's directory. If the server dies with the machine, the next
`imkr` brings the layout back, shells start in their old directories, and Claude Code and
Codex sessions known from the hooks are resumed (`claude --resume <id>`). Processes and screen
contents cannot survive a restart. Ending the session on purpose (`Ctrl+B Q`, `kill-server`, or
exiting the last shell) deletes the file.

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

```bash
imkr hive save work       # the running sessions as the hive `work`
imkr hive ls              # every hive with its sessions and age
imkr hive restore work    # start what of `work` does not run
imkr hive rename work api
imkr hive rm api
```

`imkr save` and `imkr restore` are short for `hive save` and `hive restore`. Hives live in
`~/.local/state/imkr/saves/`, plain JSON, easy to copy or back up.

## Images

Programs that show images with the kitty graphics protocol (icat, chafa, yazi, timg, mpv's
kitty output, …) work in imkr when your terminal draws them: kitty, Ghostty and WezTerm.
imkr answers their question whether images work, passes the image data on and places the
images where the program put them. They scroll with the text, come back in the scrollback, and
hide while a menu or a floating pane covers them. In other terminals, and inside tmux, nothing
changes: programs hear no answer and fall back to text. `IMKR_GRAPHICS=1` or `0` overrides
the detection, the setting `images = false` turns it off.

## Mouse

The terminal's title shows the window and the focused pane, e.g. `imkr · 2 api · claude`.


- Click a pane to focus it, click a window tab to switch to it, `+` after the tabs opens a
  new window.
- The buttons in a pane's top border split it side by side (`┃`) or top and bottom (`━`), zoom
  it (`⤢`) or close it (`×`, asks first). Floating panes have `×` only.
- The sidebar's bottom border opens a new window, a new floating pane or the settings.
- Drag the border between two panes to resize them.
- Drag a floating pane by its title bar, resize it by its bottom right corner.
- Drag over text to select it, it is copied when you let go. A double click copies a word.
- The wheel scrolls back through the history, typing jumps back to the live screen.
- Programs that use the mouse themselves, like nvim or htop, get the events. Hold Shift to
  select text in them anyway.

Copying uses OSC 52, so it works over SSH as well. Your terminal has to allow it (kitty,
Alacritty, Ghostty, foot and WezTerm do).

## Settings

`Ctrl+B` `,` opens the settings. Changes apply right away and are saved to
`~/.config/imkr/config.toml` (or `$XDG_CONFIG_HOME/imkr/`, or `$IMKR_CONFIG`).
The file can also be edited by hand, imkr reads it when the server starts:

```toml
theme = "omarchy"   # "imkr", "omarchy" to follow the desktop, or an Omarchy theme name
renumber_windows = false   # true: windows move up to fill gaps, always 1, 2, 3 …
sessions_on_start = false  # true: plain `imkr` opens the session list first
hosts = []          # other machines whose sessions to show, see "Other machines"

[which_key]
enabled = true      # show the key menu after Ctrl+B
position = "left"   # "left" or "right" bottom corner

[sidebar]
enabled = true
side = "right"      # "left" or "right"

[bars]
control = "bottom"  # imkr badge, mode and hints: "top" or "bottom"
tabs = "bottom"     # window tabs: "top", "bottom" or "off"
path = "bottom"     # directory of the focused pane: "top", "bottom" or "off"

[spacing]           # in cells, all 0 by default; a cell is about twice as tall as wide
outer_x = 0         # columns left and right of the panes
outer_y = 0         # rows above and below the panes
gap_x = 0           # columns between panes side by side, and before the sidebar
gap_y = 0           # rows between panes one above the other
top_bar = "edge"    # "edge": the outermost line, full width; "inset": inside the margin
bottom_bar = "edge"
```

Two columns look about as wide as one row is tall, so `outer_x = 2, outer_y = 1, gap_x = 2,
gap_y = 1` gives even spacing all round. imkr spaces in whole cells, the terminal's own
padding (e.g. Ghostty's `window-padding-x/y`) comes on top: a bar on the `edge` only touches the
window's edge when that padding is 0.

Themes: `imkr` is honey on your terminal's colours. `omarchy` follows the current Omarchy theme
and changes with it. Any installed Omarchy theme (`catppuccin`, `gruvbox`, `tokyo-night`, …) can be
picked by name, its `colors.toml` provides the colours. In the settings, ←/→ step through them.
Themes colour more than the accent: every window has its own colour in tabs and sidebar,
every mode its own badge, floating panes their own border, and the bars a surface of their
own. The settings show the chosen theme's colours.

Your own commands go in the `Ctrl+B c` menu, run with `sh -c` in the focused pane's directory,
in a floating pane unless `float = false`:

```toml
[[commands]]
key = "g"
name = "lazygit"
command = "lazygit"

[[commands]]
key = "t"
name = "tests"
command = "cargo test"
float = false
```

Elements on the same side share one line. New panes start in the directory of the focused
pane.

## Build

```bash
cargo build --release
cargo install --path . --locked   # put this checkout on PATH as `imkr`
cargo clippy --all-targets -- -D warnings
cargo test
```

A tag `vX.Y.Z` matching the version in `Cargo.toml` builds the release binaries and attaches
them to its GitHub release, which is where `install.sh` downloads from.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
