# Codex fetches its mail through its hooks (#1358)

Problem (09.11., v0.21.96): four messages stood at a Codex that was nearly always working as
„waits (working)“. `deliver_mail` types into a working agent only where `agent::QUEUES_INPUT`
allows it, and Codex's hooks did not fetch mail (`cli::status` skipped `mail_for_hook` for
`--agent codex`, unmeasured).

## Measurement: real Codex 0.163.0, dummy Responses provider

Private HOME/CODEX_HOME, no login, imkr test world with notifications off, fake `notify-send`,
dead DBus. Probe hooks logged their stdin and printed planned answers; the provider logged every
request.

- `UserPromptSubmit` → `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit",
  "additionalContext":"MARKER_CTX …"}}`: the next request carries a `developer` message
  `MARKER_CTX …` directly after the user's prompt.
- `Stop` → `{"decision":"block","reason":"MARKER_STOPREASON …"}`: without any input Codex sends
  a new request in the **same turn** (same `turn_id`) with the user message
  `<hook_prompt hook_run_id="stop:…">MARKER_STOPREASON …</hook_prompt>`, answers it, and the
  next `Stop` carries `"stop_hook_active": true`. The TUI shows „Blocked by hook“ with the reason.
- The same during a long turn: a reason planned while the turn ran came in after it, same turn.

So Codex follows both, as Claude Code does. Path (2) of the briefing applies; (3), typing into
a working Codex, is not needed.

## Change

- `cli::hook_takes_mail` / `HOOK_MAIL_AGENTS`: `--agent codex` fetches mail like Claude Code.
- `App::mail_waits_for`: a pane that fetches its mail (`fetches_mail`) is not held as working.
- `app::fetches_mail`: not with a shell back in front — Codex has no `SessionEnd` in
  `CODEX_HOOKS`, so a quit Codex would otherwise leave its messages waiting for a hook.
- Unchanged: `mail_hold` (drafts, unsent text, questions, restarts), `codex_composer`, the typed
  fallback for an idle agent after `HOOK_WITHIN` + `QUIET_BEFORE_TYPING`, and Codex without
  trusted hooks (none run, `takes_mail` never set, typed once idle).

## Evidence

`cargo build && python3 tests/codex_hook_mail.py --codex <codex 0.163.0>`:

```
PASS: hooks trusted, first turn reported
PASS: mail for a working Codex came through its Stop hook, in order, nothing typed
PASS: idle Codex gets the message typed, confirmed by its hook
PASS: Codex without trusted hooks keeps the typed delivery
PASS: every message arrived exactly once; fake notifier has zero calls
```

Against the previous build (8ad0f8b) the same test fails where the bug was: both messages
`held`, `why: working`, while Codex worked. `tests/codex_submit.py` (no hooks installed) and
`tests/mail_stall.py` stay green.
