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
- VT-Emulation: `alacritty_terminal` hinter `src/term.rs` (`term::Parser` mit der Methoden-Oberfläche,
  die vorher vt100 hatte: `process`, `screen()`, `cell`, `contents_between`, `scrollback`, `abs_row`,
  `state_formatted` fürs Live-Update). Umbruch beim Verkleinern/Vergrößern macht `set_size`;
  Antworten an das Programm (Cursor-Position, Farben, Größe) sammelt `answers()`, der Pane-Reader
  schreibt sie über ein `dup` des Masters zurück. Gemessen 3,7× schneller als vt100 bei vollem
  Verlauf, Pane-Ausgabe schneller als tmux.

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
  Eigenes Window je Client (`own_window_per_client`, Standard an): `Client::workspace`; vor
  Eingaben eines Clients und beim Zeichnen je Client holt `App::view` dessen Window nach vorn
  (`show`), `render` zeichnet den aktiven Client zuletzt und stellt sein Window wieder her, sodass
  zwischen den Frames immer das Window des aktiven Clients `ws` ist. `follow_active` am Ende von
  `handle` übernimmt Wechsel (auch per CLI) für den aktiven Client. Modus/Menüs bleiben geteilt.
  Im Handover als `HandoverClient::workspace`.
- Andere Rechner: `imkr --host H` (`client::Conn::remote`) startet `ssh -C -T H` mit Login-Shell
  und `imkr [-s NAME] proxy [--start]`; der Proxy schreibt erst `PROXY_READY` (vor dem Raw-Mode,
  damit ssh-Fehler/Passwortabfragen lesbar bleiben) und reicht dann stdin/stdout ↔ Socket durch.
  Falle: `io::copy` nutzt unter Linux splice und hält Frames zurück (Detached kam nie an) →
  `pass_on` mit einfachem read/write. Test ohne sshd: Fake-`ssh` im Scratchpad vorn im PATH, der
  bis `--` überspringt und den Befehl mit `SHELL=/bin/sh sh -c` ausführt. Socket-Pfade max. 108
  Zeichen: für `XDG_RUNTIME_DIR` im Test ein kurzes Verzeichnis nehmen.
- Hosted Windows (Programm auf anderem Rechner): `cli::new_hosted` (`imkr --host H new`, `here` =
  Socket vor `-s`) legt per ssh `imkr new --start --window 0` (0 = freies Window) dort an und lokal
  ein Window mit `imkr hosted H PANE [SESSION]` (`cli::hosted`, Schleife: ssh -t + `attach-pane`,
  neu verbinden bei 255/`LOST`=75/Signal, sonst Ende). `attach-pane` schickt `Request::Tap` →
  `App::tap` (Verbindung in `App::taps`), danach `ServerMsg::Output` roh, Eingabe als
  `ClientMsg::Input` (Tag 8), Größe als Resize-Event. `Pane::tap`: Verlauf + `state_formatted`
  unter dem Parser-Lock, dann Kanal (`TAP_BACKLOG`, wer zurückfällt fliegt raus und verbindet neu);
  leerer Chunk = Pane zu Ende → `Exited`. Angezapfte Panes beantworten Terminal-Abfragen nicht
  (das zeigende Terminal tut es) und `sync_sizes` lässt ihre Größe in Ruhe. `saved()` speichert
  für `imkr hosted` die Zeile als `SavedPane::command` (`hosted_command`), damit es nach
  Neustart wieder anhängt. UI: `Ctrl+B w h` → `Mode::NewSessionWhere` mit `App::hosting`.
  Agent drüben: `App::tell_taps` (je Tick, nur bei Änderung, `told`) schickt OSC 7377
  (`links::remote_state`, `agent=…;state=…`) über den Tap; lokal macht `LinkScanner` daraus
  `Piece::Remote` → `Pane::remote` (auch im Handover), `agent_state`/`program` nehmen es zuerst.
  `other_agents` lässt Agents weg, die ein lokales Hosted Window zeigt (`hosted_target`).
  Test: Fake-ssh (`env -u IMKR_SOCKET … PATH=<fake-home>/.cargo/bin:…`), Datei `down` → 255.
  Lokale Testwelt mit eigenem HOME, sonst landen Befehle in der echten zsh-History.
  Direkte `ssh server 'imkr …'` im Test nehmen sonst das installierte imkr!
