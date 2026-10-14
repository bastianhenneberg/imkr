# imkr 🐝

A terminal multiplexer and window manager for humans and their coding agents, written in Rust.
Say it like *Imker*, German for beekeeper: it tends the hives your agents work in.

imkr combines tiling and floating panes with a tmux-style prefix, and keeps an eye on
the agents running in its panes: working, blocked, or idle.

The names follow tmux: a session holds windows, a window holds panes. A *hive*, a saved group
of sessions, is imkr's own, as tmux has nothing like it.

```text
Hive "work"                    saved, not running: a map of sessions on every machine
├── ⌂ laptop                   the machine you sit at, the operator
│   ├── Session "imkr"         one imkr server each, runs on when you detach
│   │   ├── Window 1 "claude"
│   │   │   ├── Pane           tiled in a split tree …
│   │   │   └── Pane           … or floating on top
│   │   ├── Window 2 "shell"
│   │   │   └── Pane
│   │   └── Popups             panes outside every window, plugins open here
│   └── Session "notes"
└── ⇄ server                   another machine, reached over ssh
    └── Session "api"
        └── Window 1
            └── Pane
```

Sessions, windows and panes run; a hive only remembers them, so bringing it back starts what is
missing, on whichever machine it belongs to.

## What sets imkr apart

Agent status, several machines and agents messaging each other are what multiplexers for
coding agents do now. What imkr adds on top, as far as the docs of herdr (0.9.4) and TUIOS
(0.9.2) show (checked on 12 and 13 October 2026):

- **Several accounts, and the next one when one is used up**: several Claude Code and Codex
  subscriptions on one machine, plus providers like OpenRouter or a local Ollama, a different
  one in every pane. When an agent hits its limit, imkr notes the account until it resets and
  goes on in the next free one, in the same conversation (`imkr account`, `Ctrl+B s c`).
- **Hives, a map of every machine**: one save holds the sessions of all your machines. It is
  shared between them, and `imkr hive restore` brings everything back from any of them, also
  on a new machine that takes the place of an old one (`--from`, `--take`). Switching hives
  (`imkr hive switch`) changes context: one customer's sessions come back, the other's end.
- **Move windows with their programs running**: `Ctrl+B w m` hands a window to another
  session, the processes keep running, nothing restarts (the terminal itself passes over).
- **You are the operator**: the machine you sit at (⌂) brings its plugins and session
  templates along to every machine you open, and every machine shows the same sidebar.
- **One search for everything**: `Ctrl+B /` finds panes, windows, sessions and agents on every
  machine together with plugins, marks, hives and every command. The capitals list each kind
  across all machines: `S` sessions, `A` agents, `W` windows, `P` panes, `M` marks, `H` hives
  (TUIOS lists across hosts too, with commands in a palette of their own).
- **Live update that keeps you connected**: `imkr update` execs the new binary in the running
  server; programs and screens stay, and so do the attached terminals (herdr's experimental
  handoff keeps programs but drops the clients).
- **Every machine on the phone**: `imkr web` shows the sessions of all your machines on one
  page, an inbox to answer waiting agents with a tap, any pane as a terminal, and the whole of
  imkr with a keyboard mode for a Bluetooth keyboard; served by one machine in your VPN and
  paired once with a QR code (TUIOS has browser access in a separate binary, its README tells
  no more).
- **Plugin catalogs of your own**: catalogs are any git repositories, private ones too, with
  catalog and plugins in one; `Ctrl+B p +` lists and adds them (herdr's marketplace indexes
  public GitHub repositories only).

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

imkr recognises claude, codex, opencode, aider, gemini, crush, goose, amp, cursor-agent, qwen,
kilo and pi. It guesses from their output and from questions on the screen. Other agents and
their questions go into the config (`extra_agents = ["my-agent"]`,
`question_phrases = ["Proceed now?"]`), and `imkr explain --pane N` says why a pane counts as
the agent it is, in its state, or as none. For exact states, let the agent report them with
hooks. For Claude Code:

```bash
imkr hooks --install    # puts them into ~/.claude, on every machine you run agents on
imkr hooks              # or just prints them for ~/.claude/settings.json
```

