# Install and update

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

## Tab completion

Tab completes imkr's commands and options, and the names that exist right now: sessions, other
machines, hives, templates, accounts, plugins, layouts, contacts, and agent addresses for `msg`.
One line in the shell's rc file:

```bash
eval "$(imkr completions zsh)"    # ~/.zshrc, after oh-my-zsh or compinit
eval "$(imkr completions bash)"   # ~/.bashrc
```

The names are asked for when Tab is pressed (`imkr complete sessions` and so on), so a session
started a second ago is there.

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

## Build

```bash
cargo build --release
cargo install --path . --locked   # put this checkout on PATH as `imkr`
cargo clippy --all-targets -- -D warnings
cargo test
```

A tag `vX.Y.Z` matching the version in `Cargo.toml` builds the release binaries and attaches
them to its GitHub release, which is where `install.sh` downloads from.