- Web/Handy: `src/web.rs` (`imkr web`, eigener Prozess, HTTP von Hand auf `TcpListener`, je
  Verbindung ein Thread, `Connection: close`; WebSocket per `tungstenite` (`derive_accept_key`,
  `from_raw_socket`), Seiten in `web/` per `include_str!`, xterm.js + addon-fit eingebettet in
  `web/vendor/`). Daten: `client::local_sessions` (`/api/state`), Tippen `Request::Send`
  (`/api/send`), Terminal `/ws` = `Request::Tap` mit `watch` (Pane behält Größe und beantwortet
  selbst, `Pane::tapped` zählt nur nicht-watch; `Pane::resize` wirft Zuschauer raus → Seite
  verbindet neu) oder `size` (Fit: Größe vor dem Bildschirmabzug). Kopplung: Code in
  `<state>/web-pairing.json` (10 min, 5 Versuche), Geräte mit Token in `web-devices.json` (0600),
  Cookie `imkr_device`, POST/WS nur mit passendem `Origin` (Falle: `Referrer-Policy: no-referrer`
  lässt Chrome `Origin: null` schicken). Einstellung `web`: `watch_places` ruft minütlich
  `web::keep_running` (startet `imkr web --background`, wenn auf der Adresse nichts antwortet);
  der Prozess exect sich selbst, sobald `/proc/self/exe` gelöscht ist (Update). Adresse:
  `--listen`/`web_listen`, sonst 100.64.0.0/10 (`vpn_address`, getifaddrs), sonst 127.0.0.1:4657.
  Andere Rechner: `other_machines` (per `Request::Places` von den lokalen Servern, ohne laufenden
  Server eigenes Polling `polled_machines`), `/api/send` mit `host` → `imkr send` per ssh,
  `/ws?host=` → `client::Conn::remote` (`imkr proxy`); nur Hosts aus dieser Liste (`known_machine`).
  Ganze Session: `/ws?attach=1` → `web::attach` startet `imkr [--host H] -s S` als Client in
  einem eigenen pty (schreibt über ein `dup` des Masters, nicht portable-ptys Writer: der tippt
  beim Drop Ctrl+D, das der Client ans Pane weitergäbe), Größe per Text `{"cols","rows"}`.
  Seite: `target.whole`, Tastaturmodus `body.kbd` (`setKbd`: Vollbild, Querformat, Keyboard Lock,
  Wake Lock, jeweils nur wo der Browser es erlaubt).
  Verbundene Geräte: `note_seen` (bei `/api/state`, WS-Start/-Ende, alle `STILL_THERE` im WS)
  → `<state>/web-seen.json`, `web::connected()` (jünger als `SEEN_FOR`) → `Places::web` in
  `watch_places` → Sidebar „Phones“ (`Data::phones`, vor `machine_label` aus dem Lock holen).
  Log: `Response::note` (Ablehnung/Kopplung) → `log` mit `stamp` und Peer; leere Verbindungen
  (`read_request` → `None`, die Probe von `keep_running`) still. `firewall_hint`/`ufw_hint` liest
  `/etc/ufw/ufw.conf` + `user.rules` (lesbar ohne root), Interface aus `interfaces()`.
  Test: headless `chromium --remote-debugging-port` + Node-Skript per CDP (Node hat WebSocket).
