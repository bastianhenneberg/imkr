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
  auf dem Screen; gemeldeter Status aus Hooks hat Vorrang). Config `extra_agents`/`question_phrases`
  → `agent::configure` (globales `EXTRA`, in `apply_theme`); `imkr explain` → `Pane::explain`. `App::update_agents` läuft bei jedem
  Tick (500 ms) und klingelt bei neuem `blocked`. Beantwortete Fragen: `Pane::note_input`.
- Rechner-Identität: `protocol::machine_id()` (/etc/machine-id; `IMKR_HOSTNAME` in Tests) für Hives,
  `machine_name()` (Config `name`, sonst Hostname) für Anzeige und Origin – zwei Omarchy-Rechner
  heißen beide `omarchy`! Server nimmt `newest_origin()` (Client mit `via`), nicht stur den aktiven.
  Client ersetzt sich selbst (`reexec_if_replaced` im origin-Thread, Ziel in `IMKR_REEXEC`).
- Agent-Metadaten: `imkr status` reicht `transcript_path` aus dem Hook-JSON als
  `Request::Status::transcript` weiter (auch im Handover), `Pane::refresh_meta` (je Tick, nur bei
  geänderter mtime) → `agent::transcript_meta` (letzte `usage` in den letzten 512 KB) →
  `Meta::label` („opus 5.5 · 300k“), in `list()` als `meta`, `AgentInfo::meta`. Keine Kosten:
  bräuchte eine Preistabelle.
- Claude-Hooks: `cli::HOOKS` (Ereignis, Matcher, Zustand) für `imkr hooks` und `--install`
  (Skript + Merge in settings.json, alte imkr-Hooks raus, Backup). `Notification` mit
  `permission_prompt|elicitation_dialog` → blocked, für Agents im Bypass-Modus. Test mit Schein-HOME.
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
- Agent-Liste `Ctrl+B A` (`agent_choices`, `AgentAt`) und Marken wie Harpoon (`Mark`, `marks.json`
  neben `saves/` je Rechner, `Ctrl+B m`/`M`, `m` in der Agent-Liste markiert auch Remote-Agents,
  `Alt+1-9` in `handle_key` vor der Pane-Weitergabe, abschaltbar `marks_on_alt`). Marke findet
  ihr Pane nach Neustart über Agent-Session-ID oder Name im Window. Test: tmux `send-keys M-1`.
- Plugins: `src/plugin.rs` (TOML je Datei in `plugins/` neben der Config, `deny_unknown_fields`,
  kaputte → `Broken`), `App::plugins` (Arc, geteilt mit `watch_statuses`, das die `status`-Befehle
  mit 10 s Zeitlimit laufen lässt), `run_plugin` = Popup mit Namen des Plugins. Menü `Ctrl+B p`
  wie `Ctrl+B c` (Tasten aus den Dateien). Sidebar „Plugins“: `popup_entries`/`popup_rows`
  (alle Plugins, dann Hand-Popups, kaputte zuletzt). Test: Plugins neben `IMKR_CONFIG`.
  Plugins folgen dem Operator: `Origin::plugins`, `merge_plugins` (eigene gewinnen, fremde mit
  `from`), `Plugin::installed` vor dem Start. Fake-Welt des Servers: Config in `rconf/`, sonst
  teilen sich beide Welten den Plugin-Ordner.
- Hives über Rechner: `Hive::machine` + `machines` (`HiveMachine`: ssh-Name, Hostname, Sessions);
  `client::snapshot_everywhere` (per ssh `imkr hive snapshot`), `push_hive` (`hive import`, nur wenn
  neuer) und `pull_hives` (`hive export`, minütlich im `watch_places`), `last` bleibt lokal.
  `restore_hive(name, from, take)`: eigener Rechner lokal, andere per `hive start` + `hosts add`
  (Einklinken), offline bleibt offline. Test mit mehreren Welten: `IMKR_HOSTNAME` je Welt setzen,
  sonst halten sich alle für denselben Rechner.
