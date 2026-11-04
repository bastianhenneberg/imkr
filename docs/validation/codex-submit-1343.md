# Codex initial submission (#1339; duplicate #1343)

Tracking: #1339 is reopened; #1343 is its closed duplicate. Session, worktree
and artifact names retain `1343` for traceability.

Latest evidence: the footer-dependent false hold is now proven by real live
ANSI snapshots; see the final section. Earlier statements about unreproduced
production mail below describe the state before that evidence arrived.

## Result and limit

Baseline `91fd2ca` reproducibly leaves a long **single-line `imkr send`** in
Codex's composer. The real frontend displays `[Pasted Content 1161 chars]`,
never calls the local model endpoint, and remains idle. A separate empty
`send` (one Enter) submits the exact marker and produces an assistant answer.
Short single-line input and bracketed multiline input succeed on the baseline.

The change frames single-line input to Codex as a bracketed paste when Codex
has enabled that terminal mode. This supplies an explicit paste boundary before
the existing single Enter. It adds no retries, timers, delayed submissions,
notification changes, or confirmation inference. Shell single-line input retains
its existing encoding. The behavior is tested in the real frontend, rather than
against a program that merely records terminal bytes.

**This is a demonstrated fix for the direct `send` path, not proof that every
reported `msg` stall has been fixed.** Mail already brackets single-line input.
The reported production `msg` failure did not reproduce in these isolated runs;
no change to `deliver_mail` is justified by this evidence. Keep #1339 open until
the keeper has reviewed this scope and obtained evidence for the remaining mail
report. No production server, account, pending message, or deployment was changed.
Real frontend checks have now also passed on codingmachine in private test
sessions (details below). The unreproduced production mail failure still blocks
full acceptance of #1339.

## Reproduction

Build with `cargo build`, then run:

```sh
python3 tests/codex_submit.py \
  --codex /home/bastian/.local/share/mise/installs/codex/0.163.0/bin/codex

python3 tests/codex_submit.py \
  --codex /home/bastian/.local/share/mise/installs/codex/0.163.0/bin/codex \
  --managed

python3 tests/codex_submit.py \
  --codex /home/bastian/.local/share/mise/installs/codex/0.163.0/bin/codex \
  --managed \
  --daemon-runtime /home/bastian/.codex/packages/app-server-daemon/releases/0.166.0-x86_64-unknown-linux-musl
```

The last command copies executable resources into the test's private HOME; it
reads no credentials or conversations from the original account. `runtime.json`
records `/proc/<private-daemon-pid>/exe`, so managed mode is distinguished from
`--no-daemon` and the CLI version is distinguished from the daemon version.
No claim is made about a standalone 0.166 frontend.

To demonstrate the regression, build baseline `91fd2ca` separately and pass its
binary with `--binary /absolute/path/to/baseline-imkr`. The test fails within its
15-second response deadline at `MARKER_SINGLELINE`, with zero received markers.
There is intentionally no fallback Enter in this regression test.

The harness prints a fresh evidence directory and retains requests, CLI output,
response screens, mail state, runtime identity and `result.json`. It binds a
stdlib Responses server only to `127.0.0.1` with an OS-assigned port. It uses no
model subscription. CLI calls have 15-second timeouts; the deliberately busy
response has a 20-second maximum hold. Test imkr servers, and managed daemons
whose `/proc` environment identifies this exact private test root, are stopped
in `finally`. Downloaded runtime binaries are removed after the run; evidence is
retained.

Before **every** test application/PTY starts, the harness establishes all the
notification shields: `notifications = "off"`, its own fake `notify-send` first
in PATH, an invalid DBus socket, and no DISPLAY/WAYLAND_DISPLAY. Its HOME, XDG
directories, config, state, sockets and working folder are private. `notify.log`
must remain empty. No desktop test was performed.

## Observed checks

The final no-daemon and managed-0.166 runs both passed:

- A 1,160-character single-line burst and a 16,012-character multiline paste
  reach the actual Responses endpoint and appear in frontend assistant answers.
  Complete request bodies were compared; Codex strips the final newline from
  the multiline paste (16,011 characters in the model request).
- Two real Codex sessions exchange worker completion -> keeper -> worker
  follow-up, with each marker received in exactly one model turn.
- A message waits during an intentionally held model response, then submits
  after the real frontend returns to idle.
- A human draft holds pending mail across `imkr update`; the draft is still in
  the real composer and the queued marker has not reached the provider.
- An account switch to `codex:standard` is refused while that draft exists.
  This tests refusal, not a completed account restart/resume.
- A real `exec_command` approval generated by the local provider remains blocked
  and unanswered while mail waits, including across `imkr update`. No command is
  approved or executed.
