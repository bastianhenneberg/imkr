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

Results: **321 passed, 1 ignored**; Clippy clean. The hostname override isolates an
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
  ambiguous Codex `--last`. Configured model and policy flags (`--yolo` / Claude
  `--dangerously-skip-permissions`) reach both native resume and cross-agent targets.
  Before this review fix, same-agent resume discarded `agent_commands` flags; the
  resume builder now retains them. No existing session is restarted for policy alone.
- Missing Claude and Codex transcripts, a handoff path blocked by a regular file,
  and a real input draft sent through `imkr tap`: request fails, source PID remains
  alive, SIGTERM handler was never called.
- Source agents chdir into an actual worktree while their waiting shells stay in
  the old directory. Both target directions and native resumes start in that actual
  worktree, including spaces/apostrophes/`$(touch CWD_INJECTED)` in its path. The CWD
  is captured once and supplied as a positional argument to a checked `cd` before
  `exec`. Missing CWD before SIGTERM leaves the source alive; source-termination
  handler removal of the CWD makes startup fail visibly without launching a target
  in the old shell directory. These four extra failure scenarios bring the PTY
  checks to twelve. The independent reviewer reproduction at
  `/tmp/imkr-review-dc544f5/cwd-regression.py` also passed against this fixed build.
- State paths containing spaces, apostrophes and `$(touch INJECTED)` remain literal;
  no command is executed. The target sees the exact path returned by CLI JSON.
- The source SIGTERM handler itself observes the published Markdown and complete
  raw task snapshot before exiting. All package files 0600, package directory 0700.

Rust tests additionally exercise both adapters against 3000-record histories,
retain compaction summaries (including Claude continuation text without a summary
flag) and original task plus complete raw bytes, reject
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
source API, plugin registry or periodic summary service is introduced. Normal switches
create no repository/history bundle or automatic Git commit; Git reference reads
use `--no-optional-locks`. Separate one-off recovery backups remain separate.

The package contains a complete bounded-in-time file snapshot, not the source
model's hidden context. The extraction is partial (first user message, up to three
recent summaries and 24 recent messages; 12000 characters each). Tool payloads
remain available in the raw file. Worktree path/branch/HEAD/status references and keeper
briefing directory references allow the target to inspect current work. Unreadable
or unidentifiable conversations refuse before SIGTERM. Codex mapping currently
requires a hook/session identity or a live open rollout in the foreground tree;
versions that keep no rollout open and supply no identity safely refuse. This includes
Work-managed/shared-daemon layouts where the only rollout FD is outside the source
foreground tree: a simulated external holder test proves refusal even with matching
CWD and local transcript files. Real Work/shared-daemon Codex 0.166 has not been
validated and is not claimed supported by the local 0.163 CLI/PTYS evidence.

The end-to-end reading agents are fakes; a paid live model was not used to claim
that it followed the handoff. Real CLI help establishes the argument boundary;
real PTYs exercise the restart, shell quoting, account environment and file read.
User approval dialogs were not answered, and target-agent trust/permission policy
continues to apply. Existing automatic same-agent fallback's bounded draft wait
was not changed.

## Explicit brief before the first target action

`account switch --brief FILE` reads a selected regular, nonempty UTF-8 file on the
server host and stores it as `brief.md` (0600) in the staged package. It is synced
along with the complete transcript and Markdown before the source receives SIGTERM.
The very first target argument instructs it to read the copied brief first, reconcile
its current assignment/restrictions with historical context, and avoid superseded
tasks. This is an explicit selection, not automatic recovery-package discovery or
an approval inferred from historical tool output. CWD behavior remains source-process
CWD; the three real recovery summaries must specify their proven action worktrees.

Both cross-agent directions and both native resumes with a brief are tested. The
source SIGTERM handler deletes the original selected SUMMARY before the target runs;
the target still reads the private durable copy **before** handoff.md. CLI JSON
returns both saved paths. Missing selected briefs in both directions refuse with
the source alive; empty/non-UTF8 briefs are rejected by unit tests. Relative source
brief paths with spaces/apostrophes/`$(...)` stay literal. The explicit brief uses a
new wire request, so older servers cannot silently discard the argument. Optional
integration invocation against an archived old server:

```sh
python3 tests/handoff.py target/debug/imkr /tmp/imkr-handoff-1340/impl-legacy-1f-target/debug/imkr
```

The eighteen regular PTY scenarios become nineteen with this old-server refusal
check. No original session or reviewed recovery summary is changed by these tests.

## Explicit target directory

`account switch --dir DIR [--brief FILE]` resolves the selected path on the CLI host
and sends explicit start options in a dedicated wire request. Older servers reject
the entire request rather than silently losing either option. Without `--dir`, source
process CWD stays the default. With it, transcript identity is still checked against
source CWD while the selected target CWD is validated (existing directory and search
permission) before SIGTERM. No directory is created. The existing checked `cd`/`exec`
wrapper handles a deletion/rename race at startup; Codex also gets `--cd "$PWD"`,
including on native resume, to select its workspace explicitly.

Twelve extra PTY cases cover both extras together, directory alone and native resume
in both agents; missing directories, regular files used as directories and target
removal during the source SIGTERM handler refuse safely. The target path is distinct
from both waiting-shell and source CWD and contains spaces, apostrophes and command
substitution characters. Existing nonexistent paths remain nonexistent after refusal.
Access-denied target directories are also checked with the unprivileged test user
(uid 1000): source remains alive. Together with prior cases: 32 regular scenarios,
35 with old-server rejection checks for brief, directory and both options.

`/tmp/imkr-handoff-1340/real-start-contract.json` was read as input only. The three
real targets require the exact contract CWD and `<package>/SUMMARY.md` as `--dir` and
`--brief`, respectively. No real start, account selection, production-tree mutation
or recovery-package edit was performed by this implementation.

## Explicit managed-source identity

`account switch --source-session UUID` accepts only a full explicit Codex UUID.
It is checked against any pane hook ID before locating exactly one readable source
transcript in the source process/account root; metadata session identity and source
CWD must agree. Target `--dir` does not weaken this source-CWD check. The existing
local `/status` UI is the operator's token-free source of the UUID; imkr never types
it automatically or treats it as a machine protocol. Mapping failure points to
`/status` plus this explicit option. No database, daemon IPC or newest/CWD heuristic
was added, and `--no-daemon` is not globally forced.

Seven managed-source PTY scenarios simulate a source frontend with no rollout FD;
a separate harness process holds its exact JSONL. Explicit UUID succeeds and the
target reads that transcript; wrong/malformed UUID, mismatching hook ID, wrong source
CWD, missing zero-turn history and no explicit mapping refuse with the source alive.
The last case checks the actionable error. A further native resume case uses the
explicit UUID and the chosen target directory. Rust tests also check explicit
identity against an externally held rollout in a distinct live process tree.
An independently rebuilt exact `1f054a2` server (separate target directory, not a mutable
review build path) is used for compatibility checks. An additional old-server scenario verifies that the dedicated source-identity request
cannot be silently ignored. Totals: 40 normal PTY scenarios, 44 with all old-server
checks. These are isolated fake-source process tests, not a claim of a paid-model
Work-managed 0.166 round trip. Real local `/status` and 0.166 saved-rollout FD probes
remain separately scoped evidence; original sessions were never touched.
