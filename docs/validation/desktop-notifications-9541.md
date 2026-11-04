# Desktop notification investigation and draft (#9541)

2026-11-04. Isolated draft against `91fd2ca`; no install, push, live update,
notification-setting change, or desktop test salvo. Task remains open for Keeper
review. See [recorded results](desktop-notifications-9541.json) and the
[reproduction script](desktop-notifications-9541.py).

## Findings and limits of attribution

The installed binary reported `imkr 0.21.94`, SHA-256
`264a7f614e14e951c2b2abafd3899cda15ec8bae37f07c7f634148ded513435d`.
All five observed production session servers used that installed executable's
inode. Version alone does not establish its exact source commit.

Read-only inspection of the retained desktop history found eight imkr entries,
including four identical remote `codex is done` entries for the same session,
paired 1 ms and 29 ms apart. Two other entries contained the literal test-like
CWD `~/actual worktree ' $(touch CWD_INJECTED)`. This worker did not generate
those history entries; their original producer is unproven. They must not be
presented as ordinary user project names or proof of command execution.

The baseline has a reproducible amplification defect: each local session server
independently polls remote sessions and emits every observed transition through
`watch_places` → `pass_on_alerts` → `notify-send`. There is no machine-wide
notifier ownership. This path dates to `628b4f6` (2026-10-12), rather than the
latest Codex-dialog changes. Five active servers can therefore amplify three
remote events into fifteen popups. This is a mechanism and a controlled
reproduction, **not proof of when the user's newly reported burst began**.
The recent #1335 changes affect state recognition, not desktop payload formatting.
Repeated identical states emitted nothing in the controlled baseline; no transport
redelivery queue was found in the desktop sender. Recent onset and the provenance
of the test-like history entries remain for review.

## Draft behavior

Only automatic System notifications change. Local title/body identify the actual
state transition, detected tool, project/window name and named bee (otherwise
session/pane). Terminal titles and screen excerpts are not used. Names are
control-filtered, whitespace-normalized, markup-escaped and capped at 64 characters
plus an ellipsis. Unknown tool identity is `Agent`, never guessed as Claude.
Remote `AgentInfo.program` can contain a renamed bee rather than a tool: such
names are preserved in the body with the neutral `Agent` title.

For remote System alerts, one process owns a nonblocking `flock` per remote host
under the machine's imkr state directory. The descriptor lasts for that process;
a waiting watcher can take over after it exits. Locks do not expire, throttle,
batch, or drop distinct state transitions. They are never unlinked by production
code. Coordination I/O errors fail open so questions/errors remain visible.
Fifteen distinct questions still produce fifteen alerts; this fixes duplicate
producers rather than concealing genuine activity. Urgency remains the existing
notify-send default for all kinds; failure/blocked alerts are not demoted or
suppressed behind done alerts.

The lock assumes watchers share the same local state directory and canonical host
name. Different host aliases, different state directories, mixed old/new binaries,
or an alive but stalled owner can still limit coordination. This is not a global
notification service or a solution to every possible state-detection oscillation.

## Actual argv and event causes

These are captured calls from the compiled server via the `notify-send` stand-in,
not invented screenshots. Every call starts with `--app-name=imkr`.

| Cause in private local pane | Baseline title / body | Draft title / body |
| --- | --- | --- |
| `working → blocked` | `bee-review needs you` / `window 1 · /tmp/imkr-notify-9541/before-controls` | `Codex · is waiting for your answer` / `imkr · bee-review` |
| `blocked → failed`, hook error `rate_limit` | `bee-review stopped: limit reached` / same baseline body | `Codex · stopped with an error` / `imkr · bee-review · stopped: limit reached` |
| `working → idle` | `bee-review is done` / same baseline body | `Codex · finished its turn` / `imkr · bee-review` |

“Finished its turn” describes the actual idle transition, not a completed project
or stopped Brain timer. Custom automatic blocked/account explanations remain
specific to their actual event. No timer event was invented.

## End-to-end evidence

Three private session servers, shared private state, fake SSH returning fifteen
agents (alternating Claude/Codex), private HOME/config/runtime/socket, fake
notify-send appending JSON argv. Real network helpers are replaced by stand-ins.
A private PTY client records terminal bytes without attaching the host UI.

| Scenario | Baseline calls | Final draft calls |
| --- | ---: | ---: |
| 15 remote agents working → blocked, 3 watchers | 45 | 15 |
| Same blocked snapshot again | 0 | 0 |
| 15 remote agents → failed | 45 | 15 |
| 15 remote agents working → idle | 45 | 15 |
| Owner exits, survivors observe 15 new blocked transitions | not applicable | 15 |
| Local blocked / repeated blocked / failed / done | 1 / 0 / 1 / 1 | 1 / 0 / 1 / 1 |
| Notification mode off, automatic + manual events | 0 | 0 |

Final draft remote events retain fifteen unique identities in each burst. No
`RAW SCREEN` or injected terminal-title fixture appears. Each local event was
observed after an actual CLI status change in a running fake agent pane. The
manual notification stays exactly:

```json
["--app-name=imkr", "User title <literal>", "User CWD/JT $(touch USER_INJECTED)"]
```

The four captured OSC 9 payloads match baseline **byte for byte**, including
`claude needs you`, `claude stopped: API error`, `claude is done`, and the manual
text, with the same original window/CWD suffix. Remote OSC formatting and bells
also retain their original path; ownership applies only to System alerts.

`App::push` and `App::send_notification` are byte-identical to baseline; their
hashes are recorded in the JSON. `push.rs`, `brain.rs`, `config.rs`, `term.rs`,
`pane.rs`, and `protocol.rs` are byte-identical too. Push/Brain fixture tests pass.
This is a source/fixture boundary proof, not a claim that real phone or Brain
notifications were sent during the isolated test. Existing production settings
were neither written nor toggled.

Checks: two new desktop tests; two push tests; three Brain tests; existing remote
transition test; `cargo clippy --all-targets -- -D warnings`; `cargo fmt --check`;
`git diff --check`. Full suite was not rerun.

Reproduce with separately compiled baseline and draft binaries:

```sh
python3 docs/validation/desktop-notifications-9541.py /path/to/baseline baseline --copies 3
python3 docs/validation/desktop-notifications-9541.py /path/to/draft draft
```

The script creates its own temporary world, asserts counts/identity/negative
controls and prints its artifact directory. It never invokes the host's real
notification command or SSH. Retained initial raw artifacts for this investigation
are under `/tmp/imkr-notify-9541/` (not needed for the checked-in summary).