- An isolated bash pane still executes its normal single-line command.
- No duplicate marker turns and zero fake-notifier calls.

Default managed 0.163 also passed the submission, two-session, busy/idle,
human-draft/handover and real-approval/handover checks before the account-refusal
and shell checks were added. Its result is not substituted for the managed-0.166
run.

Unknown-layout rejection is covered by the existing
`codex_dialog_or_unfamiliar_layout_is_not_an_empty_composer` unit test. There is
no new delayed-submit state to transfer or to test against concurrent human
input. The pre-existing synchronous 20 ms write gap remains; an input event
racing within that gap was not tested. The patch does not improve missing
bracketed-paste negotiation during startup, an unfamiliar frontend, or an
unreproduced already-bracketed mail stall. It never calls a typed message
`Confirmed` based on the screen or the dummy provider's response.

## Evidence from this worktree

- Initial baseline measurement and separate-Enter recovery:
  `/tmp/imkr-submit-1343/probe/baseline-burst.log` and `baseline-managed.log`.
  The initial marker was `MARKER_BURST` (1,156 total characters).
- Canonical regression driver on the saved baseline:
  `/tmp/imkr-submit-1343/e2e-canonical-baseline.log`, evidence
  `/tmp/imkr-submit-1343-lwitgfm_/` (expected timeout, zero model turns).
  Baseline debug binary SHA-256:
  `0fe46a1ca614df524970c915b3040a8c923f4c028df0733e528fc81ae82f83b3`.
- Final no-daemon suite: `/tmp/imkr-submit-1343/e2e-final.log`, evidence
  `/tmp/imkr-submit-1343-y07_1asc/`.
- Final managed suite, CLI 0.163 / daemon 0.166:
  `/tmp/imkr-submit-1343/e2e-cleanup-166.log`, evidence
  `/tmp/imkr-submit-1343-8b0lj0wj/`, including `runtime.json`.
- Managed 0.163 suite: `/tmp/imkr-submit-1343/e2e-managed-approval.log`,
  evidence `/tmp/imkr-submit-1343-o4yaxr2h/`.
- `IMKR_HOSTNAME=imkr1343-test cargo test`: 316 passed, 1 ignored
  (`/tmp/imkr-submit-1343/tests-hostname.log`). The initial unqualified run
  hit the existing `only_a_pane_can_be_told_its_message_did_not_arrive`
  assumption that `laptop` is remote; 315 passed, 1 failed, 1 ignored.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
  `git diff --check`: passed. Clippy log:
  `/tmp/imkr-submit-1343/clippy.log`.

## Codingmachine follow-up under #1339

The SSH account is `codingmachine`, not `bastian`. Test copies of the review
binary, real Codex 0.163 frontend, harness and executable-only 0.166 runtime
package were placed in `/tmp/imkr-submit-1343-acceptance.Mu41zI/`. These are
isolated test artifacts, not an installation/update of any production server.
The same notification shields precede every test PTY.

- `no-daemon.log`, evidence `/tmp/imkr-submit-1343-vwqnn5m2/`: all checks pass,
  including exact single-line/multiline bodies, two-session completion/follow-up,
  busy-to-idle, drafts/approvals/handover, account-switch refusal and shell input.
- `managed.log`, evidence `/tmp/imkr-submit-1343-pdseft7u/`: an initial managed
  attempt using only the copied CLI exited back to bash before initial idle,
  with zero provider turns. This failed bootstrap is not a submission result;
  its underlying bootstrap error was not diagnosed here.
- `managed-166.log`, evidence `/tmp/imkr-submit-1343-jbl72osi/`: the complete
  private runtime package supplied via `--daemon-runtime` passes all checks.
  Its `runtime.json` records the actual private 0.166 executable, separately
  from the 0.163 frontend. No assumption about codingmachine's installed CLI
  version is used.

The final harness also saves each session's complete mail log and explicitly
rejects any `confirmed` event: the test provider can prove acceptance to the
harness, while imkr must not infer it from Codex's screen. Additional logs for
that assertion are `e2e-1339-no-false-ack.log` on laptop and
`no-false-ack-166.log` in the codingmachine acceptance folder.

Both explicit no-false-confirmation runs passed: laptop evidence
`/tmp/imkr-submit-1343-yaggoywv/` and codingmachine evidence
`/tmp/imkr-submit-1343-lqm_6jkf/`. The codingmachine runtime identity, full mail
logs, results and run logs are also collected on laptop in
`/tmp/imkr-submit-1343/codingmachine/review-evidence.tgz`.

## Portable keeper acceptance bundle

Prepare a credential-free archive with the test binary, real frontend, the
executable-only runtime release and the complete harness:

