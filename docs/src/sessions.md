# Sessions, windows and panes

## Getting around

```bash
imkr              # attach to the session, or start one
imkr attach       # attach, fail if there is no session
imkr kill-server  # stop the server and every shell in it
imkr keys         # list the key bindings
imkr version      # what is installed
imkr upgrade      # fetch the latest release and become it
imkr -s work      # the session `work`, started if needed
imkr ls           # the running sessions with panes and agents
```

The first `imkr` starts a server in the background. It owns the shells and keeps them
running after you detach or close the terminal. The socket lives in
`$XDG_RUNTIME_DIR/imkr/`, set `IMKR_SOCKET` to use another one. Several terminals can attach
to the same session, as in tmux: they all show it and type into it, at the size of the smallest
one (the rest of a larger terminal is filled with dots), and the control bar says how many are
attached. Each terminal shows its own window: switching windows in one leaves the others where
they are (setting *Own window per attached terminal*, off for tmux's behaviour, where all show
the same). `Ctrl+B d` detaches only the terminal it was pressed in, `imkr attach -d` detaches all
others.

Sessions are separate servers with their own panes, socket and saved layout. `imkr -s NAME`
(or `attach -t NAME`) picks one, `Ctrl+B S` switches between them without leaving the terminal,
and named sessions show their name in the control bar. In that menu `n` starts a new session (on
which machine, if others are known, is asked first),
`x` ends the selected one, `r` renames it (like `imkr rename-session NAME`, `-s OLD` for
another one), `s` saves all running sessions as a hive and `h` opens the hives.
Every list (sessions, agents, hives, marks, machines) takes `/` to search: typing narrows it to
what matches every word, in name, machine, session or directory, Esc clears the search.

`Ctrl+B w m` moves the active window to another session: pick a running one, or `n` for a new
one, started for it. E.g. to gather the windows of one project in a session of their own
(`imkr move-window --window N NAME` from a script). Its programs keep running, nothing restarts: the ptys are passed to the other
server over its socket. The window keeps its name and, if free, its number. Agents in it keep
reporting to the session they came from, which passes it on while it runs; after that their
state comes from what imkr sees on the screen.

`Ctrl+B w t` asks for a branch and opens a window of that name in a git worktree of the focused
pane's repository: `<repo>--<branch>` next to it, a new branch from the current commit or the
existing one, or the worktree made before. So agents work side by side without touching each
other's files. From a shell, also with what to run in it:

```bash
imkr worktree fix-login -- claude     # window "fix-login" in ../api--fix-login, running claude
```

Worktrees stay when the window closes; `git worktree remove` deletes one once its work is merged.

A fleet puts several agents on the same task, each in a worktree of its own, to pick the best
result:

```bash
imkr fleet speed -n 3 "make the start-up faster"   # windows speed-1 … speed-3 running claude
imkr fleet speed --diff                            # what each one committed and changed since
```

`--agent codex` (or any agent that takes the prompt as its argument) runs another one.

With *Git checkpoint after each agent turn* on in the settings, imkr keeps the state of an
agent's repository each time it finishes a turn: a commit of every file, new ones too, on
`refs/imkr/checkpoints/<session>-<pane>`, only when something changed. Your branch, index and
files stay as they are. `imkr checkpoints` lists them; `git diff <id>` shows what changed since
one and `git restore --source=<id> -- <path>` brings a file back as it was.