- Agent-Liste `Ctrl+B A` (`agent_choices`, `AgentAt`) und Marken wie Harpoon (`Mark`, `marks.json`
  neben `saves/` je Rechner, `Ctrl+B m`/`M`, `m` in der Agent-Liste markiert auch Remote-Agents,
  `Alt+1-9` in `handle_key` vor der Pane-Weitergabe, abschaltbar `marks_on_alt`). Marke findet
  ihr Pane nach Neustart über Agent-Session-ID oder Name im Window. Test: tmux `send-keys M-1`.
- Plugins: `src/plugin.rs` (TOML je Datei in `plugins/` neben der Config, `deny_unknown_fields`,
  kaputte → `Broken`), `App::plugins` (Arc, geteilt mit `watch_statuses`, das die `status`-Befehle
  mit 10 s Zeitlimit laufen lässt), `run_plugin` = Popup mit Namen des Plugins. Menü `Ctrl+B p`
  wie `Ctrl+B c` (Tasten aus den Dateien). Sidebar „Plugins“: `popup_entries`/`popup_rows`
  (alle Plugins, dann Hand-Popups, kaputte zuletzt). Test: Plugins neben `IMKR_CONFIG`.
  Plugin-`[[actions]]` (`plugin::Action`, Taste in `Ctrl+B p`, `App::run_plugin_action`: Popup oder
  Hintergrund mit Meldung per `Request::Flash`) und `[[hooks]]` (in `run_hooks`, nur ohne `from`).
  `imkr plugin add/update/rm` (`cli::plugin`): Quelle (`plugin::parse_source`: Pfad, git-URL,
  `owner/repo`), Klon in `plugins/.sources/NAME`, Liste `.sources/sources.json`, Dateien aus
  `plugins/` oder oberster Ebene (`source_files`) als Symlinks nach `plugins/` (`link_source`,
  eigene gleichnamige bleiben), `{dir}` = Ordner der echten Datei (`plugin::parse`). Vorschau per
  `describe` und Frage (ohne Terminal nur mit `--yes`); Update: `fetch` + Vorschau aus
  `FETCH_HEAD` + `reset --hard` im Klon. Danach `Request::ReloadPlugins` an alle Sessions.
  Unterordner als Quelle: `Source::sub` (`owner/repo/dir`, `URL#dir`), `root` = Klon + sub,
  `clone_dir` = ganzer Klon; Update läuft im Unterordner (`ls-tree` relativ, `show FETCH_HEAD:./`).
  Marktplatz: Config `plugin_catalogs`, `plugin::fetch_catalog` (Klon in `plugins/.catalogs/`,
  bei jedem Auflisten fetch+reset), `catalog_entries` (`catalog.toml` oder Ordner mit Plugins),
  `entry_source`; CLI `search`, `add NAME` (`resolve_plugin`), `market` (zeilenweise, im Popup
  per `Ctrl+B p +` = `Command::PluginMarket`).
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
  speichern nie `command` (nach einem Crash startet nichts von selbst). Nach der Vorlage (auch
  „leer“ und ohne Vorlagen) fragt `ask_session_dir` den Ordner (`PromptFor::SessionDir`,
  Vorschlag root/fokussiertes Pane/`~`, Liste darunter `dir_choices` (`DIR_ROWS`, ↑↓ `dir_pick`,
  Tab `dir_key` → `template::complete_with`), andere Rechner per ssh `imkr dirs` (Pfad auf stdin)
  in `dir_lists` gecacht, `MAX_PATH`);
  der Ordner wird `Template::root`, „leer“ = Vorlage mit einem Window.
- Ruhende Sessions: `persist::dormant_sessions` (State-Dateien ohne laufenden Server = stopped,
  Hive `last` = closed), `client::local_sessions` hängt sie mit `SessionInfo::dormant` an (also
  auch in `ls --json` und für andere Rechner). `go_to` holt closed per `persist::revive` (remote
  `imkr revive`) zurück, stopped stellt der startende Server selbst her. `imkr forget`, `x` in
  `Ctrl+B S`. Server erben keine Claude-Sitzungsvariablen (`client::is_agent_session_var`), sonst
  speichert Claude in den Panes keine Transcripts. Nach Neustart: `watch_places` schreibt die
  laufenden Sessions nach `<state>/running` (`persist::note_running`, nur ohne `IMKR_SOCKET`),
  `client::run` startet per `revive_stopped` die dort genannten, die jetzt stopped sind
  (Einstellung `revive_sessions`). Bewusst beendete fallen raus: die übrigen Server schreiben
  die Datei neu, und ihr State ist gelöscht.