`--install` writes `~/.claude/hooks/imkr-agent-state.sh`, adds its hooks to
`~/.claude/settings.json` next to your other hooks (backed up as `settings.json.bak-imkr`, imkr's
own from before replaced), and works where `~/.cargo/bin` is only on interactive shells' PATH.
Waiting is reported both by permission requests and, for agents running with permissions
skipped, by the notification that a question waits. Agents started after it report their state.
Where pi is set up (`~/.pi/agent`), `--install` also puts an extension into its extensions that
reports pi's state the same way: working while it runs, done after its turn, waiting when an
error stays after pi's own retries.

The hooks call `imkr status working|blocked|idle|clear`, which knows its pane from
`$IMKR_PANE` and does nothing outside imkr. It reads the hook's JSON from stdin: a subagent
finishing does not mark the agent done, and the session id is kept to resume the agent later. The
transcript's path comes along too: imkr reads the model and context of the agent's last answer
from its end and shows them as `opus 5.5 · 300k` in `Ctrl+B A`, the inbox, `imkr agents` and,
for the focused agent, in the sidebar.

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

### Several accounts, and the next one when one is used up

One machine, several Claude Code and Codex subscriptions, and providers Claude Code reaches by
address and key (OpenRouter, a local Ollama). Each account is an environment the agent starts
with: a subscription gets a directory of its own (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) holding only
its login, and everything else links to `~/.claude` or `~/.codex`, settings, hooks, skills, MCP
servers and the conversations. So a conversation goes on in another account where it stopped.
`~/.claude` itself is the account `standard`.

```bash
imkr account add claude privat          # logs it in: Claude Code starts in it, /login, /exit
imkr account add codex work             # `codex login` in it
echo "$KEY" | imkr account add claude router --url https://openrouter.ai/api \
                                             --model qwen/qwen3-coder --token-stdin
imkr account add claude local --url http://localhost:11434 --token ollama --model qwen3-coder
imkr account                            # every account, in the order to fall back in
imkr account use privat                 # new shells of this session start in it
imkr account switch privat --pane 3     # the agent in pane 3 goes on in it, now
imkr account run privat -- claude       # one command in it
imkr account sync                       # share what is new in ~/.claude, MCP servers again
```

`Ctrl+B s c` lists them: Enter puts the focused agent into the chosen account, `u` new shells of
this session. Switching ends the agent and starts it again in the same conversation, typed into
its shell as `imkr account run claude:privat -- claude --resume <id>`, so no key shows on the
screen; the session id comes from the hooks.