```sh
python3 tests/package_codex_submit.py \
  --binary target/debug/imkr \
  --codex /home/bastian/.local/share/mise/installs/codex/0.163.0/bin/codex \
  --runtime /home/bastian/.codex/packages/app-server-daemon/releases/0.166.0-x86_64-unknown-linux-musl \
  --out /tmp/imkr-submit-1343/codex-submit-1339.tgz
```

The packager exclusively creates a new output file and a SHA-256 sidecar. It
never starts an application, installs anything or sends files to another host.
The archive contains `README.txt`, `manifest.json` (per-file hashes), `run.py`,
`codex_submit.py`, `imkr`, `codex` and `runtime/<release>/`. No account folder is
packaged. Review those scripts and the manifest before execution.

After copying the reviewed archive to **each host**, use a fresh directory:

```sh
review_dir=$(mktemp -d /tmp/imkr-1339-review.XXXXXX)
tar -xzf /path/to/codex-submit-1339.tgz -C "$review_dir"
python3 "$review_dir/run.py"
```

For codingmachine, execute those same commands over SSH as `codingmachine`.
The runner checks artifact hashes and runs no-daemon plus managed sequentially;
`--mode no-daemon` or `--mode managed` selects one. It writes per-run logs and
`summary.json`. Every test application/PTY is created by the shielded harness,
with its own HOME/XDG/config/state, loopback provider and fake notifier. The
outer runner has a 180-second limit per mode; the harness also bounds individual
operations and cleans its own servers in `finally`. A forcibly terminated
harness requires checking only its exact private `SUBMIT_TEST_ROOT` processes.

A passing bundle proves the specified isolated handshakes and guard cases on
that host. It does not diagnose an unreproduced production already-bracketed
mail stall or replace keeper rollout review. The history/composer fix on
`91fd2ca` remains distinct from this direct-send first-submission fix.

For one combined remote command after review, the repository also provides:

```sh
python3 tests/run_codex_submit_hosts.py \
  --bundle /tmp/imkr-submit-1343/codex-submit-1339.tgz \
  --host laptop --host codingmachine \
  --output /tmp/imkr-1339-two-host-results
```

Use SSH destinations appropriate to the keeper's machine (`--host` accepts an
SSH alias or `user@host`). This orchestrator copies the same archive to a fresh
private `/tmp/imkr-1339-review.*` directory on each host, runs both modes,
retains per-host logs and a combined summary, and returns nonzero if either host
fails. The output directory must be new. It does not replace/install binaries
in PATH or update existing servers. SSH/copy/remote-run operations are bounded.

Prepared artifact: `/tmp/imkr-submit-1343/codex-submit-1339.tgz` with SHA-256
`1d930f505d93fc13bbf6d336cd2be279e07eb2c1809ca53612d1ff2459100a79`;
the checksum is also in `codex-submit-1339.tgz.sha256`. This initial archive was
created at gzip's default compression level; the builder now uses level 1 for
faster repackaging, so a rebuilt archive can have a different archive hash.
Its per-file manifest still verifies the actual executable and harness contents.

The prepared archive and SSH orchestrator were exercised together on
codingmachine: `/tmp/imkr-submit-1343/bundle-run.log` and
`/tmp/imkr-submit-1343/bundle-codingmachine/summary.json` report exit 0 for
`/tmp/imkr-1339-review.MLAukv`. Both no-daemon and managed runs passed all real
frontend assertions. Their remote evidence directories are
`/tmp/imkr-submit-1343-04alfdp5/` and `/tmp/imkr-submit-1343-fs0wvs6_/`.
Laptop's real frontend checks were already run directly as documented above;
the combined two-destination command is provided for keeper reruns after review.

## Live missing-footer false hold: bounded final finding

The keeper supplied read-only private snapshots from codingmachine:
`/tmp/imkr-mail-diagnostic-20261104-151541/`. Copies were examined locally in
`/tmp/imkr-submit-1343/live-evidence/`; no originals were typed into or changed,
and no production queue was resent. Private transcripts are not committed.

Exact ANSI replay with imkr's own terminal parser establishes:

| Snapshot | Size | Cursor (1-based) | Before | After |
| --- | --- | --- | --- | --- |
| monitor-runtime | 21x48 | 19;3 | unknown, therefore held | empty |
| monitor-rollout | 21x48 | 19;3 | unknown, therefore held | empty |
| ai-brain-monitor | 85x230 | 83;3 | unknown, therefore held | nonempty, still held |

All three lack `? for shortcuts`. The two narrow captures have a DIM suggestion,
a blank penultimate row and a last-row model status. Requiring the optional
shortcut footer makes the otherwise recognizable empty composer unknown; this
is a concrete second cause, separate from unbracketed single-line submission.