- Shell-Integration (OSC 133): `LinkScanner` liefert auch `Piece::Mark` (A/B/C/D;exit), `Pane::shell`
  (`Shell::mark`: C = Ausgabe beginnt, D = fertig), `Pane::last_command` (Ende exklusiv: `text`
  nimmt die Endzelle mit), `Request::LastCommand`; CLI `run`, `wait` ohne `--until`,
  `shell-integration zsh|bash`. Die zsh des Users sendet OSC 133 schon selbst.
- Links: `src/links.rs` (`LinkScanner` trennt OSC 8 aus der Ausgabe, im Pane-Reader nach dem
  Grafik-Scanner; Start/Ende mit Cursorposition als `Link` in `Pane::links`, max. 500; `url_in`
  für Klartext-URLs). Ctrl+Klick in `App::mouse` vor `forward_mouse` → `Pane::link_at` →
  `ServerMsg::Open` (Tag 6, nur an Clients mit `Caps::open`) → Client `open_url` (xdg-open).
  Test: Fake-`xdg-open` im Scratchpad, Klick als `tmux send-keys -l $'\e[<16;x;yM'` (16 = Ctrl).
- Kontextmenü: Rechtsklick → `App::open_context` (Tab → Window-Befehle, Pane → Pane-Befehle, außer
  das Programm nimmt die Maus), `Context` mit `Command`s, `Mode::Context`, eigene Zeichnung
  (`draw_context`), Klick per Zeile (`context_mouse`). Test: `tmux send-keys -l $'\e[<2;x;yM'`.
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
  Session endet (`ServerMsg::Exited`): Client geht per `other_running_session` in eine andere
  laufende Session hier (Session-Liste offen, Flash), sonst Ende und `forget_place` (sonst
  startet `imkr` die beendete leer neu). Testwelten immer mit `unset IMKR_SOCKET`!
- Tailscale: `client::tailnet_peers` (`tailscale status --json`, MagicDNS-Name, nur linux/macOS),
  `Places::tailnet` (minütlich in `watch_places`), Vorschläge in `machine_choices` (Gruppe
  „Tailscale“), `imkr hosts tailnet`. Test: Fake-`tailscale` im Scratchpad-bin.
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
  nach Eingabe/Tick. Vorher ~1000 Frames/s bei Dauerausgabe. Rest liegt im pty-reader (seit
  alacritty_terminal deutlich weniger); Benchmark: `cargo test --release reading_costs -- --ignored --nocapture`.
  Messen: CPU je Thread aus `/proc/<server>/task/*/stat`.
- Persistenz: `src/persist.rs`, Datei `~/.local/state/imkr/session.json` (mit `IMKR_SOCKET`
  daneben als `<socket>.state.json`, mit `IMKR_STATE` frei wählbar). Gespeichert höchstens 1×/s
  bei Änderung, gelöscht am Ende von `App::run` (nur bewusstes Beenden kommt dort an). Test: Server
  mit `kill -9` töten und neu starten. Verlauf: `App::save_scrollback` (alle 20 s je Pane mit neuer
  Ausgabe, 0600) nach `persist::scrollback_dir` (`<state>.scrollback/<pane>.txt`), `restore` lädt
  ihn per `Pane::spawn_after` vor (`restored_history`) und räumt erst dann auf
  (`prune_scrollback`) – nie im Betrieb, beim Herunterfahren sterben die Shells vor dem Server.