- Popups (`App::popups`, `popup`, `popup_shown`): Panes in `panes`, aber in keinem Workspace;
  `input_pane()` statt `ws.focus` für Tasten/Paste, `popup_rect()` für Größe/Maus, `draw_popup`
  nach den Leisten. Nicht in `saved()` (Persistenz), aber im Handover (`popups`, `popup`), sonst
  enden sie beim Update wie ein Pane ohne Platz. `close()` entfernt sie. `popup_full` (Ctrl+B z
  über einem Popup) = Panes + Sidebar, `break_popup` (Ctrl+B !) macht ein Window daraus; auf einem Vollbild-Popup verkleinert `!`
  erst (wie z). Basis
  für Plugins.
- Session-Vorlagen: `src/template.rs` (TOML in `templates/` neben der Config) → `to_saved` macht
  daraus ein `persist::Saved` (mit `SavedPane::command`, `Saved::template`), gestartet wie ein Hive:
  lokal `client::start_from`, remote `start_from_on` (`hive start` per ssh), danach `Request::Go`.
  `~` bleibt bis `App::restore` (`template::home_dir`, Home des Zielrechners). Folgen dem Operator
  über `Origin::templates`. Splits ↔ Layout: `Layout::splits`/`split_at`. Laufende Sessions
  speichern nie `command` (nach einem Crash startet nichts von selbst).
- Ruhende Sessions: `persist::dormant_sessions` (State-Dateien ohne laufenden Server = stopped,
  Hive `last` = closed), `client::local_sessions` hängt sie mit `SessionInfo::dormant` an (also
  auch in `ls --json` und für andere Rechner). `go_to` holt closed per `persist::revive` (remote
  `imkr revive`) zurück, stopped stellt der startende Server selbst her. `imkr forget`, `x` in
  `Ctrl+B S`. Server erben keine Claude-Sitzungsvariablen (`client::is_agent_session_var`), sonst
  speichert Claude in den Panes keine Transcripts.
- Links: `src/links.rs` (`LinkScanner` trennt OSC 8 aus der Ausgabe, im Pane-Reader nach dem
  Grafik-Scanner; Start/Ende mit Cursorposition als `Link` in `Pane::links`, max. 500; `url_in`
  für Klartext-URLs). Ctrl+Klick in `App::mouse` vor `forward_mouse` → `Pane::link_at` →
  `ServerMsg::Open` (Tag 6, nur an Clients mit `Caps::open`) → Client `open_url` (xdg-open).
  Test: Fake-`xdg-open` im Scratchpad, Klick als `tmux send-keys -l $'\e[<16;x;yM'` (16 = Ctrl).
- OSC 52 aus Panes: `clipboard::Osc52Scanner` im Pane-Reader (vor dem Grafik-Scanner, über
  Lesegrenzen), `AppEvent::Clipboard` → `send_active`. Clipboard-Abfragen (`?`) werden verworfen.
  Test verschachtelt: im Pane `ssh -t -- server <scratchpad>/bin/imkr` aufrufen, nicht `imkr`
  (die zsh im Pane stellt `~/.cargo/bin` vorn in den PATH → installiertes Binary).
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
  Agents anderer Sessions: `SessionInfo::agent_panes` (aus `imkr ls --json`), Sidebar
  `OtherAgentRow`, Klick → Switch `host:name#pane` (Client schickt danach `Request::Reveal`).
  Rückrichtung: Client auf fremdem Rechner schickt alle 3 s `Caps` mit `Origin::infos` (seine
  `local_sessions`), der Server zeigt sie unter ⌂ (`origin_infos`). `#pane` nur an Clients mit
  `Caps::panes`. `machine_label` nimmt die `places`-Sperre: nie aufrufen, während sie gehalten wird.
  `Origin::hosts` (vom lokalen Server per `Request::Places`) + `Origin::via` (der ssh-Name, unter dem
  der Client den Server erreicht) → `App::known_hosts()` führt sie mit den eigenen zusammen, ohne
  sich selbst; `own_name()` nennt den Server so, wie der User ihn kennt. Test mit drei Welten:
  Fake-`ssh` verteilt nach Host (`server`, `third`) auf eigene HOME/Runtime/State/Config.
  Remote-Wechsel nach blocked/done erkennt `watch_places` per `remote_change` und legt sie in
  `Places::alerts`, `pass_on_alerts` meldet wie lokal (lokale Sessions melden sich selbst).
  Falle: `pkill -f <pfad>` trifft die eigene Shell, deren Befehlszeile den Pfad enthält.
