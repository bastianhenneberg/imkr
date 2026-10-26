# Compared with herdr and TUIOS

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
- **Built by its own agents, live**: the agents developing imkr run in it, message each other
  across machines and get the new binary under their running panes with `imkr update`; a
  release reaches every machine without a restart (`imkr hosts sync`), the agents that made it
  included.
- **Plugin catalogs of your own**: catalogs are any git repositories, private ones too, with
  catalog and plugins in one; `Ctrl+B p +` lists and adds them (herdr's marketplace indexes
  public GitHub repositories only).
