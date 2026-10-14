# Sidebar, popups, mouse and clipboard

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

## Popups

A popup is a pane outside every window, shown over the active one: a TUI you want at hand
without giving it a pane or a window. `imkr popup --name pm -- peppermint-tui` starts it the
first time and shows it; the same command, or `Ctrl+B Space`, hides it and shows it again. It
keeps running while hidden, so the program keeps its state, and it survives `imkr update`. Keys,
pastes and the mouse go to it while it is shown; a click outside hides it. `Ctrl+B z` makes it
fill everything but the bars, sidebar included, and back; its frame then says `popup · full`.
`Ctrl+B !` turns a popup into the pane of a window of its own, named after it, for when it should
stay, and back: on a pane without a popup shown, `Ctrl+B !` makes the pane a popup. On a full
popup `Ctrl+B !` makes it small again first, like `z`, as a full popup already looks like a window. The sidebar lists the popups under *Popups*: a click
shows one, or hides it, `x` in the sidebar (`Ctrl+B e`) ends it. It closes when its program ends.
Plugins build on this.

## Mouse

The terminal's title shows the window and the focused pane, e.g. `imkr · 2 api · claude`.


- Click a pane to focus it, click a window tab to switch to it, `+` after the tabs opens a
  new window.
- The buttons in a pane's top border split it side by side (`┃`) or top and bottom (`━`), zoom
  it (`⤢`) or close it (`×`, asks first). Floating panes have `×` only.
- The sidebar's bottom border opens a new window, a new floating pane or the settings.
- Right click a pane for its menu (split, zoom, float, rename, move to another window, make it
  a popup, close), a window tab for the window's (rename, layout, move to another session, new
  window, worktree). Programs that use the mouse get the right click; Shift+right click opens
  the menu over them.
- Drag the border between two panes to resize them.
- Drag a floating pane by its title bar, resize it by its bottom right corner.
- Drag over text to select it, it is copied when you let go. A double click copies a word.
- The wheel scrolls back through the history, typing jumps back to the live screen.
- `Ctrl`+click opens a link: a URL written as text, or a hyperlink a program marks (OSC 8, like
  `ls --hyperlink` or Claude Code's file links). It opens on the machine you sit at, also when
  the session runs on another one; only `http(s)`, `mailto` and `file` links.
- Programs that use the mouse themselves, like nvim or htop, get the events. Hold Shift to
  select text in them anyway.

Copying uses OSC 52, so it works over SSH as well. Your terminal has to allow it (kitty,
Alacritty, Ghostty, foot and WezTerm do).

## Clipboard

Text you select is copied with OSC 52, so it lands in the clipboard of the machine your terminal
runs on, over ssh too. What programs in panes copy the same way, nvim or an imkr on another
machine reached by `ssh` in a pane, is passed on to your terminal (like tmux's `set-clipboard`);
requests to read the clipboard are not.

## Images

Programs that show images with the kitty graphics protocol (icat, chafa, yazi, timg, mpv's
kitty output, …) work in imkr when your terminal draws them: kitty, Ghostty and WezTerm.
imkr answers their question whether images work, passes the image data on and places the
images where the program put them. They scroll with the text, come back in the scrollback, and
hide while a menu or a floating pane covers them. In other terminals, and inside tmux, nothing
changes: programs hear no answer and fall back to text. `IMKR_GRAPHICS=1` or `0` overrides
the detection, the setting `images = false` turns it off.
