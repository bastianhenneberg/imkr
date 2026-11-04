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

## Draft behavior and transport boundary

Only automatic agent System notifications change. Title/body identify the actual
state, detected tool, project/window name and bee (otherwise session/pane).
Names are control-filtered, whitespace-normalized, markup-escaped and capped at
64 characters plus an ellipsis. Terminal titles and screen excerpts are not used
as popup text. Unknown tool identity is `Agent`, never guessed as Claude. Remote
`AgentInfo.program` can contain a renamed bee: that name is preserved with the
neutral `Agent` title.

For remote System alerts, one process owns a nonblocking `flock` per remote host
under the machine's imkr state directory. Other session servers still observe
state but do not independently emit desktop alerts. A watcher takes over after
its owner exits. Coordination I/O errors fail open so attention is not silenced.
This addresses duplicate producers before collection; collection is additional
behavior, not the claimed cause of the recently reported burst.

A small **per-server** collector holds automatic events for one second from the
first event in each `(host, originating session, event kind)` group. Later
arrivals do not extend the deadline. The existing 500 ms upkeep cadence adds
scheduling slack. Fifteen bees in one session/kind become one summary retaining
all fifteen identity lines; larger groups split into chunks of fifteen. Each
bee/kind retains its latest detail within that short collection window.
Different sessions and event kinds remain separate. Thus fifteen local agents
across three session servers produce three summaries. No global service,
persisted event queue, or new setting was introduced.

Errors use `--urgency=critical`, questions/account attention `normal`, and turn
completions `low`. Ready errors are sent before questions and completions by one
short-lived worker per flush, avoiding a race between independent child
processes. The actual desktop daemon controls presentation. Automatic argv uses
`--` before title/body to prevent labels becoming options. The generic sender is
unchanged for manual notifications, OSC, login and screen-access notifications.

Duplicate identity includes host, session, pane, kind and actual event detail
within the current observed episode. Identical snapshots and repeated submissions
do not notify again after a flush. Recovery, agent replacement, or a changed
waiting-question fingerprint re-arms immediately; no TTL hides a fresh question.
Selection markers and wrapping/whitespace are normalized. Fingerprints remain
private in memory, never in popup bodies or other transports. They are best-effort
screen identities: changes within the retained tail can produce a new fingerprint;
distinguishing content outside it requires a state transition to be recognized.

An ordinary question answered during collection is canceled. Actual failures
and delivery warnings survive recovery or disappearance of their agent. A
normal shutdown starts remaining commands before server threads exit; ordinary
questions from local agents ending with that server are canceled. A new remote
owner announces active questions/errors once, including at initial discovery,
so a crashed owner's pending incident is not lost. It never replays completions.
Takeover can intentionally reannounce an already displayed, still-active
incident. Exactly-once delivery across process death is not claimed.

Ownership assumes a shared local state directory and canonical host name.
Aliases, separate state directories, old binaries, and a living but stalled owner
limit coordination. This does not solve every possible state-detection oscillation.
The recent onset of the user's live burst remains unproven.

## Actual desktop argv and event causes

Calls below were recorded from compiled servers via a fake notify-send, not
invented screenshots. Baseline argv starts with `--app-name=imkr`; the burst
draft adds the explicit urgency and option terminator before title/body.

| Private local event | Baseline title / body | Burst draft title / body |
| --- | --- | --- |
| working → blocked | `bee-review needs you` / `window 1 · /tmp/imkr-notify-9541/before-controls` | `Codex · is waiting for your answer` / `local · imkr · bee-review` |
| blocked → failed, hook error rate_limit | `bee-review stopped: limit reached` / same CWD body | `Codex · stopped with an error` / `local · imkr · bee-review · stopped: limit reached` |
| working → idle | `bee-review is done` / same CWD body | `Codex · finished its turn` / `local · imkr · bee-review` |

“Finished its turn” describes the idle transition, not a completed project or
stopped Brain timer. Custom automatic attention/account explanations stay
specific to their actual event. No timer event was invented.

