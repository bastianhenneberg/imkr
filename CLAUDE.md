# imkr

Terminal-Multiplexer und Fenster-Manager in Rust. Ideen aus TUIOS (Floating, BSP-Tiling,
Workspaces) und herdr (Agent-Status, Server/Client, Socket-API), aber eigener Code.

## AI Brain

Projekt-Slug: `imkr`

## Stack

- UI: `ratatui`. `crossterm` ist zusätzlich direkt eingebunden, nur wegen des `serde`-Features
  (Events gehen per Socket vom Client zum Server). Version muss zu der von ratatui passen,
  sonst gibt es zwei crossterm-Versionen: `cargo tree -d | grep crossterm` prüft das.
- PTY: `portable-pty`
- VT-Emulation: `vt100` (später evtl. `alacritty_terminal`)

## Architektur

- `imkr server` (vom ersten Client im Hintergrund per `setsid` gestartet) besitzt PTYs,
  Layout und rendert. Der Client ist dünn: Raw-Mode, schickt crossterm-Events, schreibt die
  ANSI-Ausgabe des Servers auf stdout. Protokoll: `src/protocol.rs` (Tag + Länge + Payload).
- Der Server rendert mit `Viewport::Fixed` in einen Socket-Writer. **Nie** `terminal.clear()`
  oder `terminal.resize()` aufrufen: beide fragen das (nicht vorhandene) TTY des Servers
  nach Cursor bzw. Größe. Bei Resize wird das Terminal neu gebaut.
- Workspace-Logik (Tiling-Baum + Floats + Fokus) ist reine Geometrie in `src/workspace.rs` und dort
  getestet. `App` hält den aktiven Workspace in `ws`, die übrigen in `hidden`, und tauscht beim Wechsel.
- Begriffe wie tmux: in der Oberfläche, CLI und README heißt ein Workspace **Window** (Session →
  Window → Pane), „Hive“ ist unser eigener Begriff. Intern (Code, Save-Files, Protokoll, JSON der
  CLI-Antworten) bleibt `workspace`, damit Live-Update und alte Saves weiterlaufen. `--workspace`
  bleibt Alias für `--window`.
- Maus, Selektion, Copy-Mode: `App::mouse`, `App::copy_key`. Positionen in Selektion/Copy-Cursor sind
  *absolute* Zeilen (0 = älteste Zeile im Verlauf), damit Scrollen sie nicht verschiebt. Text holt
  `pane::text_between`. Maus-Kodierung für Programme: `src/mouse.rs`, Clipboard (OSC 52): `src/clipboard.rs`.
  tmux-Falle 2: Wird die letzte tmux-Session beendet, beendet sich der tmux-Server und vergisst
  `set-clipboard on` – vor OSC-52-Tests jedes Mal neu setzen.
  tmux-Falle: `;` in `send-keys` ist ein Befehlstrenner, auch mit `-l` – als `'\;'` schicken.
  Headless testen: rohe SGR-Sequenzen per `tmux send-keys -l $'\e[<0;x;yM'`; OSC 52 landet mit
  `set-clipboard on` im tmux-Buffer (`tmux show-buffer`).
- Agent-Status: `src/agent.rs` (Erkennung per argv aus `/proc/<pgid>/cmdline`, Aktivität, Fragen
  auf dem Screen; gemeldeter Status aus Hooks hat Vorrang). `App::update_agents` läuft bei jedem
  Tick (500 ms) und klingelt bei neuem `blocked`. Beantwortete Fragen: `Pane::note_input`.
- Verbindungen attachen erst nach `ClientMsg::Attach`; alle anderen (CLI, Agents) schicken
  `ClientMsg::Request` und bekommen `ServerMsg::Reply`. Befehle: `src/cli.rs`.
- Mehrere Clients wie in tmux: `App::clients` (Größe, Caps, Fenstertitel, Bilder je Client), alle
  sehen dasselbe bei der Größe des kleinsten (`App::fit_screen`, Rest mit Punkten, `outside`).
  `App::active` ist der Client, dessen Eingabe gerade läuft: nur er wird von `Ctrl+B d` getrennt,
  per Session-Wechsel weitergeschickt und bekommt das Clipboard; Glocke/Benachrichtigung an alle.
  `render` nimmt die Clients beim Zeichnen heraus, in `draw` daher `App::attached` statt
  `clients.len()`. `attach -d` = `Attach { alone: true }`, als Payload-Byte, das alte Server ignorieren.
  Headless-Test: zwei tmux-Sessions verschiedener Größe am selben `IMKR_SOCKET`.
- Andere Rechner: `imkr --host H` (`client::Conn::remote`) startet `ssh -C -T H` mit Login-Shell
  und `imkr [-s NAME] proxy [--start]`; der Proxy schreibt erst `PROXY_READY` (vor dem Raw-Mode,
  damit ssh-Fehler/Passwortabfragen lesbar bleiben) und reicht dann stdin/stdout ↔ Socket durch.
  Falle: `io::copy` nutzt unter Linux splice und hält Frames zurück (Detached kam nie an) →
  `pass_on` mit einfachem read/write. Test ohne sshd: Fake-`ssh` im Scratchpad vorn im PATH, der
  bis `--` überspringt und den Befehl mit `SHELL=/bin/sh sh -c` ausführt. Socket-Pfade max. 108
  Zeichen: für `XDG_RUNTIME_DIR` im Test ein kurzes Verzeichnis nehmen.
