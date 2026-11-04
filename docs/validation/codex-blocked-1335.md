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

## Follow-up: actual live dialogs and color requirement

A later keeper message reported real approval dialogs in the worker sessions
while installed imkr still said working. On receipt, both dialogs were already
gone: fresh `imkr read` and `explain` showed normal working composers (2/10 ms
since output), so those particular transient screens could not be captured
again without inventing a new approval. No foreign approval was answered.
The installed binary still reported 0.21.94; this commit had only been tested
in the isolated server.

The first colorless capture was explained by `NO_COLOR=1` inherited from the
tool environment. Repeating the actual isolated TUI with that variable removed
produced ANSI foreground `38;5;9` for `codex ◆ blocked`, its frame and sidebar
indicator: **light red, not blue**, under the built-in theme. Blue
`38;2;97;175;239` belonged to focus/window labels. Captures and JSON from this
repeat run are preserved separately at `/tmp/imkr-1335/color-recheck/`; the
original driver filenames now hold this later run. The earlier notes/stale
hook capture remains in `notes-explain.json`.

This proves the current color output, not a desktop screenshot. The explicit
request for blue needs a separate UI color change; sharing Claude's existing
blocked state alone does not prove it blue. The keeper and the worker assigned
visual bug #1338 were sent the exact evidence and source locations
(`theme::HONEY.danger`, `sidebar::state_style`). That color requirement remains
pending their visual integration check. The status fix does not change colors.

## Cross-session inbox and session switch

The follow-up requirement was exercised end-to-end with two separate isolated
servers, not just `agent::state` or the waiting session's own inbox:

1. Session `waiting` ran the real Codex 0.163.0 against a local fake Responses
   server on port 18736, with an actual `exec_command` approval and
   `-a on-request`. Session `observer` ran only a shell.
2. Injected an old `working` hook report into `waiting` while the real dialog
   remained on screen. Its `SessionInfo` still contained `agents.blocked: 1`
   and `agent_panes[0].state: blocked`.
3. Opened `Ctrl+B i` in **observer**. Its actual rendered inbox listed
   `waiting` / `codex` and showed the approval question. The observer sidebar
   simultaneously showed the waiting session and blocked Codex agent.
4. Pressed Enter with an empty inbox answer. The attached client switched to
   **waiting**, showing the actual command approval. It did not submit the
   approval: subsequent `explain` still had `answered: false`,
   `reported: working`, `quiet_ms: 83`, `state: blocked`.

Result: PASS. Driver `/tmp/imkr-1335/cross.py`, log
`/tmp/imkr-1335/cross-result.log`, evidence directory
`/tmp/imkr-1335/cross/`: `sessions.json`, `waiting.json`, `inbox.txt`,
`switched.txt`, `after-switch.json`. Textual UI captures are also checked in
beside this report. Both servers and the fake endpoint were stopped.

The reviewed path is `Pane::agent_state` → `App::update_agents` /
`shown_state` → `App::list` → `client::local_sessions` (`Request::List`,
constructs `SessionInfo.agents` and `agent_panes`) → observer's `places.local`
→ `other_agents` → `agent_entries` → `inbox_agents` (filters blocked/done/failed)
→ `inbox_items`. Empty-answer Enter calls `go_to_other_agent`, which switches
the client to the selected session and pane. The state fix reaches this whole
path; no separate inbox filtering/selection patch was needed. Another machine
and mixed-version client/server propagation were not tested.

## Real Claude control without bypass

Used installed Claude Code 2.1.304 with an empty isolated HOME, dummy API key,
local fake Anthropic Messages endpoint on port 18737, `--permission-mode
default`, and an explicit `permissions.ask = ["Bash"]` rule. No real account
or subscription was used. The real TUI displayed `echo imkr-control` and
`Do you want to proceed?`; the command was never approved or run.

