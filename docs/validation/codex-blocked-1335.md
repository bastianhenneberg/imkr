# Codex waiting dialogs — bug #1335

Validated on 2026-11-04 against base `a86e823`, using the installed
`/home/bastian/.local/share/mise/installs/codex/0.163.0/bin/codex`.

## Cause and change

`Would you like to` already matched command approvals, but recent output won
before question detection, and hook reports won before both. The real TUI
continues updating its terminal title/spinner while a dialog waits. Structured
`request_user_input` questions do not contain any of the old question phrases.
Also, `Pane::note_input` marks Tab/arrow navigation as answered.

Codex now uses the live confirmation/submission footer at the bottom of the
screen, including notes mode and the multiline controls rendered in narrow
panes. A live Codex dialog wins over recent output and old hook reports;
`answered` cannot hide it until Codex removes the controls. Other agents retain
their existing precedence. `explain` describes the same decision. Session
aggregation, sidebar and inbox consume that existing blocked state without
separate changes. Color remains the existing theme's blocked color, identical
to Claude; the built-in theme calls this `danger`, not a fixed blue.

## Real CLI/TUI evidence

All test data and processes used `/tmp/imkr-1335/`: empty HOME, CODEX_HOME,
XDG config/state/runtime, explicit imkr config, dedicated tmux socket and
`runtime/imkr/codex1335.sock`. Codex used `--no-daemon`, a custom provider with
`requires_openai_auth = false`, and a fake Responses HTTP endpoint bound only
to `127.0.0.1:18735`. No subscription credentials or model inference were used.
No approval or question was submitted. Only Tab navigation was sent to a
question. Test servers are stopped after measurement.

The fake endpoint emitted actual tool calls (`exec_command` with
`require_escalated`, and `request_user_input`); the **real binary** rendered
the screens. Captures are checked in under `src/fixtures/codex-0.163.0/`.

Temporary reproducibility/evidence files:

- `mock.py`, `check.py`, `home/.codex/config.toml`: fake endpoint and isolated
  integration driver/config. The driver has start, inspect, notes, approval,
  history, inbox and stop actions; run only with its isolated environment.
- `approval.txt`, `question.txt`: original real Codex terminal captures.
- `notes-explain.json`: real question after Tab: `answered: true`,
  `reported: working`, `quiet_ms: 25`, **state: blocked**.
- `list.json`, `sessions.json`, `agents.json`, `web.json`: final integration
  captures; both real Codex dialogs blocked, session count `blocked: 2`.
  An earlier capture also recorded the fake history control as done
  (idle promoted to unseen done), with `blocked: 2, done: 1`; that fake
  process exited after its 120-second test timeout before the final capture.
- `sidebar.txt`, `sidebar-ansi.txt`, `inbox.txt`: actual imkr UI; pane title,
  window and agent sidebar show `◆`, and inbox lists the structured question.
  The headless capture did not retain foreground colors; this proves the
  shared blocked state/style path, not a photographed blue pixel.

The first integration run exposed a layout difference: narrow panes render
three control lines without `|`. The fix normalizes those separators and
whitespace. The driver briefly attached an extra client using tmux's inherited
session environment; it was replaced with an explicit isolated environment.
No input or configuration change was made in that attached session.

## Checks and controls

- `env IMKR_HOSTNAME=imkr1335-test cargo test`: **310 passed, 1 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo fmt --check` and `git diff --check`: passed.
- `python3 docs/build.py`: generated the HTML docs.
- Captured approval, question and notes fixtures: detected while output is
  recent and with stale working/idle reports. Historical literal dialogs
  followed by the normal composer: not blocked. A question mark in prose,
  normal idle, and the normal working spinner: not treated as dialogs.
- Real isolated imkr + fake executable named `codex` printed the literal
  approval capture followed by `Finished` and the normal composer: done,
  not blocked. Real approvals and structured questions in the same session
  remained blocked.

Sandboxed full tests first failed on prohibited socket/filesystem operations;
outside the sandbox one existing test still assumed `laptop` was a remote
host, which collides with this machine's name. Using the repository's existing
`IMKR_HOSTNAME` override resolved it. Logs: `tests.log`,
`tests-unrestricted.log`, `tests-hostname.log`, `clippy.log`, `docs.log`.

## Decision and limits

Use TUI detection here rather than installing hooks into user accounts.
The installed binary contains PermissionRequest hook support, but imkr's
existing installer configures Claude/pi; it does not configure Codex. A
permission-only hook would not by itself establish coverage of structured
questions or already-running sessions. No complete Codex hook lifecycle was
validated or installed. Official configuration reference consulted:
https://learn.chatgpt.com/docs/config-file/config-reference.

Detection is deliberately bounded to the rendered 0.163.0 control layouts.
It does not infer that free-form assistant prose requires confirmation, and
async input, onboarding, other versions/localizations and clipped controls
have not been validated. A transcript deliberately ending in the exact live
footer can still mimic a dialog: screen inspection cannot prove modal state.
A process freeze without those controls remains governed by output/idle
heuristics. No claim of general hang detection is made.