The wide ANSI capture is newer than its plain-text screen capture. Its prompt
starts at row 79; rows 79–82 contain non-DIM multiline input. The cursor is on
an empty input row 83, row 84 is the separate blank separator and row 85 contains
the model status. Treating the *first* blank row as the separator incorrectly
excludes that cursor. The updated parser uses the actual penultimate separator
for the footerless layout, recognizes the nonempty draft, and **keeps it held**.
It must not release that wide queue just because an older text capture showed
an empty suggestion.

The bounded source change keeps the hint layout and also recognizes the
footerless last-row-status layout, with bracketed paste enabled, a blank
penultimate separator, the final composer marker and the cursor inside its
input region. Only a DIM suggestion at column 3 is treated as empty; a non-DIM
draft (even the exact suggestion words), collapsed paste, historical match,
dialog, unknown suffix or misplaced cursor remains held. It sends no extra
Enter, infers no Confirmed and changes no account/handoff code.

Evidence: `live-composer-before.log` and `live-composer-after-final.log` under
`/tmp/imkr-submit-1343/`. The temporary standalone parser probe source is retained
there as `composer_snapshot.rs`, not added as a production binary. Its only
output is geometry/state classification, not private transcript content.
A sanitized unit regression covers both dimensions, placeholder/draft identity,
collapsed paste, cursor movement, history, approvals and a multiline draft with
a trailing blank input row.

**Real TUI check scope:** native frontend 0.166 with a private managed 0.166 daemon
passed exact body/ACK turns, worker -> keeper -> worker, busy/idle, human draft,
real approval and pending mail across update, account-switch refusal, shell,
zero fake-notifier calls and no false Confirmed on laptop AND codingmachine at
21x48 and 85x230. These real TUI runs still displayed hints. They are regression
checks, not an end-to-end reproduction of the footerless layout. Attempts to
configure that layout did not remove the hint; explicit assertions rejected
them as footerless evidence. No more rendering configuration attempts were made
after the keeper asked to stop, and no normalized-stream E2E claim is made.
The missing-footer evidence is the actual live-capture parser replay above.
Live NO_COLOR was reported unset; no new NO_COLOR result is claimed.

Laptop logs and evidence:

- `footer-final-real-21x48.log`, `/tmp/imkr-submit-1343-alk5g738/`.
- `footer-final-real-85x230.log`, `/tmp/imkr-submit-1343-0boz4_ys/`.

Codingmachine logs are in `/tmp/imkr-submit-1343-acceptance.Mu41zI/`:

- `footer-real-21x48.log`, `/tmp/imkr-submit-1343-9386dej1/`.
- `footer-real-85x230.log`, `/tmp/imkr-submit-1343-6_1mk2nm/`.

`IMKR_HOSTNAME=imkr1339-test cargo test --bin imkr`: 317 passed, 1 ignored.
Clippy all targets passes. The guarded 21-message fake-agent regression remains
a separate supporting test, never a replacement for these real frontend checks.
The older `codex-submit-1339.tgz` package predates this footer fix and must not be
mistaken for this new source state. Existing production queues may contain stale
commands: release/reconciliation belongs to the keeper, not to this test suite.
#1339 remains open pending keeper review; nothing was deployed.

Final footer-fix artifact (native frontend 0.166 plus runtime 0.166):
`/tmp/imkr-submit-1343/codex-submit-1339-footer.tgz`, SHA-256
`ada956435b9655f8626f450fdfd2ccd51df9427972bad2dce734597d177eb972`.
This is a new archive; the previous reviewed archive was not overwritten.
Both the packaged runner and the SSH orchestrator now accept `--rows` and
`--cols`. For example, after review:

```sh
python3 tests/run_codex_submit_hosts.py \
  --bundle /tmp/imkr-submit-1343/codex-submit-1339-footer.tgz \
  --host laptop --host codingmachine --rows 21 --cols 48 \
  --output /tmp/imkr-1339-footer-review-results
```

A final native-0.166 managed run on laptop passed at 21x48 after the historical
hint-text guard was added: `footer-final-last-local.log`, evidence
`/tmp/imkr-submit-1343-770ykzs9/`. The exact final artifact then passed both
no-daemon and managed at 21x48 through the SSH package runner on codingmachine:
`footer-bundle-run.log` and `footer-bundle-codingmachine/summary.json`, exit 0.
Remote extraction `/tmp/imkr-1339-review.HbI0W8`; evidence directories
`/tmp/imkr-submit-1343-r0cgeigf/` and `/tmp/imkr-submit-1343-1zqahfum/`.
The 85x230 real frontend checks above preceded only the additional guard that
ignores historical shortcut text above the current composer; that guard is
covered by the final sanitized unit regression. No footerless real-TUI E2E
result is claimed. The existing guarded 21-message test also passed with zero
extra Enter and zero notifier calls (`footer-mail-stall.log`).
