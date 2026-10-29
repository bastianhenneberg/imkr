# Seeing and driving the screen

An agent in a pane can read what you type at it and what its program prints. It cannot see
your desktop. `imkr screen` changes that, deliberately and visibly: screenshots of a screen,
and the pointer and keyboard on it — on this machine or on another one of yours.

This is off everywhere until you turn it on, and on a machine you sit at imkr asks you first.

```bash
imkr screen status                     # may anything happen here at all?
imkr screen shot                       # a PNG, scaled down, with how to read coordinates off it
imkr screen click 1200 640             # logical coordinates, as the shot reports them
imkr screen type "git status"
imkr screen key Return
imkr --host desktop screen shot        # the other machine's screen, through the usual ssh way
```

Agents reach the same things as MCP tools (`imkr_screen_status`, `imkr_screen_shot`,
`imkr_screen_click`, `imkr_screen_move`, `imkr_screen_type`, `imkr_screen_key`), and a
screenshot comes back as a picture, not as a path.

## Start with status

`imkr screen status` is the pre-flight check, and it is worth running before anything else —
it is the one command that works even where nothing else would:

```text
machine   omarchy
backend   hyprland
setting   computer_use = ask · allowed for 27 more minutes to claude@omarchy:web/2
screen    HDMI-A-1 3072x2048 logical at 0,0 · scale 1.25 · on
screen    AGENT 2560x1440 logical at 3072,0 · scale 1 · on ← used without --monitor
tool      grim ✓
tool      ydotool ✓
tool      wtype ✓
note      ydotoold running for 25h
ready     yes
```

It answers four different questions that all look alike from outside:

* **Is the session locked?** Then nothing happens. imkr does not unlock a session, does not
  drive a lock screen and does not fill in a password — ever, whatever the setting says. A
  lock whose client has died leaves the session locked with nobody to unlock it; imkr reads
  that off the compositor and refuses just the same.
* **Are the screens on?** A screen in power saving gives black pictures. All of them off is
  a refusal, one of them off is a note.
* **Are the tools there?** `grim` for pictures, `ydotool` for clicks, `wtype` for text.
* **May this agent?** See below.

## Coordinates

Everything is in **logical** pixels, the ones the compositor counts in — not device pixels.
A 3840×2560 screen at scale 1.25 is 3072×2048 logical, and the screen beside it starts at
3072. Mixing the two puts every click on the wrong screen, so imkr does the division once
and speaks logical everywhere.

A screenshot is scaled down to something an agent can look at and says how to get back:

```json
{
  "image": { "width": 1280, "height": 720 },
  "scale": 0.5,
  "origin": { "x": 3072, "y": 0 },
  "hint": "click at x = 3072 + image_x / 0.5000, y = 0 + image_y / 0.5000",
  "covers": { "from": { "x": 3072, "y": 0 }, "to": { "x": 5632, "y": 1440 } }
}
```

With `--monitor NAME` on `click` and `move`, the coordinates count from that screen's own
corner instead — which is what you read straight off a screenshot of that one screen.

**Small text does not survive the scaling.** A whole 3072-wide screen at 1280 across is 42 %,
and a terminal font is a blur. `--region X,Y,WxH` takes a piece at full size instead:

```bash
imkr screen shot --region 3072,0,900x300 --width 900     # readable, and a tenth the bytes
```

## Which screen

Without `--monitor` imkr takes the screen named in the settings, and otherwise the focused
one. `--monitor all` takes the whole layout at once.

A headless screen is worth knowing about: a compositor can make one that no cable leads to,
and an agent can then work on it while the real screen is off and nobody is disturbed.

```bash
hyprctl output create headless AGENT      # a screen for the agent, beside the real one
```

Then set `Screen for agents` in the settings (`Ctrl+B ,`) or `computer_use_monitor = "AGENT"`.
If that screen is not there when a command runs, imkr says so and does nothing — it does not
quietly take the real one instead.

## Who may, and for how long

One setting per machine, `computer_use`, off by default:

| | |
|---|---|
| `off` | nobody sees or drives this screen. Commands say so and name the setting. |
| `ask` | imkr asks **here**, visibly, before the first action. A yes holds for 30 minutes. |
| `on` | always allowed — for a machine nobody sits at. |

With `ask`, the agent's command waits while imkr puts the question in front of whoever is
attached to a session on that machine, with a desktop notification as well:

```text
╭ ⚠ Let an agent use this screen ───────────────────────────────╮
│  claude@omarchy:web/2                                         │
│  wants to see this screen and drive it: move the pointer,     │
│  click and type.                                              │
│  Allowing it holds for 30 minutes, for this machine only.     │
│  Every action goes into screen.log.                           │
│   y  yes    any other key cancel                              │
╰───────────────────────────────────────────────────────────────╯
```

