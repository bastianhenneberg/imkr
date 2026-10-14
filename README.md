# imkr 🐝

A terminal multiplexer and window manager for humans and their coding agents, written in Rust.
Say it like *Imker*, German for beekeeper: it tends the hives your agents work in.

imkr has tmux's prefix keys and sessions, tiling and floating panes, and an eye on every coding
agent in them: working, waiting for you, or done. On this machine and on all your others.

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

The names follow tmux: a session holds windows, a window holds panes. A *hive*, a saved group of
sessions across machines, is imkr's own.

## What it does

- **Knows your agents.** Claude Code, Codex, opencode, aider, gemini, pi and more show as
  `● working`, `◆ blocked` or `✦ done`. One key (`Ctrl+B a`) takes you to the one that waited
  longest; the inbox (`Ctrl+B i`) answers them without going there.
- **Every machine in one place.** The sessions and agents of all your machines in one sidebar,
  one search (`Ctrl+B /`) and one inbox. Switching goes over ssh, your terminal stays local.
- **Agents talk to each other.** `imkr msg server:api/claude "…"` types a message into another
  agent once it is idle, on any machine. Also to a colleague's imkr, over a VPN or through
  AI Brain, and only after they accept it.
- **Several accounts.** Several Claude Code and Codex subscriptions, or OpenRouter and a local
  Ollama, one per pane. When an agent hits its limit, imkr goes on in the next free account in
  the same conversation.
- **Hives.** Save the sessions of every machine under one name and bring them back anywhere,
  also on a new machine that replaces an old one. Switching hives changes from one customer's
  work to another's.
- **Nothing restarts.** `imkr update` swaps the binary under running shells and attached
  terminals. Windows move between sessions with their programs running. After a reboot the
  layout comes back and agents resume their conversations.
- **On the phone.** `imkr web` puts every machine's agents, an inbox and any pane into a browser,
  in your VPN, paired once with a QR code.
- **Plugins** are plain programs in a popup, from a file or a git catalog of your own.

## Install

Linux on x86_64 or aarch64:

```bash
curl -fsSL https://raw.githubusercontent.com/bastianhenneberg/imkr/main/install.sh | sh
```

This puts the latest release into `~/.local/bin`. With a Rust toolchain:
`cargo install --git https://github.com/bastianhenneberg/imkr --locked`.
More in [docs/install.md](docs/install.md).

## First steps

```bash
imkr                     # start a session, or go back to the last one
imkr hooks --install     # exact agent states from Claude Code (and pi)
imkr skill --install     # teach Claude Code to message other agents
```

Then press `Ctrl+B`: a menu lists every key, and `Enter` lets you pick with the arrows.

| Key | |
|---|---|
| `Ctrl+B` `%` / `"` | split side by side / top and bottom (`#` / `-` on a German keyboard) |
| `Ctrl+B` `n` | new window |
| `Ctrl+B` `a` | the agent that waited longest |
| `Ctrl+B` `i` | inbox: answer waiting agents |
| `Ctrl+B` `/` | search everything, on every machine |
| `Ctrl+B` `S` | sessions, here and on other machines |
| `Ctrl+B` `,` | settings |
| `Ctrl+B` `d` | detach, everything keeps running |

Another machine: `imkr --host server` (ssh without a password), and it shows in the sidebar from
then on.

## Documentation

| | |
|---|---|
| [Install and update](docs/install.md) | installer, live updates, building |
| [Sessions, windows and panes](docs/sessions.md) | every key, worktrees, fleets, checkpoints, restarts |
| [Sidebar, popups, mouse](docs/screen.md) | sidebar, popups, mouse, clipboard, images |
| [Agents](docs/agents.md) | states, hooks, accounts, messages, contacts, MCP, tmux shim, events |
| [Other machines](docs/machines.md) | the operator, ssh, settings sync, hosted windows |
| [Hives and templates](docs/hives.md) | saved groups of sessions, session templates |
| [On the phone](docs/phone.md) | `imkr web`, pairing, firewall |
| [Plugins](docs/plugins.md) | plugin files, repositories, catalogs |
| [Settings](docs/settings.md) | config file, themes, own commands, hooks |
| [Compared with herdr and TUIOS](docs/comparison.md) | what imkr adds, checked October 2026 |

`imkr keys` lists every key, `imkr --help` every command.

## Status

imkr is in daily use on several machines, but young and changing fast; the version is below 1.0.
Linux only for now: it reads `/proc` to tell what a pane runs and where.

Inspired by [TUIOS](https://github.com/Gaurav-Gosain/tuios) and
[herdr](https://github.com/herdrdev/herdr), with code of its own.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