When an agent says its account is used up ("5-hour limit reached ∙ resets 7pm", Codex: "You've
hit your usage limit"), imkr notes that account as used up until then and, as *Account used up*
in the settings says, tells you which account is next (`ask`, the default), goes on in it by
itself (`auto`), or does nothing (`off`). The next one is the next in the list that is logged in
and not used up, round. Accounts and keys live in `~/.local/state/imkr/accounts/` (0600).

### Agents talking to each other

Agents in imkr can find, read and message each other, on this machine and on the others:

```bash
imkr agents --all                                  # every agent with its address
imkr msg server:api/claude "The API returns 409 on duplicates now, please update the client."
imkr msg --wait api/3 "Which port does the dev server use?"   # waits, prints its screen
imkr msg server:api/claude < handover.md                       # a long text from stdin
```

An address is `machine:session/pane`; in the own session a pane number, name or program is
enough. imkr types the message in once the agent is idle, one at a time, never while it works
and never into a question it asks, which stays yours. Each message says who sent it and how to
answer. `imkr skill` prints the guide for agents, `imkr skill --install` puts it into Claude
Code's skills, so the agents use it on their own. `imkr read server:api/3` shows another
agent's screen, on any machine.

Answers find their way back without every machine reaching every other: a server needs no ssh
to your laptop. A message for a machine out of reach waits in the sender's outbox (`imkr
outbox`), and the machine it is for picks it up when it next asks that one, every 15 seconds
while its imkr runs, so a laptop that was offline gets it later.

### MCP

`imkr mcp` serves the same as tools over MCP on stdin/stdout: `imkr_agents`, `imkr_panes`,
`imkr_read`, `imkr_send`, `imkr_message`, `imkr_run`, `imkr_wait` and `imkr_new_pane`. Each runs
imkr's own command, so addresses on other machines work as on the command line, and text never
goes through a shell. For Claude Code:

```bash
claude mcp add imkr -- imkr mcp
```

Started in an imkr pane, the server knows its session and pane by itself.

### tmux for agent teams

Programs that drive tmux, like Claude Code's agent teams, can open imkr panes instead. Turn on
*tmux for agent teams* in the settings: panes started after that find imkr as `tmux` on their
PATH, with `$TMUX` and `$TMUX_PANE` set, and `split-window`, `new-window`, `send-keys`,
`capture-pane`, `kill-pane`, `display-message`, `list-panes`, `list-windows`, `select-pane`
and `rename-window` work on imkr panes (`%3` is pane 3); layout and option commands are
accepted and do nothing, `kill-session` and `kill-server` are refused. It is off by default,
since programs that see `$TMUX` act as if inside tmux. Tested with scripts that use these
commands, not yet with every program that drives tmux.

### Running commands in a pane

With shell integration (OSC 133), imkr knows where a command's output starts and ends and how it
ended. Many prompts send it already; otherwise add one line to the shell's rc file:

```bash
eval "$(imkr shell-integration zsh)"   # or bash; does nothing outside imkr
```

Then a script or an agent can use a shell in another pane like its own:

```bash
imkr run --pane 3 -- cargo test   # types it in, waits, prints the output, exits with its code
imkr wait --pane 3                # waits for the command running there (--timeout S)
```

### Copying files

`imkr cp` copies like scp, and also to and from the directory a pane works in, here or in
another session or on another machine:

```bash
imkr cp notes.md @3                     # into pane 3's directory
imkr cp report.pdf @server:api/claude   # into the directory of claude in session api on server
imkr cp @api/claude::target/out.log .   # a file below that pane's directory, here
```

### Events

`imkr subscribe` prints what changes in the sessions on this machine, one JSON object per line,
for scripts, status bars and agents that wait for each other (`-s NAME` watches one session,
`--snapshot` lists every pane first):

```json
{"event":"state","session":"api","pane":1,"program":"claude","window":2,"from":"working","state":"blocked","time":1791843836}
```

Events are `state` (an agent's state changed, `null` without one), `pane-opened`, `pane-closed`
and `pane-moved` (to another window, with `from_window`).

## Clipboard

Text you select is copied with OSC 52, so it lands in the clipboard of the machine your terminal
runs on, over ssh too. What programs in panes copy the same way, nvim or an imkr on another
machine reached by `ssh` in a pane, is passed on to your terminal (like tmux's `set-clipboard`);
requests to read the clipboard are not.

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

## Plugins

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
enabled = true                    # false keeps the file but leaves it out
```

The sidebar lists every plugin under *Plugins*, `◇` until it runs and `◆` after, with its status
lines below. A click or its key starts it the first time and shows it, the same again hides it;
it runs on hidden, keeps its state, and has every popup key (`Ctrl+B z`, `!`, `Space`).
`Ctrl+B p` lists the plugins' keys. Since a plugin is just a program, it can be written in
any language and cannot take imkr down; a file with a mistake shows in the sidebar with what is
wrong, `imkr plugins` lists them all, and `Ctrl+B R` reads them again.

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

## Other machines

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

## On the phone

`imkr web` shows this machine's sessions in a browser, made for phones: the agents that wait for
you or are done, with their last lines and buttons to answer (`1`, `2`, `y`, Enter, Esc, or a
line of text), every session with its windows and panes, and any pane as a terminal (xterm.js)
to watch and type into, with a row of the keys a phone lacks (Esc, Tab, Ctrl, arrows) and
Nerd Fonts' symbols built in, so prompts show their icons. *Fit*
gives the pane the phone's size while you look at it, else the page shrinks the pane's text to
fit. It works in any browser, Android or iPhone, and nothing is set up besides: the server is
part of the imkr binary and is updated with it (`imkr update` and `imkr hosts sync`).

The other machines imkr knows (`hosts`, and those reached with `--host`) are on the page too,
grouped by machine: their agents in the inbox, their sessions and panes, reached through ssh as
the session list reaches them. So one machine with `imkr web` is enough; pair the phone with the
one that is always on.

A firewall on the machine must let the port in, on the VPN only. With ufw and NetBird:
`sudo ufw allow in on wt0 to any port 4657 proto tcp` (Tailscale: `tailscale0`). `imkr web` and
`imkr web pair` say so when ufw is on and the port is not let in. What it turns away (wrong
code, unknown device, another site's page) goes into `~/.local/state/imkr/web.log` with the time
and the device's address, as do pairings: if the page never shows and the log has nothing, the
request did not reach the machine.

It listens on this machine's VPN address if there is one (Tailscale and NetBird both hand out
addresses from 100.64.0.0/10), else only on 127.0.0.1, port 4657; `--listen` or `web_listen`
picks another address. Put it on a VPN, not the open internet: with the phone in the same
tailnet or NetBird network, `http://<address>:4657` is all it takes. In the home Wi-Fi,
`--listen 192.168.1.10` works too; `0.0.0.0` (every network) only with care.

```bash
imkr web                 # serve; prints the address, and a code if no device is paired yet
imkr web pair            # a QR code and code to pair another device, ten minutes, five tries
imkr web devices         # the paired devices
imkr web forget phone    # unpair one (or `all`)
```

*Whole session*, at the top of an opened session, shows imkr itself in the browser: every
window, the sidebar, `Ctrl+B` and its menus, as in a terminal. With a Bluetooth keyboard (and
glasses, or the phone sideways) that is a full workplace: ⌨ in the terminal's bar switches to
keyboard mode, full screen without the key bar and the line to type in; ⌨ ✕ in the corner, or
leaving full screen, switches back. Over plain HTTP the browser may not keep the screen awake:
set a longer screen timeout on the phone for that.

While a device has the page open, the sidebar of the sessions here lists it under *Phones*, with
the pane or session it shows.

Pairing gives the device a token in a cookie; only paired devices see anything or type into a
pane. *Web view for phones* in the settings (`web = true`) keeps it running: every imkr server
starts it when nothing answers on its address, after a reboot too, and it moves to a new binary
by itself after an update.

```toml
web = true
web_listen = "100.97.56.232:4657"   # optional
```

Without HTTPS a browser will not install the page as an app; it works as a page all the same. For
HTTPS put it behind `tailscale serve 4657` or the reverse proxy of your NetBird or other server.

## Updates

A new version does not need a restart:

```bash
imkr upgrade                                     # the whole thing, in one
cargo install --path . --locked && imkr update   # from a checkout
```

`imkr upgrade` asks for the newest release, stops right there if it is the one already
running, and otherwise downloads the archive for this machine, checks its sha256, and
replaces the binary it was started from — by renaming inside the same directory, so nobody
ever sees a half-written file. It then moves a running session over, the same way `imkr
update` does. The installer is still a way to update (it overwrites the binary it finds),
but it writes to `~/.local/bin`, which shadows a cargo install rather than replacing it.

`imkr update` tells the running server to become the new binary. It execs it in the same
process, which keeps every pane's pty, the socket and the attached clients' connections open, and
hands over layout, names and screens (history included) in a file. Shells and agents keep
running, the attached clients just redraw. Only the server is updated: a client started before
restarts itself as the new binary within a few seconds, at the same place, the session on another
machine included (clients from before 0.7 keep their old code until they attach again).

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
ones (not running, no agents, or quiet for 12 hours) and shows them again. `imkr ls` gives the
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
every other machine known (see *Other machines*); one that is off keeps what the hive had of it.
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

## Settings

`Ctrl+B` `,` opens the settings. Changes apply right away and are saved to
`~/.config/imkr/config.toml` (or `$XDG_CONFIG_HOME/imkr/`, or `$IMKR_CONFIG`).
The file can also be edited by hand, imkr reads it when the server starts:

```toml
theme = "omarchy"   # "imkr", "omarchy" to follow the desktop, or an Omarchy theme name
renumber_windows = false   # true: windows move up to fill gaps, always 1, 2, 3 …
sessions_on_start = false  # true: plain `imkr` opens the session list first
session_banner = true     # the session's name on screen for a moment after a switch
session_banner_at = "middle"  # or "top", "bottom": the middle of that third
hosts = []          # other machines whose sessions to show, see "Other machines"
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