`y` allows it, any other key refuses — and the refusal reaches the waiting agent at once,
rather than leaving it to time out. Without an answer nothing happens at all. In a terminal
without imkr, `imkr screen allow` and `imkr screen refuse` are the same two answers.

While a permission holds, the control bar says so, and says it louder while something is
actually happening:

```text
 ⬢ imkr   1  + │ 1 pane · ^B menu · ⦿ screen 29m          ▸ ~/code · ⌂ omarchy
 ⬢ imkr   1  + │ 1 pane · ^B menu · ⦿ screen click        ▸ ~/code · ⌂ omarchy
```

Every action is appended to `~/.local/state/imkr/screen.log`, and `imkr screen log` prints the
last lines:

```text
2026-10-29 14:01:44 claude@omarchy:web/2 asked drive
2026-10-29 14:01:51 claude@omarchy:web/2 allowed 30 minutes
2026-10-29 14:01:51 claude@omarchy:web/2 shot AGENT 1280x720
2026-10-29 14:01:53 claude@omarchy:web/2 click left at 4352,700
2026-10-29 14:01:54 claude@omarchy:web/2 type 41 characters
```

The log says *how much* was typed, never *what*. Text goes into whatever has the keyboard
focus, and imkr cannot tell what that is — so it does not write it down, and you should not
type anything secret through it either.

`computer_use` and `computer_use_monitor` are **local settings**: `imkr settings sync` leaves
them alone in both directions. A machine you sit at keeps its own answer, however the machine
that nobody sits at is set up.

### What this is and is not

The permission is a control, not a wall. An agent that can run commands on a machine can do
anything that machine's user can do, including writing imkr's own files. What `computer_use`
buys you is that imkr itself does nothing until you say so, that you see it while it lasts,
and that there is a record afterwards. If you do not want an agent on a machine at all, do
not give it a shell there.

## Where the pictures go

Without `--save`, a screenshot is written where imkr keeps its working files and the path is
printed; an agent gets the picture itself and needs nothing more.

A **person** needs the file somewhere they can open it, and that is usually not the machine
the screenshot came from. `--save` puts it in the shared folder:

```bash
imkr screen shot --save
# saved to /home/you/codingshare/screenshots/shot-1793278960.png
# on laptop: /Users/you/codingshare/screenshots/shot-1793278960.png
```

See [the shared folder](settings.md#the-shared-folder) for setting that up. Without one,
`--save` says so and the plain form keeps working.

## Other systems

Only Linux under **Hyprland** is supported so far. macOS and Windows are a gap, not a
promise:

| | |
|---|---|
| macOS | screenshots through ScreenCaptureKit (the `xcap` crate), input through CGEvent (the `enigo` crate), both behind the Screen Recording and Accessibility permissions in System Settings |
| Windows | screenshots through the Desktop Duplication API (`xcap`), input through SendInput (`enigo`) |

`imkr screen status` prints exactly that where there is no backend, instead of a blank no.
Everything above the backend — the commands, the coordinates, the permission, the log — is
written to survive another one being added: one trait per ability (`Sees`, `Points`, `Types`,
`Checks`), and the commands never learn which one answered.

## Under the hood on Linux

* **Hyprland's own socket**, not `hyprctl`: inside a pane or over ssh neither
  `HYPRLAND_INSTANCE_SIGNATURE` nor a PATH with `hyprctl` is given. imkr finds the running
  instance in `$XDG_RUNTIME_DIR/hypr` itself, and hands the helper tools the session's
  `WAYLAND_DISPLAY` the same way.
* **`grim -s`** counts in logical pixels, which is what makes reading a coordinate off a
  screenshot one division.
* **The pointer moves through the compositor**, which is exact and absolute. `ydotool`'s
  own absolute mode saturates at the right edge and needs its factor measured; only the
  click itself goes through `ydotool`, because a button press has nowhere else to go.
* **Text goes through `wtype`**, which brings its own keymap, so umlauts and anything else
  beyond ASCII arrive whatever keyboard layout is active. It is handed `LC_ALL=C.UTF-8`:
  under a `POSIX` locale it drops everything from the first non-ASCII character *without
  saying so*, and imkr refuses to type rather than let that happen.
* **The text never appears in a command line**, here or over ssh — it goes in on standard
  input.

If clicks stop arriving while the pointer still moves, the input daemon has drifted; it has
been seen to do that after weeks of uptime. `imkr screen status` says how long it has been
running, and `systemctl --user restart ydotool` fixes it.