- Mehrere Rechner in einem imkr: Hosts aus `Config::hosts` + gemerkte (`client::remembered_hosts`,
  `~/.local/state/imkr/hosts`). Thread `watch_places` füllt `App::places` (lokal alle 2 s per
  `client::local_sessions`, Hosts alle 15 s per `ssh … imkr ls --json`). Sidebar-Abschnitt
  „Sessions“ und `Ctrl+B S` bauen daraus `Place`s; Wechsel = `ServerMsg::Switch("host:name")`,
  nur an Clients mit `Caps::hosts`. Der Client merkt sich den Rechner in `SHOWN_HOST` und schickt
  auf fremden Servern `Caps::origin` (eigener Hostname + Sessions) für den Rückweg; `draw` liest
  das aus `App::origin` (render nimmt die Clients heraus!). Letzte Session: `last-session` im
  State-Verzeichnis, `imkr` ohne Argumente geht dorthin; „Session list on start“ entscheidet der
  Client nach lokaler Config (`Request::OpenSessions`). Test: Fake-`ssh` mit eigenem HOME
  (`.cargo/bin/imkr` als Link), `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`, `IMKR_CONFIG` = zweite Welt.
  Falle: `pkill -f <pfad>` trifft die eigene Shell, deren Befehlszeile den Pfad enthält.
- Window in andere Session verschieben: `App::move_window` → `Pane::stop_reading` (Reader-Thread
  wartet per `poll` auf PTY oder Stopp-Pipe, damit nach der Übergabe kein Byte mehr hier gelesen
  wird), `ClientMsg::Move` (JSON `upgrade::MovedWindow`) plus PTY-FDs per SCM_RIGHTS (`fdpass`)
  an den Ziel-Socket; der Reader-Thread dort holt die FDs direkt nach dem Frame ab
  (`receive_window`). Ziel: `App::move_in` (IDs/Nummer behalten wenn frei, `Workspace::renumber`,
  `Pane::take_over` mit `ForeignChild` über pidfd, weil der Prozess Kind der Quelle bleibt).
  Quelle: `Pane::release` (Prozess nicht beenden, nur noch `wait`en), `App::moved` leitet Hook-
  Status weiter. Falle: portable-pty's Master-Writer tippt beim Drop `\n` + Ctrl+D → Shells
  sterben; daher schreibt `Pane` über ein `dup` des Masters. Test: zwei Sessions mit eigenem
  `XDG_RUNTIME_DIR`/`XDG_STATE_HOME` (sonst echte Zustände!), Tick-Skript auf Lücken prüfen.
- Agent-Tests ohne echten Agent: Skript namens `claude` im Scratchpad, das arbeitet und dann eine
  Frage stellt, per `imkr new --window 2 -- <skript>` starten.
- Live-Update: `imkr update` (`src/upgrade.rs`): Server execs das neue Binary im selben Prozess,
  PTY-Master-FDs, Listener und alle Client-Verbindungen (`clients`; `client` nur für ältere
  Server) ohne `FD_CLOEXEC` weiter, Rest (Layout, Screens als `state_formatted`, History als
  Text) in `<socket>.upgrade.json`, Env `IMKR_UPGRADE`.
  portable-pty kann keinen Master aus einem FD bauen → `AdoptedMaster`/`AdoptedChild` implementieren
  dessen Traits. Protokoll kompatibel halten: der alte Client bleibt nach dem Update verbunden.
  Test: Binary zweimal ins Scratchpad kopieren (hm1/hm2), mit hm1 starten, `hm2 update`, PIDs vergleichen.
- Persistenz: `src/persist.rs`, Datei `~/.local/state/imkr/session.json` (mit `IMKR_SOCKET`
  daneben als `<socket>.state.json`, mit `IMKR_STATE` frei wählbar). Gespeichert höchstens 1×/s
  bei Änderung, gelöscht am Ende von `App::run` (nur bewusstes Beenden kommt dort an). Test: Server
  mit `kill -9` töten und neu starten.
- Farben nie fest verdrahten: `theme::current()` (accent, on_accent, subtle, danger, success,
  warning). Themes aus Omarchys `colors.toml` (`~/.config/omarchy/themes`, `~/.local/share/omarchy/themes`,
  aktuell: `~/.local/state/omarchy/current/`), `src/theme.rs`. Test des Live-Folgens mit falschem
  `HOME` samt nachgebautem `.local/state/omarchy/current`.
- Bilder (kitty graphics): `src/graphics.rs` (Scanner, Command, PaneGraphics), im Pane-Reader
  vor vt100 abgefangen, `App::image_output` platziert nach jedem Frame. Test ohne Grafik-Terminal:
  Client unter `script -qfc 'env IMKR_GRAPHICS=1 imkr' raw.txt` in tmux starten und die
  APC-Sequenzen in `raw.txt` prüfen; Testprogramm mit echter PNG über Python (zlib/struct).
- Tastenbelegung nach dem Prefix steht **nur** in `src/bindings.rs` (`BINDINGS`). Which-Key-Menü,
  Hilfe-Overlay und `imkr keys` lesen daraus, neue Befehle dort eintragen, nicht in `app.rs`.
- Einstellungen: `src/config.rs` (serde/TOML, `SETTINGS`-Tabelle fürs Settings-Menü). Neue
  Einstellung = Feld in `Config` + Eintrag in `SETTINGS`, das Menü liest nur die Tabelle.
- Manuell testen ohne die eigene Session zu stören: `IMKR_SOCKET=/tmp/x.sock` und `IMKR_CONFIG=/tmp/x.toml`
  setzen (sonst wird die echte Config überschrieben), headless in tmux starten (`tmux new-session -d ...`, `send-keys`, `capture-pane -p`).

## Befehle

```bash
cargo run            # starten, Ctrl+B q Quit-Menü, Ctrl+B d detacht, `cargo run -- kill-server` beendet
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo test
```

## Konventionen

- Lizenz: MIT OR Apache-2.0
- Code und Kommentare auf Englisch