- Verteilen: `imkr update`/`upgrade` stellen jede laufende Session des Rechners um (`-s` nur eine,
  eigenes `IMKR_SOCKET` außerhalb des Runtime-Verzeichnisses nur diesen). `imkr hosts sync`
  (`cli::hosts_sync`, parallel je Host): `client::installed_on` → `install_on` (install.sh per
  ssh nach `~/.local/bin`) oder `remote_output(["upgrade"])`. `Conn::remote` meldet Exit 127 als
  `NotInstalled`, `offer_install` fragt im Terminal. Test: Fake-ssh-Host `bare` mit leerem HOME
  und `PATH=/usr/bin:/bin` (lädt das echte Release).
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
- Event-Schleife: Rendern bei reiner Programmausgabe höchstens alle `FRAME` (8 ms), Eingaben
  (`is_input`) sofort; Agents/Hooks/Git/Dateien/Speichern höchstens alle `UPKEEP` (100 ms) oder
  nach Eingabe/Tick. Vorher ~1000 Frames/s bei Dauerausgabe. Rest liegt im pty-reader (vt100,
  teuer bei vollem Verlauf); Benchmark: `cargo test --release reading_costs -- --ignored --nocapture`.
  Messen: CPU je Thread aus `/proc/<server>/task/*/stat`.
- Persistenz: `src/persist.rs`, Datei `~/.local/state/imkr/session.json` (mit `IMKR_SOCKET`
  daneben als `<socket>.state.json`, mit `IMKR_STATE` frei wählbar). Gespeichert höchstens 1×/s
  bei Änderung, gelöscht am Ende von `App::run` (nur bewusstes Beenden kommt dort an). Test: Server
  mit `kill -9` töten und neu starten. Verlauf: `App::save_scrollback` (alle 20 s je Pane mit neuer
  Ausgabe, 0600) nach `persist::scrollback_dir` (`<state>.scrollback/<pane>.txt`), `restore` lädt
  ihn per `Pane::spawn_after` vor (`restored_history`) und räumt erst dann auf
  (`prune_scrollback`) – nie im Betrieb, beim Herunterfahren sterben die Shells vor dem Server.
- Farben nie fest verdrahten: `theme::current()` (accent, on_accent, subtle, danger, success,
  warning). Themes aus Omarchys `colors.toml` (`~/.config/omarchy/themes`, `~/.local/share/omarchy/themes`,
  aktuell: `~/.local/state/omarchy/current/`), `src/theme.rs`. Test des Live-Folgens mit falschem
  `HOME` samt nachgebautem `.local/state/omarchy/current`.
- Bilder (kitty graphics): `src/graphics.rs` (Scanner, Command, PaneGraphics), im Pane-Reader
  vor vt100 abgefangen, `App::image_output` platziert nach jedem Frame. Test ohne Grafik-Terminal:
  Client unter `script -qfc 'env IMKR_GRAPHICS=1 imkr' raw.txt` in tmux starten und die
  APC-Sequenzen in `raw.txt` prüfen; Testprogramm mit echter PNG über Python (zlib/struct).
- Suche `Ctrl+B /`: der Pane-Picker mit `Picker::global`; `App::found` sammelt `Found` (Art, Text,
  Hinweis, Ziel `Go`) aus Panes, Windows, `session_items`, `agent_choices`, Plugins, Marken, Hives
  (beim Öffnen gelesen, nicht je Frame) und `BINDINGS` (nur Befehle mit genau einer Taste).
- Agents untereinander: `imkr msg` (`cli::msg`, Adresse `machine:session/pane`, `parse_address`,
  fremde Rechner per `client::remote_output` mit Text auf stdin) → `Request::Message` → `App::mailbox`,
  `deliver_mail` (je Tick) tippt nur bei idle/done/ohne Agent, nie bei blocked, 3 s Abstand je Pane.
  `imkr agents [--all]`, Skill-Text in `skills/imkr/SKILL.md` (`imkr skill [--install]`).
  Test-Falle: `imkr status` liest stdin (Hook-JSON) – in Heredoc-Tests `</dev/null` anhängen.