## End-to-end evidence

Private HOME/config/runtime/state/sockets, fake SSH returning fifteen agents
alternating Claude/Codex, and fake notify-send recording real argv. All network
helpers are stand-ins. A private PTY client records terminal bytes without
attaching the host UI. Only private Popen server processes are stopped or crashed.
The first final run was invalidated by a /tmp quota error; its partial results
are not acceptance evidence. The task's build cache/binaries were moved to the
worktree's ignored target directory and the run restarted.

| Measured scenario | Baseline | Burst draft |
| --- | ---: | ---: |
| 15 remote working → blocked, 3 watchers | 45 | 1 normal summary |
| Same blocked snapshot again | 0 | 0 |
| 15 remote → failed | 45 | 1 critical summary |
| 15 remote working → idle | 45 | 1 low summary |
| Changed question while still blocked | not measured | 1 fresh normal notice |
| Only its selection arrow moves | not measured | 0 |
| Mixed 15: 5 failed, 5 blocked, 5 completed | not measured | 3 notices: critical → normal → low |
| 15 waiting bees in 2 sessions with overlapping pane IDs | not measured | 2 summaries: 7 + 8 |
| Owner exits; survivors observe 15 new questions | not applicable | 1 summary with 15 identities |
| Owner killed during collection of 15 failures | not applicable | 1 critical summary after takeover |
| 15 local agents across 3 independent session servers | not measured | 3 summaries, 5 per session |
| Repeated local blocked reports after summaries | not measured | 0 |
| Local blocked / repeated blocked / failed / done | 1 / 0 / 1 / 1 | 1 / 0 / 1 / 1 |
| Local graceful exit with pending failure | not measured | 1 critical notice |
| Off mode, automatic + manual events | 0 | 0 |

Each fifteen-bee summary has fifteen distinct identity lines, with no RAW SCREEN
or injected terminal-title fixture text. Receipt/observation timestamps use the
same monotonic clock. Exact deadline tests use an injected Instant; measured
receipt delays are in the JSON: normal cases measured 1.14–1.47 seconds, and
pending-failure crash recovery 2.07 seconds. Remote polling delay precedes observation and
is not included in these collection measurements. Takeover may add a separate
poll/upkeep delay.

The manual notification remains byte-identical:

```json
["--app-name=imkr", "User title <literal>", "User CWD/JT $(touch USER_INJECTED)"]
```

All four captured local OSC 9 payloads match baseline byte-for-byte, including
`claude needs you`, `claude stopped: API error`, `claude is done`, and manual text
with the original window/CWD suffix. Remote OSC formatting, bells, activity and
Push/Brain calls retain their old branches; new same-state question detection
calls the desktop collector directly.

App::push and App::send_notification are byte-identical to 91fd2ca; their hashes
are in the JSON. push.rs, brain.rs, config.rs, term.rs, pane.rs and protocol.rs
are byte-identical too. Existing push/Brain fixture tests passed. This is a
source/fixture boundary proof, not a claim of sending real phone or Brain events
in these isolated tests. No production settings were written or toggled.

Checks: nine desktop tests, prior two push tests, three Brain tests and existing
remote-transition test; Clippy all-targets with -D warnings; fmt and diff checks.
The complete suite was not rerun. The JSON distinguishes the measured original
baseline, initial ownership-only draft and final burst draft.

Reproduce with separately compiled baseline and draft binaries:

```sh
python3 docs/validation/desktop-notifications-9541.py /path/to/baseline baseline --copies 3
python3 docs/validation/desktop-notifications-9541.py /path/to/draft draft
```

The script creates a temporary world, asserts counts/identity/priorities/recovery
and negative controls, and prints its artifact directory. Initial raw artifacts
remain under /tmp/imkr-notify-9541; the final JSON identifies its private world.
No rollout is authorized; Task #9541 stays open for Keeper review.
