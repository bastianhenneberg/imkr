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
- Begriffe wie tmux: in der Oberfläche, CLI, README und docs/ heißt ein Workspace **Window** (Session →
  Window → Pane), „Hive“ ist unser eigener Begriff. Intern (Code, Save-Files, Protokoll, JSON der
  CLI-Antworten) bleibt `workspace`, damit Live-Update und alte Saves weiterlaufen. `--workspace`
  bleibt Alias für `--window`.
- Sprünge wie flash.nvim: `Mode::Jump` mit `App::jump` (`Jump`: Eingabe + `JumpTarget`s),
  `jump_targets` sucht in den sichtbaren Zeilen aller Panes (`Pane::rows_from`, fokussiertes
  zuerst), Labels aus `JUMP_LABELS` ohne Zeichen, die auf einen Treffer folgen; `jump_key`
  setzt Fokus und `CopyCursor` (eine Markierung im selben Pane bleibt, der Sprung setzt ihr
  Ende), `draw_jump` malt Treffer/Labels. Copy-Mode: `w`/`b`/`e` (`word_motion`, Wörter =
  alles zwischen Leerraum, `WORD_ROWS` um den Cursor), `V` zeilenweise. Tasten des Copy-Modes nur in `bindings::COPY_KEYS` (Tafel
  `menu::draw_copy_keys` im Copy-Mode, `i` bzw. `which_key.copy_mode` blendet sie aus, Hilfe,
  `imkr keys`). `Ctrl+B j` oder `s` im
  Copy-Mode; Copy-Mode auch auf `Ctrl+B y` (`[` braucht auf QWERTZ AltGr).
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
  Codex-Ausnahme (#1335): `question_for` erkennt ausschließlich aktuelle Dialog-Footer
  (`codex_dialog`, auch MCP-Allow, schmale Panes/Notizen, Codex 0.163.0). Diese schlagen Output und
  alte Hook-Meldungen; `answered` gilt dort nicht, solange der Dialog weiter angezeigt
  wird (Tab/Pfeile sind keine Antwort). Echte TUI-Fixtures: `src/fixtures/codex-0.163.0/`;
  Messung/Grenzen: `docs/validation/codex-blocked-1335.md`.
- Rechner-Identität: `protocol::machine_id()` (/etc/machine-id; `IMKR_HOSTNAME` in Tests) für Hives,
  `machine_name()` (Config `name`, sonst Hostname) für Anzeige und Origin – zwei Omarchy-Rechner
  heißen beide `omarchy`! Server nimmt `newest_origin()` (Client mit `via`), nicht stur den aktiven.
  Client ersetzt sich selbst (`reexec_if_replaced` im origin-Thread, Ziel in `IMKR_REEXEC`).
- Agent-Metadaten: `imkr status` reicht `transcript_path` aus dem Hook-JSON als
  `Request::Status::transcript` weiter (auch im Handover), `Pane::refresh_meta` (je Tick, nur bei
  geänderter mtime) → `agent::transcript_meta` (letzte `usage` in den letzten 512 KB) →
  `Meta::label` („opus 5.5 · 300k“), in `list()` als `meta`, `AgentInfo::meta`. Keine Kosten:
  bräuchte eine Preistabelle.
- Konten: `src/account.rs` (`Store` mit `root` = `<state>/accounts`, `home`; je Konto Ordner
  mit Links auf `~/.claude`/`~/.codex` außer Login (`own_file`), `.claude.json` als Kopie ohne
  Identität (`account_bound`), MCP-Server bei `sync` neu; Anbieter per `url`/`token`/`model` →
  `env`; `used-up.json`, `freest`, `used_up_in` erkennt die Limit-Sätze samt Reset-Zeit).
  `Pane::accounts`/`App::accounts` (je Tool, `standard` = kein Eintrag), über `Spawn::accounts`
  in die Shell-Umgebung, gespeichert in `Saved`/`SavedPane`, im Handover über `saved`.
  `Request::UseAccount` (ohne `pane`: neue Shells; mit: `switch_account` = SIGTERM per
  `Pane::stop_foreground`, dann `finish_switches` tippt `imkr account run T:N -- <resume>`).
  `check_used_up` je Tick (nur nicht-working, `USED_UP_LINES`, `USED_UP_QUIET`), Einstellung
  `account_fallback` off/ask/auto (`AccountFallback::ALL`, eine Reihenfolge für Menü und Taste).
  UI `Ctrl+B s c` (`Mode::Accounts`).
  Freiestes Konto statt nächstes (#9102): `usage::report` legt die Zahlen in
  `<state>/accounts/usage.json` (`keep_in`, `cached`, `FRESH` 120 s), `Store::freest(tool,
  current, numbers)` sortiert die Kandidaten nach `usage::fullness` = (Woche, 5h) — die Woche
  entscheidet (kommt erst in Tagen zurück), 5h nur bei Gleichstand; wirklich am Limit stehende
  Konten sind über `used_up` ohnehin draußen. Ohne Zahlen bleibt die Listenreihenfolge
  (`free_after`). Entschieden wird nicht sofort: `check_used_up` und `Ctrl+B s C` legen einen
  `AccountChoice` ab, `measure_usage` misst im Thread (schreibt nur den Cache), `decide_accounts`
  am ENDE von `update_agents` (sonst überdeckt „stopped: limit reached" die Meldung) handelt,
  sobald der Cache frisch ist oder `USAGE_WAIT` (25 s) um ist. Meldungen nennen beide Konten mit
  Prozent und den Namen aus `account::shown`, nicht den Registernamen. Tasten: `Ctrl+B s C`
  (`FreestAccount` → `move_to_freest_account`, fokussiertes Pane), `Ctrl+B s U`
  (`AccountFallback` → `step_account_fallback`); `imkr account` und die Kontoliste zeigen die
  Woche aus dem Cache (600 s), fragen aber nie selbst — das tut `imkr account usage`.
  Ein Wechsel ist ein **Neustart** (#1305): die Umgebung eines laufenden Prozesses ist von außen
  nicht änderbar, `switch_account` beendet den Agenten und `finish_switches` tippt
  `… -- claude --resume <id>`. Darum vorher `Store::conversation(tool, session)` →
  `account::Conversation` (`Found`/`Missing`/`Unknown`): `Missing` bricht ab, **ohne** SIGTERM —
  eine frische Sitzung hat noch keine gespeicherte Konversation, `--resume` endet mit „No
  conversation found" und der Agent ist weg (so starb die Biene `fallback-konto`). Gesucht wird
  `<account>/projects/<ordner>/<id>.jsonl`; `projects` ist ein Link auf das gemeinsame
  `~/.claude/projects`, deshalb trägt ein Wechsel die Konversation überhaupt mit. `Unknown`
  (ohne Session-ID → `--continue`, oder codex) läuft wie bisher. Danach beobachtet
  `App::check_resumed` (`RESUME_GRACE` 4 s, `RESUME_WAIT` 60 s, `account::resume_failed`) den
  Neustart und meldet, wenn er trotzdem scheitert oder der Agent nicht zurückkommt — eine
  Sitzung darf nicht stumm sterben. Quittungen sagen „restarts … (same conversation)", nicht
  „goes on in": die alte Zusage las sich harmlos. **Falle:** ein Neustart ist ein SessionEnd,
  `Pane::takes_mail` fällt weg (`app.rs` Status mit `state: None`) und wird erst vom ersten Hook
  des neuen Agenten wieder gesetzt — bis dahin läuft die Zustellung über den getippten Weg.
  Zwei Musterlisten für denselben Bildschirm (#1305): `agent::FAILURES` entscheidet die
  **Anzeige**, `account::used_up_in` das **Handeln** (einziges Tor zum automatischen Wechsel,
  `check_used_up` ist der einzige Aufrufer). Sie liefen auseinander — `FAILURES` traf „weekly
  limit" als Teilstring, `used_up_in` wollte „weekly limit reached" — also stand der Agent auf ✗
  und wechselte nicht. Nicht zusammengelegt (`FAILURES` umfasst `API Error`, `overloaded`: die
  dürfen kein Konto wechseln), aber sie nennen sich gegenseitig im Kommentar; eine Formulierung
  gehört in beide. `used_up_in` trifft die Satzfamilie `hit your <was> limit` (`hit_your_limit`),
  nicht einen Wortlaut; `reset_date` liest „resets Nov 5, 4am" als Datum (die 5 als Stunde ließ
  das Konto ~23 h zu früh als frei gelten). Fenster nicht angleichen: `failure` liest 8
  Bildschirmzeilen **roh**, `used_up_in` bekommt `agent::tail` (30 Zeilen zurück, ohne Rahmen und
  Leerzeilen, davon die letzten 8).
  Was `imkr account ls` über den Fallback sagt, steht in `AccountFallback::explained()` **neben
  dem Enum**, je Modus, mit dem Namen der Einstellung — vorher versprach die Zeile den Fallback
  als Tatsache, auf einem Rechner mit `ask`, der nie von selbst schaltete. Test
  `account_ls_describes_every_fallback_mode_truthfully` prüft **alle drei** Modi.
  Logins: `src/login.rs` (`take_renewer` = `renew.lock`, ein imkr je Rechner meldet in
  `App::watch_logins` alle `LOGINS_EVERY` einmal `dead` und `ending` (3 Tage vorher) per
  `send_notification`; `renew` = Claude Code ein Wort antworten lassen, mit Sperre je Konto in
  `accounts/locks/` und Neu-Lesen unter der Sperre; `claude_runs_in`). Refresh-Token-Ende ist fest
  ab `/login` (~30 Tage), Erneuern verschiebt es nicht → keine Dauer-Erneuerung, nur Vorwarnung.
  `Pane::spawn`: gespeichertes Konto abgemeldet → `default_of`.
  Agent mit Kontowahl: `Ctrl+B k` (`Command::NewAgent` → `new_agent`; Ziel `Config::new_agent_in`
  pane|window, `p` in der Liste schaltet um, `pane_for_agent`: Shell vorn → dieses Pane, sonst
  `split(Row)`, kein Platz/Popup → neues Fenster) und Vorlagen-Fenster mit
  `claude`/`codex` in `restore` (nur bei >1 eingeloggtem Konto) → `AgentStart` + `Mode::AgentAccount`
  (`agent_start_items`, `start_agent` tippt `imkr account run T:N -- <cmd>`, Esc startet Vorlagen
  im Standardkonto); `attach` lässt eine wartende Frage stehen. Befehl je Agent: `agent_commands`.
  Identität je Konto (`Store::identity`: Claude `oauthAccount` in `.claude.json`, Codex `id_token`
  in `auth.json`), `same_login` warnt bei doppelten Abos nach `add`/`login` (löscht nie); `login_dead`/`login_spent`: leerer oder abgelaufener Refresh-Token = abgemeldet → keine Identität, nicht `logged_in`, `default_of` nimmt dann das erste eingeloggte, `used_up_for` gilt je Abo.
  Pane-Rahmen: `App::name_accounts` (alle `ACCOUNTS_EVERY`) → `Pane::shown_account`
  (`account::shown`: Adresse vor dem @, bei Anbietern `provider_name` + Modell), dahinter das
  Modell aus `Meta`. Test: Fake-`claude` im
  Scratchpad-bin, das die Limit-Meldung druckt, wenn `CLAUDE_CONFIG_DIR` fehlt.
- Verbrauch: `src/usage.rs` (`report`: je Konto einmal je Abo (`Identity::key`), Claude per curl
  `api.anthropic.com/api/oauth/usage` mit dem Token aus `.credentials.json` auf stdin (`-H @-`, nie
  argv), nie selbst erneuern: alter Schlüssel → `renew_key` lässt Claude Code ein Wort mit Haiku
  antworten (`--no-session-persistence --tools "" --strict-mcp-config --setting-sources ""`), nur
  wenn `claude_runs_in` nichts findet (Chrome-Helfer `--chrome-native-host` zählt nicht);
  `claude auth status` erneuert NICHT; Codex aus `rate_limits` der neuesten
  `sessions/**/*.jsonl`; Tokens je Agent aus den geteilten Transkripten, Claude je `message.id`
  einmal, ohne Cache-Lesen). CLI `imkr account usage [--json] [--wait]`, `Ctrl+B s u` =
  `Command::Usage` → Popup mit `--wait`.
- Zustand `AgentState::Failed` (✗, magenta, Rang nach Blocked): Hook `StopFailure` → `imkr status
  failed`, Grund aus `error` per `agent::failure_reason` → `Request::Status.reason` → `Pane::failure`.
  Ohne Hook: `agent::failure` (Fehlertexte `FAILURES` in den untersten 8 Zeilen) + `QUIET_ERROR`
  (5 s still) macht Working/geraten zu Failed; Idle/Blocked vom Hook bleiben. Meldung + Flash in
  `update_agents`, auch im Blick. `Pane::failure_reason` für Liste/Meldung.
- Claude-Hooks: `cli::HOOKS` (Ereignis, Matcher, Zustand) für `imkr hooks` und `--install`
  (Skript + Merge in settings.json, alte imkr-Hooks raus, Backup; StopFailure nur, wenn kein
  `claude` im PATH älter als `STOP_FAILURE_SINCE` 2.1.78 ist, sonst verwirft Claude Code die ganze
  settings.json, #8751). Codex (#9532, #1357): `CODEX_HOOKS` → `install_codex_hooks` mischt sie
  in `$CODEX_HOME|~/.codex/hooks.json` **hinter** fremde (herdr): Codex vertraut Hooks je
  Position (`hooks.state` in config.toml), neue müssen einmal freigegeben werden („Hooks need
  review“, `t`) — nie selbst `trusted_hash` schreiben. Gemessen an Codex 0.163: Hooks laufen in
  der Pane-Umgebung (`IMKR_PANE`), JSON wie Claude (`session_id`, `transcript_path`, `prompt`),
  SessionStart erst mit dem ersten Prompt; die TUI selbst hält keine Rollout-Datei offen (der
  geteilte `codex app-server` schreibt). Aufruf `imkr status S --agent codex`: Session, Zustand,
  Prompt-Nachweis (`took_input` nimmt `prompt_took`, sobald `says_prompts`) und seit #1358 auch
  `mail_for_hook` (`HOOK_MAIL_AGENTS`, `hook_takes_mail`): gemessen am echten Codex 0.163
  (Dummy-Provider), `Stop` mit `decision:block` bleibt im selben Turn (gleiche `turn_id`, danach
  `stop_hook_active`), `reason` kommt als `<hook_prompt>`-User-Nachricht, `additionalContext`
  als Developer-Nachricht hinter dem Prompt; Codex zeigt „Blocked by hook". Damit hält
  `mail_waits_for` einen Codex mit `takes_mail` nicht mehr als „working" (`queues` ||
  `fetches_mail`). Codex hat in `CODEX_HOOKS` kein `SessionEnd`: `App::fetches_mail` →
  freie Funktion `app::fetches_mail` gilt nicht mehr, sobald wieder eine Shell vorn steht.
  Ohne freigegebene Hooks (Option 3 bei „Hooks need review") läuft keiner, `takes_mail` bleibt
  aus, Post wird wie bisher bei idle getippt. E2E: `python3 tests/codex_hook_mail.py --codex
  <pfad>` (zwei CODEX_HOMEs, weil Codex das Vertrauen in config.toml ablegt; Tasten per
  `imkr tap` roh, `send` würde sie als Paste rahmen); gegen 8ad0f8b fällt er mit „held: working". Resume mit Optionen:
  `Config::resume_commands` setzt `agent_commands` vorn ein (`codex --yolo resume {session}`);
  `switch_account` lehnt Codex ohne Session ab statt `--last`. pi: Erweiterung
  `integrations/pi/imkr-agent-state.ts` (`PI_EXTENSION`, `install_pi_extension` nach
  `~/.pi/agent/extensions/` wenn `~/.pi/agent` da), ruft `imkr status` mit `session_id` auf stdin;
  Resume `pi --session {session}`. Test ohne API: Node-Skript lädt die .ts (Type-Stripping) und
  löst `session_start`/`agent_start`/`agent_end` aus, als Programm `pi` in einem Pane. `Notification` mit
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
- Pane für Programme (#530/#529, AI-Brain-Harness): `imkr tap ADRESSE` (`cli::tap`, `TapTarget`,
  `Request::Tap` mit `watch`), JSON-Zeilen `screen`/`out`/`gap`/`exit`, Eingabe `text`/`input`,
  `{"screen":true}` = frisches Bild. Gebündelt `TAP_EVERY`, Deckel `TAP_MAX` (Reverb verwirft
  >100 KB still; Harness nimmt `--max 40000` für ≤60 KB JSON); Lücke ⇒ neu verbinden ⇒ Vollbild,
  `fit_screen` kürzt den Verlauf (Klartext vor dem ersten ESC) vorn. Ein Kanal für alle
  Verbindungen, Ereignisse tragen die Verbindungsnummer (alte Leser zählen nicht).
- imkr web in AI Brain (#530): `web/index.html` hat eine Transportschicht (`transport`: state/send/
  terminal). Im Frame (`window.parent !== window`; imkr web selbst schickt `X-Frame-Options: DENY`)
  geht alles per `postMessage` an die Elternseite (`ask`, `BridgedSocket`, Nachrichten `imkr:
  state|send|open|input|close` hin, `reply|data|closed` zurück), sonst fetch/WebSocket wie bisher.
  Asset-Pfade relativ (AI Brain setzt `<base>`). Für die Harness: `imkr web page|state|send`
  (`web::page`, `web::state`, `web::send`); die drei sterben still an geschlossener Pipe.
- Orchestrierungs-Ansicht `Ctrl+B O` (#534): `App::mail_log` (`protocol::MailNote`, `MAIL_LOG`,
  `note_mail`), gefüllt beim Zustellen (`Request::Message`, nur Absender mit `:`) und von
  `imkr msg` selbst (`cli::note_sent` → `Request::Sent`, nur ohne `--from`, Fehler egal).
  `crew_rows` (Panes hier mit Notizen, Absender zuerst; Ziele per `cli::parse_address` →
  `crew_peer`: Pane hier oder `other_agents`), `crew_key`, `draw_crew`; im Handover `mail_log`
  (und seit #8774 `mailbox`).
- Keeper und Bienen (#9133): `src/keeper.rs`. `imkr keeper [NAME]` (`free_name` → `keeper`,
  `keeper-2`; `session_taken` = laufend oder gestoppt, closed gibt den Namen frei), Ordner
  `<state>/keeper/NAME/` (`briefings/`, `keeper-stand.md`), `start` = `start_server` + `New`
  bzw. `Worktree { fresh }` mit `<agent>` statt der leeren Shell, `wait_ready` (zweimal
  idle/done; blocked → `Ready::Asking`: nichts bestätigen, Meldung mit Adresse und Befehl,
  Exit 1), dann `first_input` (`/keeper`, Codex `$keeper`). Skill liefert imkr NICHT
  (kommt aus AI Brain), `check_skill` sucht `~/.claude/skills`, `CLAUDE_CONFIG_DIR`, Konten
  (`skill_dirs`), sonst `missing_skill` mit harness-Befehl. `imkr spawn NAME --brief F|-`:
  Briefing als `Request::Message` (Absender `own_address`), `bees.json` (flock) nur auf der
  Keeper-Seite (`--keeper`, sonst Session-Name). `--host` liest main global → ein ssh-Aufruf
  `spawn … --brief - --from ADR --json` (drüben `<state>/bees/NAME.md`, 0600; ohne `--from`
  zeichnet nicht auf). `crew_rows` hängt per `bee_at` (Session-Name) Task an Hinweis und
  Briefing an die Vorschau.
  `spawn` setzt `by` (= `protocol::caller()`, remote per `--by`) auch über `Request::Worktree`
  (`App::take_by`), so stehen Bienen unter ihrem Keeper. Keeper-Marke: `App::keeper` ↔
  `Saved::keeper` (Neustart, revive, Handover), `list` trägt `"keeper"` je Pane →
  `SessionInfo::keeper` (`ls --json`, remote, Web). `Request::Keeper { mark }`, CLI
  `keeper --mark|--unmark [SESSION]`. Anzeige `keeper::MARK` (⚑, accent): Sidebar
  (`SessionRow`/`OtherAgentRow::keeper`, `Data::keeper`), `Ctrl+B S`/`Ctrl+B A` über
  `App::keeper_places`, `badge_text(keeper)`, `window_title_text`, `imkr ls`/`agents` (Marke am
  Zeilenende, die Adresse bleibt erstes Wort), `web/index.html`. Keeper zuerst je Rechner
  (Sortierung nur innerhalb einer Rechner-Gruppe).
  UI (#9291): `NewStart` (Shell/Template/Keeper) in `template_choices`, ⚑ keeper zuletzt und nur
  bei `offer_keeper` (`keeper::has_skill_here`, einmal in `choose_template`), sonst ohne Vorlagen
  kein Zwischenschritt. `PromptFor::SessionDir(place, NewStart)` → `start_keeper` → Kontowahl über
  `AgentStart::keeper` (nur lokal) → `launch_keeper` ruft `imkr [--host H] keeper NAME --dir D
  [--account T:N]` im Thread, danach `Request::Go` an den eigenen Socket (auch bei „waiting for an
  answer“). `Ctrl+B S` `f` = `toggle_keeper` (lokal `Request::Keeper`, fremd per ssh), Auswahl
  folgt dem Eintrag. `--account` (`keeper::agent_for`: NAME oder TOOL:NAME, muss eingeloggt sein)
  → Platzhalter `<tool@name>`; `placeholder_account` trägt das Konto ins Pane.
  Erste Shell (#9291): `new --start --window 0` (fresh) **tippt** das Programm in Shell 0
  (`App::type_first`, `cd` nur wenn der Ordner abweicht; `client::start_server_in` startet den
  Server gleich dort) statt sie zu ersetzen → Agent auf Pane 0 mit Shell darunter, `switch_account`
  geht. Gemessen mit echter zshrc: Typeahead vor dem Prompt kommt an. Falle: gab es die Session
  als gestoppte (State-Datei), stellt `--start` sie wieder her und das Programm landet daneben
  (doppelter Agent) — `cli::new` sagt das auf stderr, keeper/spawn lehnen per `session_taken` ab.
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
  (Einklinken), offline bleibt offline. Wechseln: `client::hive_outsiders` (laufende Sessions
  der Hive-Rechner, die er nicht hat; `hive_machines`/`is_this_machine`) → `switch_hive`
  (restore, dann `kill-server` je Außenseiter, die eigene Session lässt `keep` dem Aufrufer).
  UI `w` in `Ctrl+B H` → `Action::SwitchHive` mit `confirm_list` (`hive_switch_ends` aus
  `places`), `App::switch_hive` beendet die eigene Session zuletzt (Clients ziehen weiter). Test mit mehreren Welten: `IMKR_HOSTNAME` je Welt setzen,
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
- Ende ohne Panes (letzte Shell `exit`, `Ctrl+B w x` aufs letzte Fenster): `close`/`close_workspace`
  merken vorher `final_layout` (`keep_final_layout`), das Ende legt es nach `last` – sonst ging die
  Session ganz verloren (#8544).
- Bilder auf andere Rechner (#8581): Client-Eingabe-Thread fängt Ctrl+V ab, wenn `SHOWN_HOST`
  gesetzt ist und `clipboard::local_image` (wl-paste/xclip, 2 s Frist) ein Bild liefert →
  `Request::PasteImage` (base64, ≤15 MB) → `App::paste_image` legt es in `paste_dir()`
  (`~/.cache/imkr/paste`, 0600, >3 Tage weg) und fügt den Pfad per `paste` ein. Sonst geht
  Ctrl+V durch.
- Eine Person = ein Konto (#8577): `Store::last_identity` (auch abgemeldet), `left_over` →
  `listed(tool)` statt `of_tool` in Listen, Login-Wache, Nutzung. `account add NAME`: gibt es
  NAME → neu anmelden; `logged_out_as` (abgemeldetes benanntes Konto dieser Person) → das
  anmelden; nach dem Login `same_person` (nur benannte Ziele) → `move_login` (Datei verschieben,
  `account_bound`-Schlüssel), neues Konto weg, außer `account_in_use`. Nie in `standard`
  hineinziehen (Name ginge verloren, Kontolisten-Sync ist eine Vereinigung).
- Worktrees (#8567): `worktree::Setup` (`copy`/`symlink`/`post_create`/`pre_remove`) aus Config
  `[worktree]` + `.imkr.toml` (`setup_for`, Listen ersetzen, `"<global>"` fügt ein), `prepare`
  (Globs `expand`/`fits`) nur bei frischem Worktree in `worktree_window`, `post_create` per
  `sh -c` vor dem Programm. `list`/`find`/`merge` (nie checkout, Konflikt → abort)/`remove`
  (ohne --force nur sauber+gemergt). CLI `imkr worktree ls|merge|rm`, `panes_in` VOR dem
  Entfernen (danach ist cwd „(deleted)“), eigenes Pane bleibt.
- imkr-Meldungen im AI-Brain-Posteingang (#536): `src/brain.rs` (`Note`, `report` im Thread wie
  push, `connected` = Harness im PATH + `connection.json` + **keine Testwelt** (`test_socket`),
  `read` je `BRAIN_READ_EVERY` in `watch_places` mit eigener Sperre `imkr-brain.lock` und
  `imkr-brain-seen.json`, mindestens einmal wie bei den Kontakten). Angedockt an `App::push`:
  ein Tor, zwei Wege — `PUSH_AGAIN` gilt für beide, `quiet` **nur** fürs Handy (der Posteingang
  ist eine Liste, keine Unterbrechung). Kontakt-Nachrichten meldet `contacts::tell_brain`.
  Zurück: `Request::Seen { pane }` nimmt das ✦ (`unseen`). Einstellung `brain_inbox` (Standard
  an; `read` liest sie aus der Datei, damit Abschalten ohne Neustart wirkt). Vertrag mit der
  Harness: `ai-brain-harness meldung senden --art blocked|done|failed|account|message --rechner
  R --sitzung S --pane N [--fenster N] [--agent A] [--link U] <titel>` (Text auf stdin),
  `meldung gelesen --json` → `{"gelesen":[{id,sitzung,pane,art,rechner,abholungen}]}`,
  `meldung bestaetigen <id>`. Drüben: MCP `report-imkr-event-tool`, `fetch-imkr-read-tool`,
  `confirm-imkr-read-tool`, Tabelle `imkr_meldungen`, Inbox-Art `imkr` (`App\Inbox\ImkrQuelle`
  als `AbhakbareQuelle`+`ZaehlbareQuelle`), Aufräumen nach 30 Tagen. Test: Testwelt mit
  Schein-`ai-brain-harness` im PATH, eigenem `XDG_CONFIG_HOME` samt `ai-brain-cli/connection.json`
  — ohne die Datei meldet imkr nichts.
- Computer-Use (#9074): `src/screen.rs`, ein Trait je Fähigkeit (`Sees` Monitore+Screenshot,
  `Points` Zeiger+Klick, `Types` Text+Taste, `Checks` Vorabprüfung; `Desktop` bündelt sie,
  `desktop()` liefert das Backend oder sagt, was fehlt — macOS/Windows sind eine benannte
  Lücke mit `gaps()`, nicht ein stummes Nein). Linux/Hyprland spricht den **Sockel direkt**
  (`$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`, Signatur und `WAYLAND_DISPLAY` selbst gesucht:
  in einer Pane und über ssh fehlen beide), `grim -s` (rechnet auf die **logische** Größe,
  darum ist die Rückrechnung eine Division), `grim -g` für `--region`, Zeiger per
  `dispatch hl.dsp.cursor.move{ x = …, y = … }` (Hyprland 0.56 wertet den Rest als Lua; exakt
  und absolut, `ydotool --absolute` sättigt), Klick per `ydotool click 0xC0`, Text per `wtype -`
  (eigenes Keymap, Text nie in argv). **Falle:** `j/monitors` gibt `x`/`y` logisch, `width`/
  `height` in Gerätepixeln — durch `scale` teilen. **Falle 2:** `wtype` dekodiert über die
  Locale; unter `POSIX` verschluckt die stdin-Form ab dem ersten Nicht-ASCII-Byte den Rest
  **still** und meldet Erfolg → `tool()` setzt `LC_ALL=C.UTF-8`, `check_utf8` fragt
  `locale charmap` (wtype selbst zu fragen geht nicht: jede Form, die dekodiert, tippt auch).
  Gesperrt = nichts (`session_locked`: `LOCK` in `solitaryBlockedBy`, steht auch ohne Locker;
  dazu laufender hyprlock/swaylock). Freigabe je Rechner: `computer_use = off|ask|on` und
  `computer_use_monitor` in der Config, **beide in `settings::LOCAL_KEYS`** (Test
  `computer_use_stays_on_its_machine`); `ask` → `cli::permit` schreibt
  `<state>/computer-use.json` (flock) und schickt `Request::ScreenAsk` an die Sessions
  (`ask_sessions`: bei eigenem `IMKR_SOCKET` **nur** diese — sonst steht die Frage in den
  echten Sessions) → `App::screen_ask` → `Mode::Confirm(Action::AllowScreen)` + Notification;
  `y` = `allow` (30 min), jede andere Taste = `refuse_screen` (sofort, nicht auslaufen lassen).
  Leiste: `App::screen_spans` aus `App::screen_use` (je Upkeep gelesen). Protokoll
  `<state>/screen.log`, **lokale** Zeit, Text nur als Zeichenzahl. CLI `imkr screen
  status|shot|move|click|type|key|log|allow|refuse` (`--host H` über `remote_output`,
  Screenshot als base64 zurück), MCP `imkr_screen_*` (Screenshot als `image`-Block).
  Test ohne Compositor: `Hyprland::at(socket)` + Fake-Sockel mit echter `j/monitors`-Antwort —
  so ist „gesperrter Rechner wird abgelehnt" messbar, ohne jemandes Bildschirm zu sperren.
  Am echten Schirm: Testwelt mit eigenem `IMKR_SOCKET`/`IMKR_CONFIG`/`XDG_STATE_HOME`, aber
  **echtem `XDG_RUNTIME_DIR`** (sonst findet es Hyprland nicht).
- Sessions mit Auftraggeber (#9087): `imkr new --by ADRESSE` (`cli::new`) setzt `Request::New.by`
  → `App::by`, gespeichert in `Saved::by` (und damit im Handover über `saved`), also über
  Neustart, `revive` und `update`. Ohne `--by` nimmt `new` den Aufrufer, **aber nur wenn
  `--start` den Server in genau diesem Aufruf gestartet hat** (`fresh`, #9068) —
  `protocol::remember_caller`/`caller()` liest `IMKR_PANE`+`IMKR_SESSION` in `main`, **bevor
  `-s NAME` IMKR_SESSION überschreibt**. Weg nach außen: `App::list` trägt `by` auf **jedem**
  Pane (wie `workspace_name`; das Reply ist eine Liste), `client::local_sessions` nimmt es vom
  ersten, ruhende aus `Dormant::layout.by` → `SessionInfo::by` → `ls`, `ls --json`, remote.
  Anzeige: `App::boss` (`Boss::Running|Gone|Unknown`; `Gone` nur über Rechner, die antworten,
  `Places::polled` trennt „noch nicht gefragt“ von „läuft nicht“), `App::by_place` (Maschine
  **wie geschrieben**, `cli::address_parts` ohne das Wegfiltern des eigenen Namens),
  `App::by_label`, `App::boss_map` (einmal je Adresse, nicht je Zeile je Frame), freie Funktion
  `tree_order` (Kinder direkt hinter dem Eltern-Eintrag, Zyklen bleiben Wurzeln). Seit #9621 auch
  **über Rechner**: `by_place_from` liest die Adresse aus Sicht des Schreibers (ohne Rechner =
  dessen Rechner, unser Name → `None`, sonst der Host), die Biene wandert unter ihren Keeper.
  Weil die Listen nach Rechner gruppieren und eine Gruppe zusammenhängen muss, kommt jede Zeile
  in die Gruppe ihrer **Wurzel** (`root_hosts`): Sidebar setzt `SessionRow::machine` darauf und
  `SessionRow::elsewhere` = eigener Rechner (`App::machine_word`, „· codingmachine“ hinter dem
  Namen), `Ctrl+B S` hängt ihn an den Titel und nimmt das eigene Rechner-Label aus dem Hinweis.
  Ziel (`host`) bleibt das der Biene. Auftraggeber nicht in der Liste → Wurzel in eigener Gruppe.
  Sidebar `SessionRow::by`/`depth` (`↳`, Unterzeile `by …`), `Ctrl+B S` über `SessionEntry`
  (6. Feld), `Ctrl+B O` über `App::commissioned`/`crew_session` (Baum **über** Rechner, Gruppe
  je Auftraggeber, auch wenn er keine Nachricht geschickt hat). Verwaist = nur Marke, nie Ende.
  `imkr agents --by ADRESSE` (`cli::same_address`). Test: zwei Welten mit Fake-ssh; Falle:
  `new --start --window 0` legt das Programm auf Pane **1**, `--by host:session/0` zeigt dort
  ins Leere (und wird korrekt „gone“). Seit #9291 Pane **0** (siehe Keeper).
- Hotkeys Computer-Use (#9095): `Ctrl+B s e` = `Command::ComputerUse` → `App::step_computer_use`
  (`screen::ComputerUse::ALL` + `config::step_in`, dieselbe Reihenfolge wie das Menü, speichert
  sofort), `Ctrl+B s E` = `Command::AllowScreenNow` → `App::allow_screen_now` (`screen::allow`/
  `refuse` als „the user here“, bei `off` gibt es nichts: `cli::permit` weist vorher ab).
  `s x` ist „end this session“ — ein Schalter gehört nicht neben die zerstörendste Taste.
  Das Speichern schreibt die **ganze** Config (Test `stepping_computer_use_keeps_the_rest_of_the_config`):
  Werte bleiben, Kommentare nicht — so war es bei jedem Schalter schon vorher.
- Ablageordner (#9082): `src/share.rs`, Datei `~/.config/share-dir.toml` (`dir`, `[elsewhere]
  host = pfad`) — **nicht** in imkrs Config: imkr und der AI-Brain-Harness lesen und schreiben
  sie beide, jedes läuft auch allein. Feldnamen sind ein Vertrag, `imkr share --json` druckt
  sie, `Shared::rest` (serde flatten) bewahrt fremde Schlüssel beim Schreiben. `put()`
  verschiebt (kein zweites Bild vom Schirm liegen lassen), `elsewhere()` benennt die Datei für
  die anderen Rechner. `imkr screen shot --save` ist der erste Nutzer; ohne Ordner läuft alles
  weiter mit `missing()`. `imkr share setup` (auch `Ctrl+B s f`) bietet Mounts aus
  `/proc/self/mounts` und Sync-Ordner an und **rät unbeaufsichtigt nicht** (ein erster Versuch
  nahm den erstbesten SMB-Share). Kein SETTINGS-Eintrag: `Setting::step` muss ohne Seitenwirkung
  umkehrbar sein, sonst schreibt der Test `stepping_back_undoes_stepping_forward` in das echte
  `~/.config`.
- Push (#8566): `src/push.rs` (`[push]`: ntfy-Themenadresse → JSON an die Server-Wurzel,
  webhook → `push::Event` als JSON, per `curl` im Thread). `App::notify(id, kind, what)` ruft
  `App::push`: `wants(kind)`, nicht wenn ein Client in `quiet` s getippt hat (`user_at`), je
  (Pane, Art) höchstens alle `PUSH_AGAIN` (Zustand flackert sonst doppelt). `imkr push test`.
- Agent-Platzhalter (#8565): `Config::expand_agents` (`<claude>`, `<agent>` = `default_agent`,
  `<claude@konto>` → `imkr account run`; nur bekannte Agents/`agent_commands`, sonst bleibt der
  Text). Angewandt in `restore` (Vorlagen-`command`), `App::agent_argv` vor `new_pane` (new,
  worktree, fleet schickt `<agent>`), `template` speichert Agents als `<name>`.
- Fenstertitel (#8564): `term::Listener` merkt `Event::Title`/`ResetTitle`, `Pane::title` →
  `clean_title` (Spinner weg, nur Programmname = leer); über `list` (`"title"`) in
  `AgentInfo::title`, Sidebar-Zeile unter dem Agent (`AgentRow`/`OtherAgentRow::title`),
  `with_title` in `Ctrl+B A`, `imkr agents`; `HandoverPane::title`. Zeilen unter einem Agent mit
  gleichem Ziel gehören zu ihm (`selection_index`, `targets` dedupt).
- Archiv (#506): was `keep_newest` aus `last` drängt, geht nach `saves/.archive.json`
  (`persist::Archived` mit `hive`, `archive`/`unarchive`/`archived`), nie weg. Die Marke kommt aus
  `Hive::hive_of` in `last`, gesetzt in `keep_in_last` per `hive_for` (aktiver Hive, wenn er die
  Session hat, sonst der neueste gespeicherte, der sie hat — nicht nach Zeitpunkt: beim Wechsel ist
  der neue Hive schon aktiv, wenn die alten Sessions enden). `revive` fällt auf das Archiv zurück,
  `forget` räumt es mit. UI: eigene Ansicht der Session-Liste (`show_archive`, nur in
  `Mode::Sessions`, Titel „Archive“, Gruppen `▣ archive · HIVE`, leer mit Hinweis, Enter startet
  dort nie Neues): `a` in `Ctrl+B S` und `Ctrl+B s a` (`Command::Archive`; „save all as hive“ dort `v`),
  `choose_template`/`go_to` holen archivierte Namen zurück. Punktdateien zählt `saves()` nicht.
  Andere Rechner: `client::pull_archive` (ssh `imkr archive export`, im Takt von `pull_hives`) →
  `persist::store_replica` (`saves/.archive.<host>.json`, `Replica`), die Ansicht zeigt eigene +
  `replicas()` je `▣ HIVE · RECHNER`; Enter `revive_elsewhere` (antwortet → `imkr revive` dort,
  sonst `Action::StartArchivedHere` → `start_archived_here` mit `client::start_from`), `x` →
  `forget_elsewhere`, danach `drop_from_replica`. Hier Gestartetes merkt `note_taken`
  (`.archive-taken.json`); `pull_archive` schickt erst `imkr archive drop NAME SAVED_AT` (nur genau
  der Eintrag, nie `forget`: der löschte auch eine dort neu laufende Session gleichen Namens).
- Session-Zustand: `protocol::SESSION_ORDER` (blocked, working, done, idle) für `urgent()` (Farbe)
  und `counts()`; Sidebar und `Ctrl+B S` zeigen je Agent einen Punkt mit seiner Fensternummer
  (❶–❿, U+2776…, ab 11 `●11`, ohne bekanntes Fenster `●`; `SessionInfo::dots` aus `agent_panes`,
  eigene Session `App::own_dots`, `protocol::sort_dots`; `sidebar::agent_dots`, ruhige grau; im
  Menü über `MenuItem::marks`). Nicht die Anzahl: zweimal ❶ ließ sich nicht auseinanderhalten.
  Einstellung `nerd_font` (`sidebar::set_nerd_font` in `apply_theme`): Nerd-Font-Kreise
  `md-numeric_N_circle` (U+F0CA0 + 2·(n−1), 1–9), sonst ❶ aus einer Ersatzschrift (bei Victor Mono
  Noto CJK, breit gezeichnet → oval). Leerstelle HINTER jedem Punkt: Ghostty zeichnet ein
  Nerd-Font-Symbol bis zu zwei Zellen breit, wenn eine Leerstelle folgt; ohne waren sie
  unterschiedlich groß (der letzte stieß an den Rand).
  Vorher stand done vor working: Session grün, obwohl ein Agent noch arbeitete. Test: zwei Welten mit Schein-ssh, Datei `down`.
  Aktiver Hive (#507): `persist::active_hive`/`set_active_hive` (`saves/.active`, je Rechner, nie
  `last`), gesetzt von `switch_hive` und Speichern unter einem Namen, folgt `rename`/`delete`;
  `client::switch_hive` speichert ihn vor dem Wechsel (`own` = Layout der aufrufenden Session).
- Ruhende Sessions: `persist::dormant_sessions` (State-Dateien ohne laufenden Server = stopped,
  Hive `last` = closed, nur die `LAST_KEEPS` = 10 neuesten, `keep_newest`), `client::local_sessions` hängt sie mit `SessionInfo::dormant` an (also
  auch in `ls --json` und für andere Rechner). `go_to` holt closed per `persist::revive` (remote
  `imkr revive`) zurück, stopped stellt der startende Server selbst her. `imkr forget`, `x` in
  `Ctrl+B S`. Server erben keine Claude-Sitzungsvariablen (`client::is_agent_session_var`), sonst
  speichert Claude in den Panes keine Transcripts. Nach Neustart: `watch_places` schreibt die
  laufenden Sessions nach `<state>/running` (`persist::note_running`, nur ohne `IMKR_SOCKET`),
  `client::run` startet per `revive_stopped` die dort genannten, die jetzt stopped sind
  (Einstellung `revive_sessions`). Die Liste baut `to_bring_back`: laufende plus die der alten
  Liste, die jetzt stopped sind (beim Herunterfahren enden die Server nacheinander, der letzte
  ließe sonst die früheren fallen). Bewusst beendete fallen raus: State gelöscht bzw. closed.
  Sidebar zeigt keine ruhenden Sessions (`session_rows` → `retain`), nur `Ctrl+B S` (`o`).
  Ohne Client nach dem Boot (#9619): `src/boot.rs`, Einstellung `revive_at_boot` (in
  `settings::LOCAL_KEYS`, SETTINGS „Restart sessions at boot, without a client“, Wert
  `boot::shown`). Der Server spiegelt sie in eine systemd-User-Unit (`App::sync_boot` beim Start,
  nach jeder Settings-Änderung, `reload_config` → `boot::sync`: `imkr-revive.service` in
  `$XDG_CONFIG_HOME/systemd/user` + Link in `default.target.wants`, nur eigene mit `MARK`
  anfassen, kein `systemctl`), nie in Tests/Testwelt mit `IMKR_SOCKET`. Unit: `oneshot`,
  `RemainAfterExit`, `KillMode=process` (gemessen: per setsid gestartete Kinder überleben Ende
  und Stop der Unit), `ExecStart=… boot run` → `client::revive_stopped` (liefert jetzt je Session
  eine Zeile fürs Journal). `boot run` setzt `boot::ENV` (`IMKR_AT_BOOT`), der Server nimmt es
  in `server::run` heraus (`take_env`, Panes erben es nicht) und `restore` tippt dann **keinen**
  Resume-Befehl, sondern legt ihn in `App::resume_later` (#1357; `saved()` speichert daraus den
  Agenten weiter, sonst kannte das nächste Speichern nur die Shell und der Agent war für immer
  weg; im Handover). `attach` des ersten Clients → `resume_left_over` tippt ihn, nur wo
  `Pane::shell_in_front`. `imkr revive` und der Client-Weg tippen sofort. Ohne bekannte Session
  (`SavedPane::resume_command` = None, aber `agent`) sagt `restore` es (`not_resumed`, Flash +
  stderr, beim Boot über `resume_note` beim ersten Attach) — nie still.
  `boot::gaps` (kein systemd: `/run/systemd/system`; kein Linger: `/var/lib/systemd/linger/$USER`;
  macOS benannt) → Flash `boot::told`, `imkr boot [status]`. E2E: Testwelt **ohne**
  `IMKR_SOCKET` mit eigenem `XDG_CONFIG_HOME` (sonst schriebe der Server die echte Unit),
  Server per PID mit `kill -9`, `imkr boot run`.
- Tab-Vervollständigung: `src/completions.rs` (`COMMANDS`: Befehl, Optionen, Unterbefehle und welche
  Namen dort passen, `Words`), `imkr completions zsh|bash` erzeugt das Skript daraus, Namen fragt es
  beim Tab per `imkr complete KIND` ab. Neuer Befehl → Eintrag in `COMMANDS` (Test
  `every_command_is_offered` liest die Befehle aus main.rs). Test der Skripte: zsh mit Platzhaltern
  für `compadd`/`_describe`, bash mit gesetztem `COMP_WORDS`.
- `protocol::test_socket()`: `IMKR_SOCKET` zählt nur als Testwelt, wenn es außerhalb des
  Runtime-Verzeichnisses liegt. In einem Pane ist es der Socket der eigenen Session; vorher suchten
  `hive ls/save/restore/switch` aus einem Pane neben dem Socket und fanden keine Hives.
- SIGPIPE: Rust ignoriert es, `println!` paniced dann an einer geschlossenen Pipe. `main` setzt für
  reine Ausgabebefehle `SIG_DFL` (still Exit 141), nicht für die an Verbindungen lebenden
  (`lives_on_connections`: Client, attach, proxy, server, web, hosted, attach-pane). Neuer
  langlebiger Befehl, der Sockets schreibt → dort eintragen.
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
  `client::local_sessions`, Hosts alle 5 s (`HOSTS_EVERY`) per `ssh … imkr ls --json`). Sidebar-Abschnitt
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
  Session endet (`ServerMsg::Exited`): Client geht per `next_running_session` in eine andere
  laufende Session hier (Session-Liste offen, Flash); hat dieser Rechner keine mehr, in die
  zuletzt besuchte auf einem anderen (Host aus `last_place`, dann Config-`hosts` +
  `remembered_hosts`, je Host ein `remote_sessions`, Abbruch beim ersten Treffer, `is_stand_in`
  und dormant raus) — Host wandert in `SHOWN_HOST` und `remember_place`, damit blankes `imkr`
  dorthin geht und keine leere `default` anlegt (#1150). Läuft nirgends eine, Ende und
  `forget_place` (sonst startet `imkr` die beendete leer neu). Testwelten immer mit `unset IMKR_SOCKET`!
  Nach einem Wechsel zeigt der neue Server den Namen 1,2 s (`draw_banner`, `BANNER_FOR`, Ort
  `Caps::banner_at` = `session_banner_at`),
  übermittelt als `Caps::banner` in den ersten Caps, Einstellung `session_banner` des Client-Rechners.
  Falle: neue `Request`-Varianten nie ungefragt auf Attach-Verbindungen schicken – ältere Server
  können sie nicht parsen, `while let Ok(Some(msg))` endet und der Client fliegt raus. Neue Felder
  mit `#[serde(default)]` in `Caps` überliest ein alter Server dagegen.
- Settings-Sync: `src/settings.rs` (`collect`: config + `templates/` + `layouts.json` + eigene
  `plugins/*.toml`, keine Links; `plan` gegen den Stand nach dem letzten Sync (`base`,
  `.sync-base/<host>.json` (0600, enthält die Kontoliste), nur auf dem Rechner, der synct): wer seitdem geändert hat, gewinnt,
  die Config je Schlüssel (`config_steps`, ohne `LOCAL_KEYS`; beide geändert: dieser Rechner;
  Erstsync: nur fehlende Schlüssel in beide Richtungen, bestehende Abweichungen als Konflikt).
  Solange Erstsync-Konflikte offen sind, keine Baseline schreiben (auch bei leerem Plan),
  sonst überschreibt der nächste 3-Wege-Sync die geschützten Werte.
  Dateien bei beiden geändert: die neuere; Falle: mtime allein sagt nichts, imkr speichert die
  Config auch ohne Änderung durch den User; `apply` prüft Pfade per `allowed`, sichert nach `.sync-backup/<zeit>/`, behält beim Config
  die eigenen `LOCAL_KEYS`, prüft mit `Config::parse`, setzt die mtime der Quelle). CLI
  `settings sync|export|import` (`cli::sync_settings_with`, drüben per `remote_output`), danach
  `Request::ReloadConfig` an alle Sessions. `hosts sync` gleicht nach den Upgrades nacheinander ab, mit Rückfrage
  wie `settings sync` (kein erzwungenes yes; ohne TTY keine Übertragung).
  Kontoliste (`accounts.toml` aus dem State, `ACCOUNTS`) wird vereinigt statt ersetzt
  (`plan`: Push nur wenn hier seit dem Sync geändert und drüben Namen fehlen, Pull nur wenn drüben
  Namen stehen, die hier fehlen — Listen dürfen dauerhaft verschieden sein; `apply` → `Store::merge_in`: fehlende anhängen samt Ordner,
  Default nur wenn keiner da); nie Logins. `default_of` zählt nur angemeldete Konten.
  Test: Fake-ssh, das nach `--` den Befehl mit HOME/IMKR_CONFIG der zweiten Welt ausführt.
- Rechner-Register (#9648): `src/machines.rs`. **Kein Absuchen des VPN** (Bastians Entscheidung:
  Kollegen-Rechner und fail2ban-Server dürfen keine abgelehnten Logins sehen) — Tailscale-Peers
  sind nur Vorschläge in `Ctrl+B s n`. **NetBird-Peers nirgends** (Bastian 10.11.: „Niemand darf
  alle NetBird-Rechner sehen, der sich imkr installiert“, das Netz ist mit Fremden geteilt): imkr
  ruft `netbird` gar nicht auf (Test `no_module_asks_netbird_for_its_peers`, E2E-Test
  `tests/no_netbird_peers.rs` mit Schein-netbird für `hosts vpn`). Eigene Rechner dort kommen über
  `hosts add NAME ADRESSE`, Register oder Kopplung und werden dann per NetBird-Adresse erreicht.
  Stattdessen `register.json`: `Entry` (Name =
  `machine_name` des Rechners, `id` = `machine_id`, Adresse, User, `at`, `removed`), Schlüssel
  `Entry::key` (id, vorher `name:NAME`). `merge` = Vereinigung, je Schlüssel gewinnt das neuere
  `at` (Umbenennung, neue IP); ein Eintrag nur mit Namen weicht dem mit id gleichen Namens (ist er
  neuer, übergibt er vorher removed/Adresse). `change_register` unter flock (`register.lock`).
  Reist beim minütlichen `ls --json` (`remote_sessions_asking(host, true)`): Fragender schickt sein
  Register als JSON in `HOSTS_ENV` + seine id in `CALLER_ENV` (`remote_command_with`, alte imkr
  ignorieren Umgebungsvariablen), Gefragter (`tell_hosts`) mischt, trägt die Adresse aus
  `SSH_CONNECTION` ein (`note_address`: VPN-Adresse ersetzt, andere füllen nur Lücken) und gibt
  `machine`/`machine_id`/`machines` auf dem ersten `SessionInfo` zurück (bei leerer Liste ein
  Stand-in); `learn_from` mischt beim Fragenden. Nie neue Flags/Requests. `note_self`: eigener
  Eintrag neu nur bei geändertem Namen/User — ein Löschvermerk für sich selbst bleibt stehen.
  `settle` (minütlich in `watch_places`, `found_at`): removed → `drop_host` (alle Namen hier, auch
  per `ids.json` gleiche id), umbenannt → neuer Name (nur `HostId::from_register`, von Hand
  hinzugefügte behalten ihren Namen), unbekannt → `reach` (ssh BatchMode + `ls --json`, je Eintrag
  höchstens alle `TRY_AGAIN` 10 min, `tried.json`) → `take` erst nach Antwort. `hosts add` →
  `register_add` (hebt den Vermerk auf), `hosts rm` → `register_remove` (Vermerk mit Zeit). Adresse
  ohne ssh-config: `host-routes.json` (`Route`), `hosts` bleibt eine Zeile je Name (alte Binaries);
  **jeder** ssh-Aufruf nimmt `machines::ssh_to(host)` statt `"--", host` (Name, wenn `ssh -G` ihn
  umbenennt, sonst Adresse + User + `-i ~/.ssh/imkr` — Auflösen allein reicht nicht, NetBird-DNS löst jeden Peer auf; `accept-new` + eigenes `machines/known_hosts` nur für
  100.64.0.0/10). Test: drei Welten, Fake-`ssh` nach Adresse 127.0.0.1–3 (setzt `SSH_CONNECTION`,
  `-G` gibt den Namen unverändert), Fake-`netbird` nur in Welt alpha, kurze `XDG_RUNTIME_DIR`,
  Server in allen drei Welten (ohne Server kein `settle`), Umbenennen per `name` in der Config.
- Kopplung per Code (#605, Task #9681): `src/pairing.rs`. `imkr hosts pair [--listen]` lauscht
  10 min nur auf VPN-Adresse/127.0.0.1 (`PORT` 4658, eigener `TcpListener`, `web::read_request`/
  `Response`), `hosts join ADR CODE` POSTet `/machine/join` (`contacts::post`) mit `Hello`
  (Name, id, User, eigener Schlüssel, Register, ssh-Host-Keys);
  beide Seiten → `machines::take_paired` (Register samt Schlüsseln, Eintrag neu = auch nach
  `rm`, Host-Keys nach `machines/known_hosts`, Host). Schlüssel reisen als `Entry::key`
  (`note_self` liest den eigenen; `merge` lässt einen Schlüssel nie durch einen neueren Eintrag
  ohne fallen — alte Versionen kennen das Feld nicht; gleich alter Eintrag füllt die Lücke).
  **Eigener Schlüssel je Rechner** (Nachtrag desktop:update): `pairing::key_file` =
  `~/.ssh/imkr-<machine-id, 12 Zeichen>`, immer von imkr selbst erzeugt (`ensure_key`, auch
  `imkr hosts key` für die Migration). `~/.ssh/imkr` war auf desktop/laptop/codingmachine
  **dieselbe** Datei (chezmoi) — Widerruf je Rechner unmöglich. Er wird nie angekündigt, nur
  noch nach dem eigenen angeboten (`pairing::offered_keys` in `by_address`; `by_name` gibt nur
  den eigenen dazu). Ein Schlüssel, den das Register bei >1 id zeigt, lässt keinen herein
  (`to_let_in` → Zeilen weg). **Nur gekoppelte Rechner nehmen Schlüssel** (Bastians
  Entscheidung): `pairing::paired()` = eigener Schlüssel vorhanden; sonst leert
  `change_register_noting` alle `key`-Felder vor dem Schreiben und ruft `sync_keys` nicht —
  ein Server, auf den man nur per Hand-Login kommt, fasst `authorized_keys` nie an (auch kein
  Backup). Test: Welt `server` in `tests/pairing.py`. **Wer Schlüssel weitergeben darf:**
  `tell_hosts` (gefragte Seite) behält sie — der Aufrufer ist gerade hereingekommen;
  `learn_from` (fragende Seite) streift sie ab — bis auf den **eigenen** Schlüssel des Antwortenden
  (ssh hat gerade seinen Host-Key geprüft), nicht bei einem hier entfernten. Sonst verteilt ein
  Rechner ohne laufende Session (kein Server, fragt nie) seinen Schlüssel nie (#1376, Welt `quiet`).
  `change_register` ruft unter der Register-Sperre
  `pairing::sync_keys`: `authorized_keys` bekommt für jeden Eintrag mit Schlüssel eine Zeile
  `restrict,pty,command="<imkr> gate <id>" KEY imkr:<id> NAME`, entfernte verlieren sie; nur
  Zeilen mit `imkr:` werden angefasst (`apply`, Test: fremde byte-gleich), Schlüssel schon in
  einer fremden Zeile → keine zweite (`by_hand`), Backup `authorized_keys.imkr-backup`, atomar,
  0600. Unit-Tests bekommen nie die echte Datei (`authorized_keys()` → None unter `cfg(test)`,
  sonst `IMKR_AUTHORIZED_KEYS`). `imkr gate ID` (`pairing::allowed`): nur
  `client::remote_command_with`-Form (per Neubau verglichen) oder `INSTALLED_CHECK`, ohne
  `hosts` außer add, `account`, `connect`, `web pair…`, `screen allow|refuse`; setzt
  `IMKR_GATED` → `tell_hosts` nimmt das als Aufrufer und lässt ihn seine eigene Entfernung nicht
  aufheben. Ehrlich: `send`/`run`/`new --` = Shell-gleich, die Beschränkung hält Shell, scp,
  Tunnel und Kopplung weiterer Rechner ab, ist keine Sandbox. Installieren per `hosts sync`
  braucht weiter den eigenen Login. `imkr hosts` zeigt „paired“/„key by hand“
  (`pairing::access`). E2E ohne sshd: Fake-ssh, das `authorized_keys` der Zielwelt wie sshd
  auswertet (erste Zeile mit dem Schlüssel, `command=` mit `SSH_ORIGINAL_COMMAND`), `cargo build &&
  python3 tests/pairing.py` (~5 min). Nach einer Kopplung versucht `settle` 15 min
  lang jede Minute (`machines/paired-at`, `TRY_AGAIN_PAIRED`): der erste Versuch des Neuen kommt
  vor seinem Schlüssel an, sonst wartete er 10 min. Falle: der Gekoppelte wird unter der
  **Absenderadresse** der Verbindung eingetragen — im Test landet jede Verbindung von 127.0.0.1,
  also die neue Welt auf 127.0.0.1 legen.
- Tailscale: `client::tailnet_peers` (`tailscale status --json`, MagicDNS-Name, nur linux/macOS),
  `Places::tailnet` (minütlich in `watch_places`), Vorschläge in `machine_choices` (Gruppe
  „Tailscale“), `imkr hosts vpn` (alias `tailnet`, mit NetBird, probiert nichts). Test: Fake-`tailscale` im Scratchpad-bin.
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
  Frage stellt, per `imkr new --window 2 -- <skript>` starten. Testwelt erbt sonst
  `CLAUDE_CONFIG_DIR` des echten Kontos (Skills!) → unsetzen; die zshrc stellt `~/.local/bin`
  vorn → im Schein-HOME dort die Attrappe ablegen, sonst startet das echte Claude Code.
  **Nicht für den Kontowechsel:** `new -- <cmd>` (außer in eine frisch gestartete Session, s.
  Keeper) legt das Pane ohne Shell an, `stop_foreground` gibt dann false (Vordergrund ==
  Pane-Prozess) und `switch_account` kann nicht wechseln. Dafür den Fake-Agenten mit vollem Pfad
  aus der Shell des Panes starten (`tmux send-keys`).
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
  fremde Rechner per `client::remote_output` mit Text auf stdin) → `Request::Message` →
  `App::post_mail` (`protocol::Mail` mit `seq`/`from`/`at` in `App::mailbox`, Notiz im
  `mail_log`), `deliver_mail` (je Tick) tippt bei idle/done/ohne Agent, bei working nur in
  Agents aus `agent::QUEUES_INPUT` (claude reiht ein, #8800), nie bei blocked, 3 s Abstand je Pane.
- Nichts in eine angefangene Zeile tippen (#9379): `Pane::note_typed` (aus `handle_key`, Paste
  und Tap-Eingabe) merkt per `pane::draft_after` eine begonnene, nicht abgeschickte Zeile
  (Enter/Esc/Ctrl+C/Ctrl+U beenden sie, `DRAFT_HOLDS` 30 min kappt eine liegengebliebene);
  `Pane::drafting` → freie Funktion `app::mail_hold` (Riegel vor jedem Agent-Zustand) →
  `mail_waits_for`/`mail_why`. Gehaltene Post bleibt im `mailbox` und steht als
  `MailWhat::Held` im Log.
- Zustellung ohne Tippen für Claude Code (#9379): die schon vorhandenen Hooks `Stop` und
  `UserPromptSubmit` (`cli::MAIL_EVENTS`) holen in `cli::status` die Post per
  `Request::TakeMail` ab und drucken `cli::hook_mail_json` — `Stop` als
  `{"decision":"block","reason":…}` (gemessen an Claude Code 2.1.238: der Agent geht damit im
  selben Lauf zurück in die Schleife und arbeitet sie ab), `UserPromptSubmit` als
  `hookSpecificOutput.additionalContext` neben dem Prompt. **Kein neuer Hook-Eintrag nötig**,
  alte Installationen können es nach `imkr update` sofort. `take_mail` setzt `Pane::takes_mail`
  (im Handover, gelöscht bei `Status { state: None }` = SessionEnd); `deliver_mail` tippt dann
  nicht mehr, solange der Agent `Working` ist (das `Stop` kommt sicher) — nur ein *idle* Agent
  feuert keinen Hook, der bekommt es nach `HOOK_WITHIN` (10 s) getippt. Exakt einmal, weil der
  `mailbox` die einzige Quelle ist. Codex holt seit #1358 genauso ab (siehe Claude-Hooks); pi
  schickt kein `hook_event_name` und bleibt beim getippten Weg.
- Enter-Kontrolle (#9379): `App::watch_enter` (aus `type_into` UND `deliver_mail`) merkt das
  Pane in `App::entered`; `check_entered` (je Tick) schickt Enter **einmal** nach, wenn der
  Agent die Eingabezeile nicht genommen hat (`App::took_input`), der Mensch das Pane nicht
  angefasst hat (`Pane::last_typed`) und das Textende noch auf dem Schirm steht
  (`pane::shows_text`, ohne Leerraum und Rahmenzeichen, weil die Eingabebox umbricht).
  Entscheidung als freie Funktion `app::enter_again`. Ursache war `imkr spawn`: das Briefing
  stand in der Eingabe, der Agent blieb done.
- **Beweis, dass eine getippte Zeile abgeschickt wurde** (#9379, v0.21.93): `Request::Status`
  trägt `event` (das `hook_event_name`), `UserPromptSubmit` zählt `Pane::prompts` hoch —
  das ist das einzige harte Signal. Der Bildschirm taugt nicht: ein Agent zeichnet seine
  Eingabebox so oder so neu, und nach dem Absenden steht der Text im Transkript und sieht von
  außen gleich aus. `took_input` nimmt für Panes mit Hook den Zähler, für die anderen
  `Entered::began` (gemeldetes Working/Blocked).
- **Nie auf einen unabgeschickten Text tippen** (#9379, v0.21.93, Fund der Biene event-gmbh):
  `App::unsent_typed` (Eintrag in `entered`, vom Agenten nicht genommen, Textende noch sichtbar)
  → zweites Argument von `mail_hold` → Nachricht wartet mit Grund. Vorher hängte `deliver_mail`
  die nächste Nachricht an die stehengebliebene an; das Enter schickte beide als EINEN Prompt
  mit dem Kopf der ALTEN Nachricht vorn — der Agent las dieselbe Nachricht erneut, die neue war
  begraben (gemessen: 2193 → 3574 → 4374 Zeichen in einem Prompt). Der Eintrag in `entered`
  bleibt deshalb liegen, auch wenn die Nachschick-Versuche verbraucht sind, bis der Agent sie
  nimmt oder der Text vom Schirm ist.
- **Codex-Verlauf ist keine Eingabezeile** (#1339): `app::codex_composer` sucht den
  Composer am Terminal-Cursor vor `? for shortcuts`; der leere Vorschlag ist dim
  (am echten TUI lesend gemessen). `unsent_typed` prüft hier den Composer selbst,
  auch ohne `entered` nach einem Server-Handover: echte Drafts und eingeklappte
  Pastes halten weiter, unbekannte Layouts/Approval ebenso. Die bisherige Suche
  nach den letzten 40 Zeichen in acht nichtleeren Bildschirmzeilen traf bereits
  beantwortete Nachrichten im Verlauf; ohne Hooks wird `entered.began` nie gesetzt.
  Den geratenen Working-Zustand als Beweis zu nehmen wäre ebenso falsch: bereits
  das Echo einer noch ungesendeten Eingabe erzeugt Ausgabe. Für Codex keine
  automatischen Enter-Wiederholungen und beim Zustellen den frischen Pane-Zustand
  statt des vorherigen Idle-Ticks nehmen. Leerer Composer gibt die Queue frei,
  ist aber **kein** `Confirmed`-Nachweis. Repro ohne Konto/Endpoint:
  `cargo build && python tests/mail_stall.py`; Fake-Codex, eigener HOME/Socket/Config,
  schnelle Antwort, Queue, Draft/Paste/Approval, Working→Idle, verschlucktes Enter,
  Server-Handover und Prozess-Neustart. `IMKR_HOSTNAME=mail-regression-host cargo test`
  vermeidet die bestehende Annahme eines anderen Tests, dass `laptop` fremd ist.
- **Getippt ist nicht angekommen** (Bug #1307): `Pane::prompts` allein ist faelschbar — JEDER
  fremde Prompt treibt den Zähler hoch. Gemessener Fall: imkr tippte 12:18:33, 12:18:38 wurde
  der Agent von einem Kontowechsel **neu gestartet** (die Eingabezeile geht mit dem Prozess),
  und Claude Code kam mit seinem Resume-Prompt zurück (`isMeta` „Continue from where you left
  off." + synthetische 0-Token-Antwort). Der Neustart hat den Text verloren, der Zähler hat es
  verborgen. Dieselbe Wurzel wie #1314: `deliver_mail` weiß nichts von `switching`. Deshalb trägt `Request::Status` jetzt auch `prompt` (Feldname aus dem Binary 2.1.238
  gelesen: `hook_event_name:"UserPromptSubmit",prompt:r`; gemessen gibt es den **ganzen** Text,
  4094 Zeichen, kein `[Pasted text]`-Platzhalter) → `Pane::note_prompt`/`submitted`
  (`PROMPTS_KEPT` 8) → freie Funktion `app::prompt_took` (Prompt nach dem Tippen **und** Inhalt
  passt) → `took_input`. Bestätigt = `MailWhat::Confirmed`; `Typed` heißt nur noch „geschrieben",
  die Zeile in `imkr mail` hat kein ✓ mehr und `imkr msg` sagt „typed into pane N, not confirmed
  yet" statt „delivered". `imkr msg --confirm` wartet auf den Beweis und endet mit Exit != 0
  (`cli::confirm_arrival`, `CONFIRM_WITHIN`). `check_entered` lässt einen Eintrag nicht mehr
  fallen, nur weil der Text nicht auf dem Schirm steht (`shows_tail`): genau die langen
  Nachrichten, die Entscheidungen tragen, faltet Claude Code zu `[Pasted text #1 +N lines]`.
- **Nicht angekommen wird gesagt, nicht nachgetippt** (Bug #1307): `MAIL_OVERDUE` (600 s) →
  `App::say_overdue` (Vermerk `MailWhat::Overdue` mit Grund, Flash, `notify(…, "blocked", …)`).
  Nie eine Nachricht — die stünde hinter der Nachricht, um die es geht. Die 600 s sind
  gerechnet: gesunde Hook-Zustellungen dauerten gemessen 393 s und 365 s, fünf Minuten hätten
  also beim Normalfall Alarm geschlagen (Test `saying_a_message_is_overdue_waits_longer_than_a_healthy_turn`).
- **Der Absender wird benachrichtigt, auch über Rechner** (Bug #1307): `App::tell_sender`
  (aus `say_overdue`, im Thread) → freie Funktion `app::sender_place` (Adresse → Rechner,
  Session, Pane; `None` für „the user at X", `review`, `notes` — nicht jeder Absender ist
  ein Pane) → lokal `Request::Overdue` an den Session-Socket, fremd per ssh `imkr overdue
  PANE --to ADR --why TEXT` (`cli::overdue`, in `completions::INTERNAL`, Text auf stdin).
  Scheitert der Weg, steht der **lokale** Vermerk trotzdem — den liest `imkr mail ADRESSE`
  und `--confirm`. `mail_line` zeigt beide Seiten verschieden (`sent` = „your message …
  has NOT arrived").
- **Abgeholte Post übersteht ein absichtliches Beenden** (Bug #1313): `take_mail` nimmt die
  Nachricht aus dem `mailbox` (das ist „exactly once") und legt eine Kopie in
  `Pane::handed` (im Handover, anders als `takes_mail`: verlieren wäre ein Verlust,
  mitnehmen höchstens eine Doppelzustellung — und die wird gesagt). Freie Funktion
  `app::handed_done`: **jeder** gemeldete Zustand löscht die Kopie, weil der Hook-Aufruf
  seinen eigenen `Request::Status` VOR `TakeMail` schickt und der nächste damit ein
  späterer ist. `Working` allein geht nicht — nach `Stop` mit `decision: block` arbeitet
  der Agent im selben Lauf weiter und meldet `working` nur bei einem Werkzeug; wer nur mit
  Text antwortet, meldet es nie, und die Kopie läge bis zum nächsten SIGTERM herum.
  `App::give_back_mail` (aus `switch_account` nach `stop_foreground`, und bei
  `Status { state: None }` = SessionEnd) legt sie vorn in den `mailbox` zurück, mit
  `MailWhat::Again` + Grund und Flash — **nie stumm**: ein Keeper, der dieselbe Nachricht
  zweimal bei einer Biene sieht, muss erkennen können, dass imkr sie nochmal geschickt hat
  und nicht er. Gemessen in der Testwelt: zwei per Hook geholte Nachrichten, SessionEnd
  davor → beide zurück und in Reihenfolge; Gegenprobe mit einer Zustandsmeldung nach der
  Übergabe → 2 taken, 0 again.
- **Nichts in ein Pane tippen, dessen Agent gerade neu startet** (Bug #1314):
  `restarting` ist das erste Argument von `mail_hold` und schlägt alles andere →
  „its agent is restarting in another account". Vorher galt ein Pane ohne Agent als
  zustellbar, und genau das ist es zwischen `stop_foreground()` und dem Neustart: die
  nackte Shell steht vorn. Die Nachricht bleibt im `mailbox`, der Hook des neuen Agenten
  holt sie, und nach `MAIL_OVERDUE` wird gemeldet — gewartet wird benannt, nicht stumm.
  **Beide Hälften des Fensters**: `switching.contains_key(&id) || resumed.contains_key(&id)`.
  `finish_switches` nimmt das Pane ERST aus `switching` und tippt DANN den
  `imkr account run`-Befehl (`resumed.insert` danach) — die zweite Hälfte ist die, in der
  die Shell wirklich vorn steht, und nur `switching` zu prüfen lässt sie offen (Fund der
  Biene konto-wechsel; `resumed` kam mit ihrem `22b1a42` auf main und ist von einem Zweig
  ab `e9e6f0e` aus unsichtbar). `check_resumed` entfernt den Eintrag, sobald der Agent
  antwortet — damit endet der Halt von selbst.
  **Gemessen, weil die Schwere davon abhängt:** eine über `imkr msg` getippte Zeile
  trägt den Kopf `[imkr message from …]` und `Request::Message` faltet allen Leerraum
  (auch Zeilenumbrüche) zu einfachen Leerzeichen — die Zeile hat also `[imkr` als
  Befehlsnamen, zsh sagt `bad pattern: [imkr`, der eingebettete Befehl läuft NICHT.
  Kopflos sind nur `review` und `notes` (die zwei anderen `post_mail`-Aufrufer); dort
  ist das erste Wort der Notiz der Befehl und läuft (gemessen: `echo IMKR-1314-MARKER`
  wurde ausgeführt). Verloren ist die Nachricht in beiden Fällen — die Shell frisst sie.
- **Mehrere Nachrichten für ein Pane gehen denselben Weg** (Bug #1307): freie Funktion
  `app::waits_for_hook(fetches, busy, more, age)`; `more` aus `pending` in `deliver_mail`. Eine
  getippt und der Rest per Hook verdrehte die Reihenfolge (gemessen: die späteren zwei kamen im
  `Stop`-Hook, während die erste noch in der Eingabezeile stand). Zwei Wege gleichzeitig können
  keine Reihenfolge halten. Der getippte Rückfall bleibt **nur** für das Pane ohne eigenen Hook
  (codex ohne freigegebene Hooks, pi, Shell, claude direkt nach SessionEnd) und für den einzelnen, wirklich geparkten
  Agenten — ihn ganz wegzunehmen hieße, dass ein codex nie wieder Post bekommt.
- Getippter Rückfall erst bei echter Ruhe (#9379, v0.21.93): `QUIET_BEFORE_TYPING` (5 s ohne
  Ausgabe) zusätzlich zu `HOOK_WITHIN`. Gemeldetes idle allein genügt nicht — zwischen zwei
  Zügen meldet ein Agent kurz idle, während er mitten in der Arbeit ist, und genau dann traf
  der Rückfall die noch nicht geleerte Eingabezeile.
- Nachrichten-Pane (#9379): `imkr mail [--follow] [--json] [ADRESSE]` (`cli::mail`,
  `mail_asker` lokal per `Request::Mail` oder per ssh mit 3 s Takt, `mail_lines` zeigt neue
  Notizen über `seq` und die Warteliste nur bei Änderung). `imkr keeper` legt es per
  `keeper::mail_pane` schmal rechts neben den Agenten (`Request::New.ratio` →
  `Workspace::split_at`), Pane heißt `mail`, Fokus geht zurück zum Agenten.
  `imkr agents [--all]`, Skill-Text in `skills/imkr/SKILL.md` (`imkr skill [--install]`).
  Test-Falle: `imkr status` liest stdin (Hook-JSON) – in Heredoc-Tests `</dev/null` anhängen.
  Ausgang: scheitert `msg` per ssh an einem Rechner, `client::queue_outbox` (`<state>/outbox`,
  24 h); der Zielrechner holt im Host-Polling (`watch_places`) per `fetch_outbox` → ssh
  `imkr outbox take NAME` (`take_outbox`, Datei erst umbenannt, dann gelesen) ab und stellt per
  `Request::Message` zu. Server brauchen so kein ssh zurück. `read` nimmt Adressen wie `msg`.
- Kontakte (andere Personen, #495): `src/contacts.rs`, `imkr connect` (`cli::connect`). Kopplung
  per Code (`invite` → `<state>/contact-invite.json`, 10 min, 5 Versuche) über den Web-Server
  (`web` muss an sein, Adresse nicht loopback): `join` POSTet `/contact/join` mit eigenem Token,
  Antwort ist der Token der anderen Seite; `contacts.json` (0600) hält je Kontakt `token` (wir
  senden) und `key` (sie senden). `msg NAME:session/pane` geht bei einem Kontakt per
  `/contact/message` (Bearer, HTTP von Hand in `contacts::post`), nie per ssh. Der Empfänger legt
  sie nach `contact-inbox.json` (`waiting`, Flash an alle Sessions), `trusted` stellt direkt zu;
  `answer_address` macht aus der Absender-Maschine den Kontaktnamen. Die Endpunkte lehnen
  Anfragen mit `Origin` oder ohne `Content-Type: application/json` ab (Browser). Dateizugriffe
  unter `flock` (`locked`). Test: zwei Welten mit eigenem `web_listen` auf der VPN-Adresse,
  andere Ports; laufende Web-Server halten das Binary fest (zweite Kopie nehmen).
  Weg über AI Brain: `Contact::brain` (AI-Brain-Name), imkr meldet sich nicht selbst an, sondern
  ruft `ai-brain-harness nachricht …` (`brain_cli`, `brain_program` fällt auf den alten Namen `ai-brain-cli` zurück; Vertrag mit der Harness-Session):
  `kontakte --json` → `{ich, kontakte:[{person,email,rechner}]}` (Name oder E-Mail exakt, an
  sich selbst abgelehnt), `senden PERSON TEXT --von A --ziel S/P`, `abholen --json` →
  `{nachrichten:[{id,von_person,von,ziel,text,abholungen}]}` (beim ersten Abholen EINEM Rechner
  zugeteilt), `bestaetigen ID` (nur der abholende Rechner). `Contact::brain` = `person`. `fetch_brain` je `BRAIN_EVERY` in `watch_places`, nur mit
  so einem Kontakt; Eingang über `take_in` wie beim NetBird-Weg. Zustellung mindestens einmal:
  AI Brain gibt Unbestätigtes nach 5 min erneut aus, `fetch_brain` bestätigt jede id
  (`bestaetigen`) und überspringt schon gesehene (`contact-brain-seen.json`, `BRAIN_SEEN`);
  `take_in` nie unter `locked` aufrufen (flock je fd, verschachtelt = Verklemmung). Je Rechner
  holt nur ein Server gleichzeitig ab (`contact-brain.lock`, LOCK_NB): zwei Sessions holten
  dieselbe unbestätigte Nachricht und reihten sie doppelt ein (Echttest 14.10.). Text max.
  8 KB (`MAX_BRAIN_TEXT`). `von_person` ist von AI Brain geprüft, `von` nur Angabe. Test: Attrappe
  `ai-brain-harness` im PATH beider Welten mit gemeinsamem Ordner als Briefkasten.
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
  `None` ohne Agents), eigene Session `App::quiet_for`; `Ctrl+B S` zeigt „✓1 2h ago“, alte sind
  standardmäßig ausgeblendet, `o` zeigt sie (`hide_old`, `is_old_session`: nur nicht laufende; laufende bleiben
  immer sichtbar wie in der Sidebar, egal wie lange ihre Agents ruhen, #7921).
  Falle: `remote_sessions` meldet für einen Rechner ohne Session eine erfundene `default`
  (`SessionInfo::is_stand_in`: 0 Panes) – die Sidebar zeigt dafür „no session yet“, `Ctrl+B S`
  statt des Namens `NEW_SESSION_ENTRY` („+ new session…“, #1362), Enter fragt per
  `ask_session_name(host)` nach dem Namen, statt eine `default` zu starten; nie als alt
  ausgeblendet. Welche Rechner so einen Eintrag bekommen, sagt **eine** Liste,
  `App::empty_hosts` → freie Funktion `empty_hosts` (mit Grund): antwortet ohne laufende Session
  (`SessionInfo::runs`; nur geschlossene zählen nicht — so verschwanden desktop und pimarchy aus
  `Ctrl+B S`, weil ruhende als alt ausgeblendet sind), antwortet nicht (`short_problem`), steht
  nur im Register (`Places::unreached` aus `machines::settle`, Wahl → `reach_registered` merkt
  die Register-Adresse). `Ctrl+B S`, Enter darauf und `machine_choices` (`Ctrl+B s n`, alle
  Hosts, auch nicht antwortende) lesen sie. Die Auswahl baut der Server, an dem der Client hängt:
  E2E mit Client-Welt ≠ Server-Welt (Schein-ssh nach Zielname). Damit die Zeit Updates und
  Neustarts übersteht: `SavedPane::idle_since` → `App::carried` (gilt bis zum ersten Idle, höchstens
  `CARRIED_FOR`), übernommene Panes setzen `last_output` 60 s zurück (sonst kurz „working“), und
  `Handover::unseen` hält ✦ done über ein Update.
- Recap: `App::activity` (Zustandswechsel aus `update_agents`, max. 500), `away_since` (gesetzt in
  `take_client`, wenn der letzte Client geht), `attach` öffnet `Mode::Recap` nach ≥ 60 s mit
  Aktivität, `Ctrl+B s l` (`open_recap`) später. Test: abhängen, Fake-Agent starten, 66 s warten.
- Notizzettel `Ctrl+B t` (Idee #504): `Pane::notes` (`persist::Note`, `sent`), gespeichert in
  `SavedPane::notes` (Neustart, `last`/revive, Handover über `saved`), `MovedPane::notes` beim
  Verschieben. `App::notepad` (`Notepad`: Pane, Auswahl, Eingabe), `Mode::Notes`, `notes_key`
  (Enter auf gewählter Notiz → `mailbox`, `y` OSC 52, `e` zurück in die Zeile, `x` weg),
  `draw_notes` je Einstellung `notes_at` (`NotesAt`: rechts volle Höhe, Mitte, unten; Höhe nach
  Umbruch), Rahmen `✎N` (offene). CLI `imkr note` → `Request::Note`.
- Wo der User sitzt: `Request::Operator` (Client nötig; `newest_origin` → `machine`/`via`, ohne
  Origin = lokaler Client → `here`), CLI `imkr operator [--json]` fragt die eigene Session, dann
  die übrigen des Rechners. Für Agents, die etwas auf dem Bildschirm des Users zeigen wollen
  (`agent-tunnel` aus chezmoi/AI-Brain-Harness).
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
  (`Request::Layout`, `Ctrl+B w l` → `Mode::Layouts`). `Ctrl+B w x` → `Action::CloseWorkspace` → `App::close_workspace` (alle Panes per `close`, Rückfrage). main-* nicht angleichen (60 % Hauptpane).
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
  neu gebaut, Line-Style ginge verloren). Start mit Enter, ↑↓ nur wo im Menü unbelegt; ←→ =
  `pick_beside` über `pick_columns`, Space/Enter führt aus, ⌫ zurück.
- Tastenbelegung nach dem Prefix steht **nur** in `src/bindings.rs` (`BINDINGS`). Which-Key-Menü,
  Hilfe-Overlay und `imkr keys` lesen daraus, neue Befehle dort eintragen, nicht in `app.rs`.
- Einstellungen: `src/config.rs` (serde/TOML, `SETTINGS`-Tabelle fürs Settings-Menü). Neue
  Einstellung = Feld in `Config` + Eintrag in `SETTINGS`, das Menü liest nur die Tabelle.
- Manuell testen ohne die eigene Session zu stören: `IMKR_SOCKET=/tmp/x.sock` und `IMKR_CONFIG=/tmp/x.toml`
  setzen (sonst wird die echte Config überschrieben), headless in tmux starten (`tmux new-session -d ...`, `send-keys`, `capture-pane -p`).

## Test-Abschirmung (App/PTY)

- **Vor jedem App-/PTY-Start** muss jede Testconfig `notifications = "off"` enthalten.
  Zusätzlich einen eigenen Fake `notify-send` ganz vorn in den Test-PATH setzen und
  Session-/System-DBus auf nicht existierende testlokale Sockets isolieren. Geerbte
  `DBUS_*`, `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY` entfernen; privates
  `XDG_RUNTIME_DIR` plus eigene HOME/Config/State/Socket-Welt verwenden. Den Guard
  speichern und Fake-Auflösung prüfen, bevor irgendein App-Prozess startet.
- Gilt auch für `tests/mail_stall.py`, `tests/handoff.py`, Demo-/manuelle PTY-Proben
  und externe Kopien alter Review-Harnesses. Keine ungeschützten Archivskripte starten;
  keine echten Desktop-Bestätigungen bedienen. Notification-Tests verwenden nur den
  Fake, nie den echten Desktopclient. Historische Testresultate nicht rückwirkend als
  nebenwirkungsfrei darstellen: frühere Popup-Leaks waren fehlende Testabschirmung.
- Vom Keeper beendete temporäre Proben gelten als erwarteter Abbruch, nicht als
  Produktregression oder Anlass für einen ungeschützten Wiederholungslauf. Originale
  unangetastet lassen. Bei neuen Desktop-Nebenwirkungen sofort alle eigenen Proben
  stoppen, zuerst den Guard reparieren; keine weiteren Featuretests davor.

## Befehle

```bash
cargo run            # starten, Ctrl+B q Quit-Menü, Ctrl+B d detacht, `cargo run -- kill-server` beendet
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo test
```

## Konventionen

- Doku: README ist die Startseite (kurz, Pitch, Install, erste Schritte, Links). Quellen in
  `docs/src/` (Markdown, `index.html` als Übersicht, `style.css`), daraus baut `python3 docs/build.py`
  die Website `docs/*.html` (Seiten eigenständig, Mermaid als SVG hell/dunkel, Cache in
  `target/docs-diagrams`). Neue Funktionen in `docs/src/` beschreiben, danach bauen und beides
  committen. `docs/src/keys.md` und `commands.md` kommen aus `BINDINGS`/`USAGE` (Test schlägt bei
  Abweichung an): `IMKR_WRITE_DOCS=1 cargo test docs_`. Mermaid-Labels mit `--` quoten.
  Kopie im Vault von Bastian: `topics/programmierung/projekte/imkr/doku/` (HTML, `markdown/`).
  Demo-GIF: `vhs docs/demo/demo.tape` (eigene Testwelt aus `docs/demo/setup.sh`: Runtime/State/
  Config, schlichte bash, damit nicht das echte claude startet; Schein-Agent `docs/demo/bin/claude`).
- Lizenz: MIT OR Apache-2.0
- Code und Kommentare auf Englisch