| Key | Action |
|---|---|
| `Ctrl+B` `%` (or `#`, `\|`) | Split the pane side by side |
| `Ctrl+B` `"` (or `-`) | Split the pane top and bottom; `#` and `-` need no Shift on a German keyboard |
| `Ctrl+B` `x` | Close the pane (asks first) |
| `Ctrl+B` `r` | Name the pane (`Ctrl+B` `w` `r` names the window) |
| `Ctrl+B` `z` | Zoom: the pane alone over the whole window, again to go back |
| `Ctrl+B` `{` / `}` | Swap the pane with the previous / next one |
| `Ctrl+B` `>` `1`–`9` / `n` | Move the pane into that window (made if needed) / a new one; it keeps running, a window left empty goes |
| `Ctrl+B` `=` | Give all panes equal sizes |
| `Ctrl+B` `w` `l` | Layout for this window: `even-horizontal`, `even-vertical`, `main-vertical`, `main-horizontal`, `tiled` as in tmux, or one saved with `s`. Its panes take the places in order and keep running, places left get a shell; `imkr layout NAME`, `imkr layout save NAME`, `imkr layout ls` from a shell |
| `Ctrl+B` `f` … | Floating panes menu: `f` float/tile, `n` new, `o` next, arrows move |
| `Ctrl+B` `Shift+←↑↓→` | Move a floating pane |
| `Ctrl+B` `n` | New window, asks for its name right away (Esc closes it again) |
| `Ctrl+B` `N` | New window in another session, on any machine: pick it, go there, name the window |
| `Ctrl+B` `Space` | Show or hide the popup used last |
| `Ctrl+B` `z` | Over a popup: fill everything but the bars, and back |
| `Ctrl+B` `!` | Popup ⇄ pane: the shown popup becomes a window of its own, else the focused pane a popup |
| `Ctrl+B` `w` … | Windows menu: `1`–`9`, `c` new, `n`/`p` next/previous, `m` move to another session, `=` renumber 1, 2, 3 … |
| `Ctrl+B` `1`–`9` | Go to window, an empty one starts a shell |
| `Ctrl+B` `←↑↓→` | Focus the pane in that direction |
| `Ctrl+B` `o` | Focus the next pane |
| `Ctrl+B` `;` | Back to the pane focused before, across windows |
| `Ctrl+B` `g` | Go to any pane: type to filter by window, name, program, state or directory |
| `Ctrl+B` `/` | Search everything: panes, windows, sessions and agents on every machine, plugins, marks, hives and every command with its keys. Words in any order, Enter goes there or does it |
| `Ctrl+B` `a` | Jump to the agent that has waited longest for you, then to finished ones |
| `Ctrl+B` `A` | Every agent here and on other machines, those waiting first: Enter or digit goes there |
| `Ctrl+B` `s` `l` | While you were away: each agent that did something since the last terminal detached, waiting ones first, with when it started (●), asked (◆) and finished (✦). Opens by itself when you come back after a minute or more and something happened |
| `Ctrl+B` `v` | Review the changes of the focused pane's repository (against the last commit, new files too): `j` `k` move, `n` `p` next / previous file, `c` notes the line, `x` removes the note, `s` sends all notes to the pane as one prompt, typed in once its agent is idle. Notes not sent stay for the next review |
| `Ctrl+B` `i` | Inbox: the agents on every machine that wait for you, then those done, each with its question. A digit picks that option, typed text and Enter answer, Enter alone goes there. Answers only ever go to the agent whose question is shown |
| `Ctrl+B` `W` | Every window everywhere, in every session on every machine, with its agents' state and programs: Enter or digit goes there, `/` searches |
| `Ctrl+B` `P` | Every pane everywhere, after its window (`1 api › claude`): Enter or digit goes there, `/` searches |
| `Ctrl+B` `m` / `M` | Mark the pane for `Alt+1` … `Alt+9`, like Harpoon / list the marks: Enter goes there, `J` `K` reorder, `x` removes |
| `Alt+1` … `Alt+9` | Straight to a marked pane, in any session, on any machine (setting *Marks on Alt+1-9*) |
| `Ctrl+B` `[` or `y` | Copy mode: scroll back with vim keys (`Ctrl-U`/`Ctrl-D`, `Ctrl-B`/`Ctrl-F`, `g`/`G`), `w`/`b`/`e` by words, `/` and `?` search down and up, `n`/`N` next, `s` jump (Enter or Space takes the yellow, nearest match), `v` select, `V` select lines, `y` copy, `q` quit. A board in the corner shows all of these keys; `i` hides it (setting "Copy-mode keys") |
| `Ctrl+B` `j` | Jump like flash.nvim: type a few characters, every match in every pane on screen gets a label, its key puts the copy cursor there, into the other pane too. To mark: jump, `v`, jump again (`s`) to where it ends, `y` |
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
submenus, `⌫` goes back. Without remembering a key: `Enter` (or `↓`/`↑` in submenus, where the
arrows mean nothing else) picks from the menu with the arrows: `↑`/`↓` in a column, `←`/`→` to the
column beside, `Enter` or `Space` runs the entry and picks on in a submenu, `⌫` goes back. After
`Ctrl+B` alone the arrows still change the pane.
Focus, move and resize repeat: for 600 ms after one of them, arrow keys work without the prefix.
A pane closes when its shell exits, imkr exits with the last pane.

## Restarts

imkr saves the layout to `~/.local/state/imkr/session.json` while it runs: windows,
splits, floating panes and each pane's directory. If the server dies with the machine, the next
`imkr` brings the layout back, shells start in their old directories, and agents are resumed:
Claude Code, Codex and pi by the session id their hooks report (`claude --resume <id>`,
`pi --session <id>`), opencode,
aider, goose and cursor-agent with their last conversation in that directory. *Resume agents
after a restart* in the settings starts them, only types the command for you to start with
Enter, or leaves them off; `[resume_commands]` in the config sets the command per agent
(`{session}` for the id, `""` turns one off), e.g. `gemini = "gemini --resume latest"`. Each pane's history
comes back too, as text above a line *restored after a restart*: the last 3000 lines are kept
every 20 seconds in `<session>.scrollback/` next to the file, readable by you only. Processes
cannot survive a restart. Ending the session on purpose (`Ctrl+B Q`, `kill-server`, or
exiting the last shell) deletes the file and keeps the layout in the hive `last`.

`Ctrl+B S` says how long a session's agents have been quiet, e.g. `✓1 2d ago`, so an old one
stands out among many; a session without agents shows neither time nor state. `o` hides the old
ones and shows them again: those not running. A running session always shows, however long its
agents have been quiet, as in the sidebar. `a` switches to the archive, sessions ended before the
ten in the hive `last`, grouped by hive; `Ctrl+B s a` opens it directly (see [Hives](hives.md)).
Saving all sessions as a hive is `Ctrl+B s v`. `imkr ls` gives the
time too.

When the session you are in ends, imkr does not drop you: you land in another session running
on this machine, with the session list open, to go on from there (`Esc` stays, `Ctrl+B d`
leaves). Only when none runs does imkr end, and plain `imkr` then starts afresh instead of
bringing back an empty copy of the ended one.

Sessions that do not run are listed with `◌`, in `Ctrl+B S` as a group of their own after the
running ones (`o` hides them and quiet ones) and in `imkr ls`; the sidebar shows running ones only:
*stopped* ones (the machine restarted, or the server crashed) and *closed* ones (ended on
purpose). Enter brings one back with its layout and agents, `x` forgets it. After the machine
restarted, the first `imkr` brings back every session that ran when it went down, not just the
one it goes to (setting *Restart all sessions after a reboot*); closed ones stay closed. From a
shell:

```bash
imkr ls              # ◌ deckbox   stopped 21:57, 2 panes in 2 windows
imkr revive deckbox  # back, without attaching
imkr forget deckbox  # its saved layout is deleted
```