- Inbox `Ctrl+B i` (`inbox_items`, `inbox_key`, `draw_inbox`): blocked/done Agents aus
  `agent_entries` mit `agent::tail` (Server liefert es in `list()` als `tail`, über
  `AgentInfo::tail` auch für andere Sessions/Rechner). Antwort per `App::answer`: lokal
  `type_into`, andere Session `Request::Send` an ihren Socket, fremder Rechner `imkr send --stdin`
  per ssh. Auswahl hängt an `InboxId` (Rechner, Session, Pane), nicht am Index: die Liste
  sortiert sich um, während sie offen ist. `asks_question`/`tail` lesen bis zur letzten
  beschriebenen Zeile, nicht bis zum Bildschirmende. Test-Agent: bash mit `read -n 1`, nicht
  `head` (sonst ist das Kind im Vordergrund und kein Agent erkannt).
- Großbuchstaben zeigen alles, über alle Sessions und Rechner: `S` Sessions, `A` Agents, `W` Windows,
  `P` Panes, `M` Marken, `H` Hives. W/P: `window_items`/`pane_items` → `Spot` (`go_spot`), andere
  Sessions über `other_panes` aus `SessionInfo::all_panes` (ältere Versionen: nur `agent_panes`).
  Panes stehen überall hinter ihrem Window: `1 api › claude` (`window_name_of`).
  Listen gruppieren nach Rechner: `MenuItem::group` (Überschrift über dem ersten Eintrag jeder
  Gruppe, Indizes bleiben gleich), Einträge eines Rechners müssen hintereinander stehen.
- Recap: `App::activity` (Zustandswechsel aus `update_agents`, max. 500), `away_since` (gesetzt in
  `take_client`, wenn der letzte Client geht), `attach` öffnet `Mode::Recap` nach ≥ 60 s mit
  Aktivität, `Ctrl+B s l` (`open_recap`) später. Test: abhängen, Fake-Agent starten, 66 s warten.
- Review `Ctrl+B v`: `src/review.rs` (`diff` liest nur: `git diff HEAD` plus `--no-index` je neuer
  Datei, nie `add -N`; `parse` nummeriert je Datei; `Note` hängt an Datei/Nummer/Code, nicht am
  Index; `prompt`). `App::review` (`Review`), `open_review`, `review_key`, `draw_review`, Notiz per
  `PromptFor::ReviewNote` (Esc/Enter zurück in `Mode::Review`), Senden über `mailbox`.
- Checkpoints: `src/checkpoint.rs` (`take`: Kopie des Index als `GIT_INDEX_FILE`, `add --all`,
  `write-tree`, `commit-tree` auf `refs/imkr/checkpoints/<session>-<pane>`, nur bei neuem Tree;
  `list`). Ausgelöst in `update_agents` beim Übergang zu Done, Einstellung `checkpoints`, im
  Hintergrund-Thread (`App::checkpoint`).
- tmux-Shim: `src/tmux.rs` (`run` übersetzt tmux-Befehle in Requests, `Options::parse`,
  `target_pane`, `expand` für `#{…}`, `key_bytes`), `main` erkennt argv0 `tmux`. Einstellung
  `tmux_shim` → `pane::set_tmux_shim(tmux::shim_dir())` in `apply_theme` (Link in
  `<state>/tmux-shim/tmux`), `Pane::spawn` setzt dann TMUX, TMUX_PANE, PATH. `Request::Kill`
  schließt ein Pane (nicht das letzte). Test: Skript mit tmux-Befehlen in einem Pane.
- Worktrees: `src/worktree.rs` (`create`: neben dem Haupt-Repo `<repo>--<name>`, über
  `--git-common-dir` auch aus einem Worktree heraus), `App::worktree_window` (`Request::Worktree`,
  `Ctrl+B w t` per `PromptFor::Worktree`). Entfernen bleibt bei git.
- Ereignisse: `imkr subscribe` (`cli::snapshot`/`changes`, Polling per `List`, läuft mit jedem
  Server) und `[[hooks]]` in der Config (`config::Hook`, `HOOK_EVENTS`, `hook_problems`):
  `App::check_hooks` vergleicht je Tick (`HookPanes`, erster Blick feuert nichts),
  `run_hooks` startet `sh -c` im Hintergrund, attach/detach direkt in `attach`/`detach`.
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