With isolated UserPromptSubmit→working and PermissionRequest→blocked hooks
(the normal imkr integration), the same two-session inbox/Enter-switch test
passed. Before switching: `reported: blocked`, `quiet_ms: 240`, `state:
blocked`. After switching: `answered: false`, `reported: blocked`,
`quiet_ms: 235`, `state: blocked`. Evidence:
`/tmp/imkr-1335/claude-cross/{waiting-read.json,waiting.json,sessions.json,inbox.txt,switched.txt,after-switch.json}`,
log `/tmp/imkr-1335/claude-cross-result.log`, drivers
`/tmp/imkr-1335/claude-cross.py` and `/tmp/imkr-1335/anthropic.py`. All test
processes stopped.

A separate `--bare` run without hooks demonstrated an existing limitation:
while a recognizable question remained on screen, output 469 ms ago kept
Claude working and the observer inbox empty. Captures:
`/tmp/imkr-1335/claude-no-hooks-working.json` and
`/tmp/imkr-1335/claude-no-hooks-inbox.txt`. This is not covered by the
Codex-specific live-control precedence fix; the supported hook control above
works. The initial Claude attempt stopped at its dummy API-key confirmation;
only the later run with that dummy key preconfigured and the literal Bash
approval is counted as the tool-approval control. No dialog was answered.

## Full lifecycle acceptance: both dialog kinds

Two more independent isolated two-session runs verified that the waiting entry
also disappears after a response. These are the newly created deterministic
fake-model test dialogs only; no user's or other worker's approval was touched.

- **Command approval:** observer inbox listed waiting Codex → empty-answer
  Enter switched to waiting without approving → selected option **3, No**,
  then Enter in that test dialog → live controls disappeared → after quiet,
  `state: idle`, `question: null` → reopened observer inbox showed
  `nothing waits for you`. The command was refused, not approved. An initial
  Esc-only attempt failed the immediate controls-removed check; the successful
  runner still printed the stale label “Esc”, corrected by an appended log
  clarification. Its actual code and capture show option 3 / interrupted turn.
- **Structured question:** same observer inbox and session switch → Enter
  submitted the first answer to the isolated `request_user_input` question →
  fake model returned `Finished.` → `state: idle`, `question: null` → observer
  inbox showed `nothing waits for you`.

Both runs first verified live controls override an injected stale working
report. After the own test response, `question` was already null before
clearing that deliberately injected report. A still-reported working state
otherwise remains authoritative; the driver then explicitly clears its own
fake report to measure the normal no-hook idle path. It does not claim to
repair arbitrary stale hooks after a turn. The waiting client had viewed the
result before returning to observer, so it was idle rather than unseen done
(which is intentionally also listed in the inbox).

CLI/TUI protocol artifacts:

- `/tmp/imkr-1335/lifecycle-approval.log` and
  `/tmp/imkr-1335/lifecycle-question.log` (PASS).
- Drivers `/tmp/imkr-1335/lifecycle-{approval,question}.py`, endpoints only on
  loopback ports 18739/18740, own HOME/config/state/runtime/tmux per run.
- `/tmp/imkr-1335/lifecycle-approval/` and
  `/tmp/imkr-1335/lifecycle-question/`: `inbox.txt`, `switched.txt`,
  `after-response-before-clear.json`, `after-response.json`,
  `after-response-screen.txt`, `inbox-after-response.txt`.
- Historical literal footers followed by a normal composer remain negative
  in the fixture unit test and the earlier fake Codex integration control.

All fake endpoints and isolated servers stopped. These checks do not change
production approval policy or install user hooks. Fixed-suite result remains
**310 passed, 1 ignored**. Exact baseline `a86e823`, measured separately from a
Git archive in `/tmp/imkr-1335/baseline` with local empty Git metadata for the
branch-detection test: **309 passed, 1 ignored**, log
`/tmp/imkr-1335/baseline-tests.log`. Both suites used
`IMKR_HOSTNAME=imkr1335-test` and unrestricted test sockets. The initial archive
without `.git` failed only that branch-detection test; it passed after local
`git init -b baseline`. This follow-up changes evidence/docs only.

The explicit **blue** requirement is still not met by this status patch: its
measured built-in blocked color is light red. The assigned visual worker owns
that coordinated UI check/change; no blue screenshot is claimed here. Bug
status remains pending keeper acceptance.