- Geteilte Ränder: `workspace::set_shared_borders` (Kacheln überlappen um 1 Zelle, nur ohne Gaps),
  `draw` zeichnet die fokussierte Kachel zuletzt, `render::join_borders` macht aus übermalten Ecken
  Kreuzungen und lässt Innenflächen (Programm-Kästen) und Floats aus.
- Farben nie fest verdrahten: `theme::current()` (accent, on_accent, subtle, danger, success,
  warning). Themes aus Omarchys `colors.toml` (`~/.config/omarchy/themes`, `~/.local/share/omarchy/themes`,
  aktuell: `~/.local/state/omarchy/current/`), `src/theme.rs`. Test des Live-Folgens mit falschem
  `HOME` samt nachgebautem `.local/state/omarchy/current`.
- Bilder (kitty graphics): `src/graphics.rs` (Scanner, Command, PaneGraphics), im Pane-Reader
  vor dem Terminal abgefangen, `App::image_output` platziert nach jedem Frame. Test ohne Grafik-Terminal:
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
- Ruhezeit je Session: `SessionInfo::quiet_for` (aus `AgentInfo::since`, 0 bei working/blocked,
  `None` ohne Agents), eigene Session `App::quiet_for`; `Ctrl+B S` zeigt „✓1 2h ago“, `o` blendet
  alte aus (`hide_old`, `OLD_AFTER` 12 h, ruhend oder ohne Agents). Damit die Zeit Updates und
  Neustarts übersteht: `SavedPane::idle_since` → `App::carried` (gilt bis zum ersten Idle, höchstens
  `CARRIED_FOR`), übernommene Panes setzen `last_output` 60 s zurück (sonst kurz „working“), und
  `Handover::unseen` hält ✦ done über ein Update.
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
- Layouts: `src/shapes.rs` (`Shape` = Splits ohne Panes, `of`, `preset` wie tmux, `build` mit den
  Panes in Reihenfolge, gespeichert in `layouts.json` neben der Config), `App::layout`
  (`Request::Layout`, `Ctrl+B w l` → `Mode::Layouts`). main-* nicht angleichen (60 % Hauptpane).
- Worktrees: `src/worktree.rs` (`create`: neben dem Haupt-Repo `<repo>--<name>`, über
  `--git-common-dir` auch aus einem Worktree heraus), `App::worktree_window` (`Request::Worktree`,
  `Ctrl+B w t` per `PromptFor::Worktree`). Entfernen bleibt bei git.
  Fleets: `cli::fleet` (N× `Request::Worktree` `NAME-i` mit `[agent, prompt]`, Ausgangs-Commit in
  `refs/imkr/fleets/NAME`, `--diff` vergleicht je Worktree dagegen), `worktree::main_root`.
- MCP: `src/mcp.rs` (`imkr mcp`, JSON-RPC je Zeile auf stdin/stdout, `handle`; jedes Werkzeug ruft
  das eigene Binary mit dem CLI-Befehl auf, `command_for`, Text per stdin). Test: Zeilen per Pipe
  mit `IMKR_SOCKET` der Testsession.
- Ereignisse: `imkr subscribe` (`cli::snapshot`/`changes`, Polling per `List`, läuft mit jedem
  Server) und `[[hooks]]` in der Config (`config::Hook`, `HOOK_EVENTS`, `hook_problems`):
  `App::check_hooks` vergleicht je Tick (`HookPanes`, erster Blick feuert nichts),
  `run_hooks` startet `sh -c` im Hintergrund, attach/detach direkt in `attach`/`detach`.
- Which-Key mit Pfeilen: `Mode::Pick(menu, i)`, Einträge `bindings::pick_keys` (eigene Befehle/
  Plugins zuerst, dann `pickable` Bindings je Gruppe: alle Tasten = derselbe Befehl), `App::pick_key`
  führt die Taste per `command` aus; Hervorhebung `menu::picked` je Span (Spalten werden aus Spans
  neu gebaut, Line-Style ginge verloren). Start mit Enter, ↑↓ nur wo im Menü unbelegt.
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
