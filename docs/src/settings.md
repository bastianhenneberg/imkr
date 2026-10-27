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
