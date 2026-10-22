# On the phone

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

## Push to the phone

The page shows what waits only while it is open. To hear of it with the phone in your pocket,
each machine's imkr can push through [ntfy](https://ntfy.sh) (its app on Android or iPhone,
subscribed to a topic only you know) and/or to a webhook of your own:

```toml
[push]
ntfy = "https://ntfy.sh/a-topic-only-you-know"   # or your own ntfy server
webhook = "https://example.org/imkr"             # gets each event as JSON
on = ["blocked", "failed", "account"]            # the default; "done" too if you like
quiet = 60     # nothing while you typed in that session in the last 60 seconds
link = "http://100.97.56.232:4657"               # what a tap opens; else imkr web's address
```

A push names machine, session and agent (`codingmachine · php: claude needs you`), what the
agent works on and, for a question or an error, the bottom of its screen. Each machine pushes
for its own agents, so the setting belongs in every machine's config (`imkr hosts sync` takes it
along). The same pane pushes the same thing at most every two minutes. `imkr push test` sends
a test and says whether it went out.
