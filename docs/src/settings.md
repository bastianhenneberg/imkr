# Settings

`Ctrl+B` `,` opens the settings. Changes apply right away and are saved to
`~/.config/imkr/config.toml` (or `$XDG_CONFIG_HOME/imkr/`, or `$IMKR_CONFIG`).
The file can also be edited by hand, imkr reads it when the server starts:

```toml
theme = "omarchy"   # "imkr", "omarchy" to follow the desktop, or an Omarchy theme name
renumber_windows = false   # true: windows move up to fill gaps, always 1, 2, 3 …
sessions_on_start = false  # true: plain `imkr` opens the session list first
session_banner = true     # the session's name on screen for a moment after a switch
session_banner_at = "middle"  # or "top", "bottom": the middle of that third
notes_at = "right"  # the notepad (Ctrl+B t): "right", "middle" or "bottom" of its pane
nerd_font = false   # the terminal font is a Nerd Font: round agent dots (else ❶, an oval in many fonts)
brain_inbox = true  # agent events also in AI Brain's inbox, see docs/ai-brain.md (needs the harness)
hosts = []          # other machines whose sessions to show, see docs/machines.md
name = "laptop"     # this machine's name for the others, e.g. its ssh alias; default: hostname
computer_use = "off"          # may agents see and drive this screen: "off", "ask", "on"
computer_use_monitor = "AGENT"  # which screen they mean; default: the focused one

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
plugin_status = true  # plugins with bar = true show their status in the control bar

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

*Shared pane borders* in the settings draws one border between panes side by side instead of
two, with junctions where borders meet (`┬ ├ ┤ ┴ ┼`), when there is no gap between panes.

Elements on the same side share one line. New panes start in the directory of the focused
pane.

Hooks run a shell command in the background when something happens: an agent becomes `blocked`,
`done`, `working` or `idle`, a pane opens or closes (`pane-opened`, `pane-closed`), another
window is shown (`window`), a terminal attaches or detaches (`attach`, `detach`). They get
`IMKR_EVENT`, `IMKR_SESSION` and, for a pane, `IMKR_HOOK_PANE`, `IMKR_HOOK_PROGRAM`,
`IMKR_HOOK_STATE` and `IMKR_HOOK_WINDOW`; a misspelt event is reported when the config loads:

```toml
[[hooks]]
on = "blocked"
run = 'paplay /usr/share/sounds/freedesktop/stereo/bell.oga'

[[hooks]]
on = "done"
run = 'imkr msg "$IMKR_SESSION/reviewer" "Pane $IMKR_HOOK_PANE is done, please review."'
```

For scripts outside imkr, `imkr subscribe` gives the same as JSON lines.


## Settings that stay on their machine

Most settings travel with `imkr settings sync` ([machines](machines.md)). A few are answers
about *this* machine and would be wrong anywhere else, so they never go either way:

| | |
|---|---|
| `name` | what the other machines call this one |
| `hosts` | the machines this one shows |
| `web`, `web_listen` | whether and where the phone view listens |
| `computer_use`, `computer_use_monitor` | whether agents may use this screen, and which one — see [seeing and driving the screen](computer-use.md) |

A machine nobody sits at can stand on `computer_use = "on"` for good while your laptop keeps
asking, and a sync between them changes neither. `Ctrl+B s e` steps that setting on the machine
you are looking at, and `Ctrl+B s E` gives the half hour right away — both stay there too.

## The shared folder

Some things imkr makes are for a person to look at — a screenshot, so far. The machine that
made it is often not the machine you are sitting at, so imkr needs somewhere both can reach.
That is one folder, and it is nothing fancy: an SMB or NFS mount, Syncthing, Nextcloud,
iCloud, or a plain folder on the machine you work at. imkr mounts nothing and does not mind
which it is.

```bash
imkr share setup        # offers the folders this machine already has
imkr share              # where it is, and where the same folder is elsewhere
imkr screen shot --save # a picture goes there instead of into imkr's working files
```

`Ctrl+B` `s` `f` opens the same setup in a popup. It lists what it finds — network mounts
from the kernel's own mount table, and the folders the usual syncing tools make — and never
picks one for you when there is more than one.

It lives in `~/.config/share-dir.toml`, **not** in imkr's config:

```toml
dir = "~/Development/codingshare"

[elsewhere]
laptop = "/Users/you/Development/codingshare"
desktop = "/home/you/Development/codingshare"
```

`dir` is this machine's path. `[elsewhere]` is the same folder as your other machines see it,
by the name you call them — that is what lets imkr print `on laptop: …` next to a file it
just wrote, instead of a path that only exists somewhere you are not.

```bash
imkr share elsewhere laptop /Users/you/Development/codingshare
imkr share elsewhere laptop --rm
```

The file is deliberately nobody's: imkr reads and writes it, and so does the AI Brain
harness, and each works on its own with it or without it. Two programs that need the same
answer should not have to ask each other for it. The field names are a contract —
`imkr share --json` prints them — and anything another program adds to the file survives
imkr writing it.

Without the file nothing is written anywhere, `--save` says so and names the setting, and
everything else keeps working.
