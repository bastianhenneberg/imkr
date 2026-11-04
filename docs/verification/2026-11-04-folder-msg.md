# Session directories and dormant message destinations (#1334, #1294)

Base: a86e823. No deployment or push; no real agent/account was started.

## Changes

- `restore_sessions` prepares layout directories before saving or starting the session. This is the common destination-side path for templates, TUI sessions and remote hive starts. Tilde expansion happens on that destination.
- `start_server_in` prepares an explicit initial directory; CLI `new --dir` expands a quoted tilde consistently for subsequent pane creation too.
- `Pane::spawn_after` rejects invalid explicit directories instead of silently using the server CWD. It does not create directories for ordinary splits/new panes.
- TUI session-directory submission preserves `/`.
- Socket failures name the target session; persisted sessions show stopped/closed, local time, and `imkr revive NAME`.
- A non-255 SSH command failure is returned to the sender. Only transport failures retain the outbox fallback.

## Verification

- `cargo fmt`; `git diff --check`.
- `cargo clippy --all-targets -- -D warnings` passed.
- `cargo test --quiet` with isolated HOME/config/state/runtime and `IMKR_HOSTNAME=probe`: 309 passed, one ignored.
- Real isolated CLI servers: nested creation, quoted tilde creation, actual pane CWD, ordinary missing pane directory rejected without creation, file-as-parent creation failure, template tilde creation, deliberately closed and SIGKILL-stopped persisted sessions each reporting the correct state and revive command.
- Real TUI in a private tmux server: Ctrl+B s n, name, empty template, directory prompt; `~/from-tui/deep` creates and starts there. `/` starts in `/`. File-as-parent shows the target session/path error without starting the requested session. Captured screens and CLI JSON confirm these results.
- Local fake SSH executes the actual CLI with a second isolated environment: destination-only tilde directory creation via `hive start`, remote closed-session error includes remote target and revive hint and is not queued; exit 255 still queues.

## Evidence and limits

Local evidence (ephemeral `/tmp`, available to the Keeper on this machine):

- `/tmp/imkr-folder-msg-probe.py` and `/tmp/imkr-fm-probe5/commands.json`.
- `/tmp/imkr-fm-probe5/tui-name.txt`, `tui-dir.txt`, `tui-result.txt`, `tuiroot.txt`, `tuifail.txt`.
- `/tmp/imkr-folder-msg-remote.py`, `/tmp/imkr-fm-remote/commands.json`.
- `/tmp/imkr-folder-msg-isolated-tests.log`, `/tmp/imkr-folder-msg-clippy.log`, `/tmp/imkr-folder-msg-docs.log`.

All test HOME/config/state/runtime/socket paths are under `/tmp`; IMKR/agent variables are removed before constructing each environment. Only private test sessions are killed. Earlier probe directories `imkr-fm-probe` through `imkr-fm-probe4` retain intermediate attempts; `probe5` is the final successful CLI/TUI run. The remote probe uses the persisted stopped-layout fixture from `probe4`.

Initial tests inside the sandbox could not bind Unix sockets or write inherited real state paths. After isolation and running socket tests outside the sandbox, one existing test still assumed this machine was not named `laptop`; setting the isolated hostname made all tests pass. No unrelated source changes were made for these failures. Remote verification used a local SSH stand-in, not a physical second machine. The TUI's existing single-line flash can truncate long errors to the terminal width; full CLI errors retain the OS cause. Creating multiple layout directories is not transactional: earlier created directories remain if a later directory fails.
