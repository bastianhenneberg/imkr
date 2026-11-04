# Account selector regression check (#1333)

Checked 2026-11-04 on base `a86e823`, in the `imkr--imkr-account-bug` worktree.

## Behavior

Enter or an account number in `Ctrl+B s c` restarts the focused Claude/Codex
agent in the selected account. The same tool resumes; another tool starts a new
conversation with its configured `agent_commands` command. `u` still changes
accounts for future shells. The CLI reply distinguishes same/new conversations.

Once the old foreground process has ended, clear its session, reported state,
failure, transcript/model, submitted prompts and mail-hook flag. The new process
must establish its own hooks. Keep the existing unsent-input checks and return
hook-fetched mail to the mailbox. Refuse overlapping switches, report PTY write
errors, and watch for the requested tool to return after launching it.

## Automated checks

- `IMKR_HOSTNAME=account-unit-proof IMKR_CONFIG=/tmp/imkr-account-proof/config.toml cargo test --offline`:
  310 passed, 1 ignored.
- `cargo clippy --offline --all-targets -- -D warnings`: passed.
- `cargo fmt --check`, `git diff --check`: passed.
- `IMKR_WRITE_DOCS=1 cargo test --offline docs_`: 2 passed.
- `python3 docs/build.py`: passed. Kept generated agents/keys pages only;
  unrelated diagrams vary with local font metrics.

The suite needs Unix socket/PTY permissions outside the Codex filesystem sandbox.
Without an isolated hostname, an existing address test assumes `laptop` is remote,
which fails on this machine named `laptop` (309 passed, 1 failed, 1 ignored).

## Isolated CLI and TUI check

Evidence directory: `/tmp/imkr-account-proof/`. The test uses its own tmux server
(`-L imkr-account-proof`), socket, HOME, XDG runtime/config/state/cache directories,
plain bash shell, account store, and Python executables named `claude` and `codex`.
No real agent, login, configuration or subscription is used. The test shuts down
its imkr and tmux servers in `finally`.

`check.py` drives the actual TUI through tmux and saves the account menu and result
screens for each selection. It checks process arguments and account environment
in `launches.jsonl`, incoming text in `inputs.jsonl`, request results in `cli.log`,
and cleared metadata in `after-switch-explain.json` and `after-switch-list.json`.

Scenarios:

- Claude → Codex: configured `--model 'test model'`, selected `CODEX_HOME`, no
  foreign resume ID despite the old Claude session having no saved transcript.
- Codex → Claude: configured `--model 'other model'`, selected
  `CLAUDE_CONFIG_DIR`, fresh conversation.
- Codex → Codex: `resume codex-saved`; Claude → Claude: `--resume claude-saved`.
- Same-agent missing conversation: refused before ending the old process.
- Human draft entered through the TUI: cross-agent switch refused.
- A second switch during startup: refused with explicit feedback.
- Mail fetched by Claude's Stop hook but not acknowledged: returned on restart,
  then received by Codex through typed delivery. The new agent has no inherited
  mail hook, session, reported state or transcript.
- Missing target executable: visible “agent did not come back” feedback.

`result.log` records the final verdict. Fixtures acknowledge the full delivered
prompt, including the imkr sender prefix; a body-only acknowledgement correctly
leaves the message unconfirmed and can block a later restart.

Limitations: fake agents prove imkr's CLI/TUI, process lifecycle, arguments,
environment and mail routing. No real Claude/Codex network calls or paid turns
were made. Live account switching with a real subscription was intentionally not
attempted. Nothing was pushed or deployed.
