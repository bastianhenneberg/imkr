# Local Claude ↔ Codex context handoff (#1340)

Validation on 2026-11-04. No original session was switched, no real recovery was
performed, and no source model or provider endpoint was called. No deployment or
push. Local installed help checked: Codex 0.163.0 (`[PROMPT]`, `--add-dir <DIR>`),
Claude Code 2.1.304 (`[prompt]`, `--add-dir <directories...>`). The builder passes
one directory argument and a `--` boundary before the single prompt argument.
Configured agent flags are retained; the historical transcript is never shell source.

## Reproduce

```sh
IMKR_HOSTNAME=handoff-regression-host cargo test --bin imkr
cargo clippy --all-targets -- -D warnings
cargo build
python3 tests/handoff.py target/debug/imkr
```

Results: **319 passed, 1 ignored**; Clippy clean. The hostname override isolates an
existing test that assumes `laptop` is a remote machine (without the override,
`only_a_pane_can_be_told_its_message_did_not_arrive` fails on this laptop).

`tests/handoff.py` starts real isolated imkr servers and shells/PTYS, with Python
executables named `claude` and `codex`. Only the fake Claude source reports a hook;
the fake Codex is resolved from its live PID/tree's open rollout descriptor and
session metadata. Each target receives the handoff via its actual shell command
line (configured model flags retained), reads that exact Markdown path and its raw transcript, and records the
literal task, decision and work markers. Checks cover:

- Claude → Codex in standard accounts and Codex → Claude in a provider account
  (`ANTHROPIC_BASE_URL` verified inside the target, no endpoint called).
- Keeper identity and queued mail in the target-readable snapshot; pending mail
  remains queued behind the existing blocked/restart/composer guards.
- Both agents' native same-agent resume uses `source-exact`, no handoff and no
  ambiguous Codex `--last`.
- Missing Claude and Codex transcripts, a handoff path blocked by a regular file,
  and a real input draft sent through `imkr tap`: request fails, source PID remains
  alive, SIGTERM handler was never called.
- State paths containing spaces, apostrophes and `$(touch INJECTED)` remain literal;
  no command is executed. The target sees the exact path returned by CLI JSON.
- The source SIGTERM handler itself observes the published Markdown and complete
  raw task snapshot before exiting. All package files 0600, package directory 0700.

Rust tests additionally exercise both adapters against 3000-record histories,
retain compaction summaries and original task plus complete raw bytes, reject
mismatched identities and duplicate session metadata, and prove a live source
process's descriptor wins over a newer neighbor transcript in the same CWD held
open in another process. The source process's `CODEX_HOME` wins over an incorrect
server/account-root fallback.

## Boundaries and extension point

Production support is deliberately limited to explicit `claude` / `codex` agent
types from accounts, never inferred from model/provider names. Extend
`handoff::metadata` and the extraction branches in `handoff::save` for another
persisted format, with fixtures and exact identity tests; extend the command
adapter only when its positional prompt boundary is checked locally. No daemon,
source API, plugin registry or periodic summary service is introduced.

The package contains a complete bounded-in-time file snapshot, not the source
model's hidden context. The extraction is partial (first user message, up to three
recent summaries and 24 recent messages; 12000 characters each). Tool payloads
remain available in the raw file. Worktree status/log/diff statistics and keeper
briefing directory references allow the target to inspect current work. Unreadable
or unidentifiable conversations refuse before SIGTERM. Codex mapping currently
requires a hook/session identity or a live open rollout in the foreground tree;
versions that keep no rollout open and supply no identity safely refuse.

The end-to-end reading agents are fakes; a paid live model was not used to claim
that it followed the handoff. Real CLI help establishes the argument boundary;
real PTYs exercise the restart, shell quoting, account environment and file read.
User approval dialogs were not answered, and target-agent trust/permission policy
continues to apply. Existing automatic same-agent fallback's bounded draft wait
was not changed.
