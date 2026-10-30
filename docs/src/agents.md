# Agents

Every pane running a coding agent shows what it is doing: `● working`, `◆ blocked` (it waits
for you), `✦ done` (finished while you were elsewhere), `✓ idle` or `✗ failed` (its turn ended
in an error: a used up limit, a refused login, an API error; magenta, with the reason). A window
tab turns red when an agent there waits or failed, the bell rings, and `Ctrl+B a` takes you to it.
An agent counts as failed when its `StopFailure` hook says so, or, without hooks (Codex, or a
Claude Code before 2.1.78, which `imkr hooks --install` leaves the hook out for), when it
is quiet for 5 seconds with an error like „usage limit“ or „API Error“ at the bottom of its screen. You also get a desktop notification when an agent waits or finishes
while you look elsewhere or are detached: through `notify-send`, or as OSC 9 for terminals like
Ghostty and WezTerm, or not at all (setting `notifications = "system" | "terminal" | "off"`).

imkr recognises claude, codex, opencode, aider, gemini, crush, goose, amp, cursor-agent, qwen,
kilo and pi. It guesses from their output and from questions on the screen. Other agents and
their questions go into the config (`extra_agents = ["my-agent"]`,
`question_phrases = ["Proceed now?"]`), and `imkr explain --pane N` says why a pane counts as
the agent it is, in its state, or as none. For exact states, let the agent report them with
hooks. For Claude Code:

```bash
imkr hooks --install    # puts them into ~/.claude, on every machine you run agents on
imkr hooks              # or just prints them for ~/.claude/settings.json
```

`--install` writes `~/.claude/hooks/imkr-agent-state.sh`, adds its hooks to
`~/.claude/settings.json` next to your other hooks (backed up as `settings.json.bak-imkr`, imkr's
own from before replaced), and works where `~/.cargo/bin` is only on interactive shells' PATH.
Waiting is reported both by permission requests and, for agents running with permissions
skipped, by the notification that a question waits. Agents started after it report their state.
Where pi is set up (`~/.pi/agent`), `--install` also puts an extension into its extensions that
reports pi's state the same way: working while it runs, done after its turn, waiting when an
error stays after pi's own retries.

The hooks call `imkr status working|blocked|idle|clear`, which knows its pane from
`$IMKR_PANE` and does nothing outside imkr. It reads the hook's JSON from stdin: a subagent
finishing does not mark the agent done, and the session id is kept to resume the agent later. The
transcript's path comes along too: imkr reads the model and context of the agent's last answer
from its end and shows them as `opus 5.5 · 300k` in `Ctrl+B A`, the inbox, `imkr agents` and,
for the focused agent, in the sidebar.

What an agent says it is doing shows under it in the sidebar, after it in `Ctrl+B A` and in
`imkr agents`, on every machine: the title it gives its terminal. Claude Code keeps a short
summary of its task there; spinners in front are cut off, and a title that only names the
program is left out.

Scripts and agents can drive imkr too:

```bash
imkr list [--json]                        # panes with program, state and directory
imkr send --pane 3 "run the tests"        # type into a pane, then Enter
imkr new --window 2 -- claude --resume    # start a command in a new pane
imkr new --float                          # a floating shell
imkr -s api new --start --window 0 -- claude
                                          # a session of its own for an agent
imkr rename "tests"                       # name the pane this runs in
imkr wait --pane 3 --until ready          # wait until an agent is idle or done
imkr read --pane 3 --lines 20             # what it wrote last
imkr rename --window 2 api                # name a window
imkr notify "tests are green"             # a notification, titled after this pane
imkr notify --title build "done in 42 s"  # with a title of its own
```

`notify` goes out the way the settings say (a desktop notification through `notify-send`,
OSC 9 to the terminal, or none) and shows up in the control bar as well.

`--start` starts the session first if it does not run yet, which is how an agent gives another
one a session of its own. Together with `--window 0` the command then takes the place of the
empty shell a new session starts with: one window, one pane, the agent in it, reachable as
`session/1`. In a session that already runs, `--window 0` opens a window of its own as before.

### Sessions started for an agent

An agent that opens a session for each of its workers leaves a mark on them, so that a list of
twelve sessions still says which belongs to whom:

```bash
imkr -s api new --start --window 0 --by laptop:imker/0 -- claude
imkr agents --all --by laptop:imker/0     # only the agents in sessions started for that pane
```

Typed **in** a pane, `--start` sets it by itself: the session is marked as started for the pane
the command ran in (`machine:session/pane`), and `--by` is only needed to name another one. A
`new` in a session that already runs marks nothing — the session somebody works in is not its
own agent's errand.

The mark belongs to the session and lives in its saved layout: it survives a restart, a crash,
`imkr revive` and `imkr update`. Everywhere sessions are listed — the sidebar, `Ctrl+B S`,
`imkr ls`, `imkr ls --json` (`by`) — a session started for another sits under it with an arrow:

```text
Sessions
  imker
  ↳ api                 ❶
  ↳ web                 ❷
  nightly
    by laptop:imker/0 — gone
```

Only within one machine, because those lists group by machine; a session whose orchestrator
sits on another one keeps its place and names it instead. `Ctrl+B O` shows the whole tree
across machines, orchestrator above and its sessions below with their state.

**When the orchestrator is gone** — its pane closed, its session ended — the session says
`— gone` and goes back to the top level. It keeps running: the mark says who asked for the
work, not who is allowed to live. A machine that cannot be reached says nothing either way,
so a laptop that is off does not make its workers look orphaned.

### Several accounts, and the freest one when one is used up

One machine, several Claude Code and Codex subscriptions, and providers Claude Code reaches by
address and key (OpenRouter, a local Ollama). Each account is an environment the agent starts
with: a subscription gets a directory of its own (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) holding only
its login, and everything else links to `~/.claude` or `~/.codex`, settings, hooks, skills, MCP
servers and the conversations. So a conversation goes on in another account where it stopped.
`~/.claude` itself is the account `standard`.

```bash
imkr account add claude privat          # logs it in: Claude Code starts in it, /login, /exit
imkr account add codex work             # `codex login` in it
echo "$KEY" | imkr account add claude router --url https://openrouter.ai/api \
                                             --model qwen/qwen3-coder --token-stdin
imkr account add claude local --url http://localhost:11434 --token ollama --model qwen3-coder
imkr account                            # every account, in the order to fall back in
imkr account use privat                 # new shells of this session start in it
imkr account switch privat --pane 3     # the agent in pane 3 goes on in it, now
imkr account run privat -- claude       # one command in it
imkr account sync                       # share what is new in ~/.claude, MCP servers again
imkr account default henneberg          # new shells on this machine start in it
```

Accounts show by the subscription they are logged in to, `(standard)` marks the one new shells
start in (`imkr account default`, else `~/.claude`), and commands take either name:
`imkr account use stelljes` finds `standard` when that is stelljes' login. A pane keeps the account
it started in, also when the default changes later. `imkr settings sync` brings the list of accounts to the other
machines, without logins: log in there once with `imkr account login NAME`.

Every agent's frame says what it runs on, after its state: `claude ● working · henneberg · opus 5.5`
(the subscription by its address before the @, then the model), or `· Ollama · qwen3-coder` for a
provider.

`Ctrl+B k` starts an agent and asks which account it runs in: every account of Claude Code and
Codex (those installed), logged out ones marked, the default selected; Enter or its number starts
it. With only one account logged in, it starts right away. It starts in the focused pane, or in a
new pane beside it when a program runs there; `p` in the list switches to a new window instead
(until imkr ends). `new_agent_in = "window"` in the config makes the new window the default. What it starts
is `[agent_commands]` in the config, else the agent's name:

```toml
[agent_commands]
claude = "claude --dangerously-skip-permissions --chrome"
```

The same command starts wherever an agent is written as a placeholder: `<claude>` in a session
template, in `imkr new -- '<claude>' …` and `imkr worktree NAME -- '<codex>'`, and in
`imkr fleet --agent claude`. `<agent>` stands for `default_agent` (`claude` unless the config
says otherwise), and `<claude@work>` starts it in the account `claude:work` right away instead of
asking. Each machine fills in its own `[agent_commands]`, and a template saved from a running
session writes its agents this way.

`Ctrl+B s c` lists them: Enter puts the focused agent into the chosen account, `u` new shells of
this session, `d` makes it the standard of this machine (like `imkr account default`), handy
when an account is used up for the week: new shells in every session start in the other one.
Each account also shows how full its week is where that was measured in the last ten minutes,
so the list says what it costs to pick one. Switching ends the agent and starts it again in the same conversation, typed into
its shell as `imkr account run claude:privat -- claude --resume <id>`, so no key shows on the
screen; the session id comes from the hooks.

When an agent says its account is used up ("5-hour limit reached ∙ resets 7pm", Codex: "You've
hit your usage limit"), imkr notes that account as used up until then and, as *Account used up*
in the settings says, tells you which account to go on in (`ask`, the default), moves the agent
there by itself (`auto`), or does nothing (`off`). `Ctrl+B s U` steps those three without the
settings menu. Accounts and keys live in `~/.local/state/imkr/accounts/` (0600).

Which account that is, it measures: of those logged in and not used up, the one with most of
its **week** left. The week is the scarce window — it comes back in days, the five hours in
hours — so the week decides and the five hours only break a tie. An account that has actually
hit a limit is no candidate at all, it is noted as used up until it resets; so a half-spent
five-hour window says nothing worse than *this one may stop again later today*.

The numbers are the ones below, asked of Anthropic per account and kept for two minutes, so a
decision is quick without being stale. Asking takes a moment, and an agent at its limit has
stopped anyway; after 25 seconds without an answer — a machine offline, say — the next account
in the list takes over, as it always did, and so it does when there are no numbers at all. The
message names both accounts: *account stelljes (88 % week) used up until today 19:00: goes on
in department (12 % week)*.

`Ctrl+B s C` does the same on demand: the agent in the focused pane moves to the freest account
right away, in the same conversation, whether or not anything is used up. It is the key the
`ask` message points at — in *that* pane, which the message names in front, since the key goes
by the focus — and the one to press when an account is nearly full and you would rather not
wait for the limit.

Limits belong to the subscription, not to the account's name. `imkr account` shows who each one
is logged in as, and `add` and `login` warn when an account is logged in to a subscription that
has one already; nothing is removed for it. One used up counts for every account of the same
subscription, so falling back never lands in the same limit.

A login Claude Code has emptied, because the server refused its refresh token, shows as
*logged out*, also for `~/.claude` (`imkr account login claude:standard` logs it in again). Such
an account is no subscription: it is not counted twice, not fallen back to, and new shells do
not start in it: when the default is logged out, they start in the first account that is
logged in. A pane whose account is logged out starts in the default when it comes back.

One person is one account. `imkr account add claude stelljes` again logs `stelljes` in anew
instead of failing, and so does adding a name a logged out account was logged in as. When the
browser login of a new account turns out to be a person another account was or is logged in as,
the login moves into that account and the new one goes again (unless agents run in it already).
A logged out account whose person is logged in elsewhere, such as an old `~/.claude`, is left
out of the lists.

A Claude login lasts about 30 days from `/login`; Claude Code renewing its key does not move
that end. So imkr says it ahead: three days before, `imkr account` and the account lists show
*login ends …*, and one imkr on the machine sends a notification once, as it does when a login
is logged out, each with the `imkr account login` that logs it in again.

#### How much each account has used

`Ctrl+B s u` shows every account with its plan and limit windows, a bar each, yellow from 80 %
and red from 95 %, with when each resets; `imkr account usage` prints the same, `--json` too.
Every such overview also keeps its numbers for the account list and for falling back, where
they are what decides which account is freest.

```text
Claude
  henneberg (standard)  Max 20x   5h          █░░░░░░░░░  14 %  resets today 21:00
                                  week        ████░░░░░░  40 %  resets 22.10. 05:00
  department            Max 20x   5h          ░░░░░░░░░░   0 %  resets today 21:59
Codex
  henneberg (standard)            week        ░░░░░░░░░░   0 %  resets 21.10. 13:19
Tokens
  Claude   5h    1.8 M   7 days   21.7 M
```

Claude's numbers are those of Claude Code's `/usage`, asked of Anthropic with the account's
login. Its access key lasts some hours and Claude Code renews it with the login (which lasts
weeks) when it runs. For an account not used for a while, the overview has Claude Code renew it:
one word from Haiku, without history, tools or MCP servers, a tiny request. imkr never renews it
itself, as that replaces the login, and does not while a Claude runs in that account. Codex notes its limits in its own logs
with every answer: they are as of its last run. Tokens count per agent, not per account, as all
accounts share the conversations; cache reads are left out. A local Ollama has no limit.

### Agents talking to each other

Agents in imkr can find, read and message each other, on this machine and on the others:

```bash
imkr agents --all                                  # every agent with its address
imkr msg server:api/claude "The API returns 409 on duplicates now, please update the client."
imkr msg --wait api/3 "Which port does the dev server use?"   # waits, prints its screen
imkr msg server:api/claude < handover.md                       # a long text from stdin
```

An address is `machine:session/pane`; in the own session a pane number, name or program is
enough. imkr types the message in once the agent is idle, one at a time, and never into a
question it asks, which stays yours. Claude Code gets it while it works too: it shows it as a
queued message and reads it at its next step, so a long task does not hold it back. Each message says who sent it and how to
answer. `imkr skill` prints the guide for agents, `imkr skill --install` puts it into Claude
Code's skills, so the agents use it on their own. `imkr read server:api/3` shows another
agent's screen, on any machine.

Answers find their way back without every machine reaching every other: a server needs no ssh
to your laptop. A message for a machine out of reach waits in the sender's outbox (`imkr
outbox`), and the machine it is for picks it up when it next asks that one, every 5 seconds
while its imkr runs, so a laptop that was offline gets it later.

### Who works for whom

When an agent hands work to others with `imkr msg`, `Ctrl+B O` shows it: each pane that sent
messages leads as an orchestrator, its workers below, on any machine, with their state, the last
message each way and how long ago. The box below shows the selected worker's screen and the
latest messages; Enter goes there. A pane that only got messages shows who it works for.

imkr learns this from the messages themselves: the session that delivers one notes the sender,
and `imkr msg` tells its own session where it sent one. The newest 400 are kept, also across
`imkr update`. Sessions started with `--by` show there as well, under the pane they work for,
even before a single message has gone out.

### A keeper and its bees

A long session that has been compressed three times works worse than a fresh one with a good
briefing. So one session only steers — the **keeper** — and every task gets a session of its
own — a **bee** — that starts with a fresh context and its briefing.

```bash
imkr keeper                      # session `keeper` (`keeper-2` … when taken), goes there
imkr keeper --host server        # the same on another machine
```

The keeper's agent (`default_agent`) starts with the skill `keeper` and its folder,
`~/.local/state/imkr/keeper/NAME/` with `briefings/` and `keeper-stand.md` for its notes. What a
keeper does is the skill's business, not imkr's: it comes from AI Brain
(`ai-brain-harness select --dazu skill/keeper`, then `ai-brain-harness sync --apply`), and
`imkr keeper` says so instead of starting a keeper that does not know its job.

A keeper's session is marked ⚑ (in the theme's accent) wherever sessions and agents are listed —
the sidebar, `Ctrl+B S`, `Ctrl+B A`, `imkr ls`, `imkr agents`, the phone page — and comes first on
its machine; its own bar and terminal title carry the mark too, so you know which window you talk
to. The mark is kept with the session, through restarts, `imkr update` and `imkr revive`.
`imkr keeper --mark [SESSION]` marks one afterwards (this one without a name), `--unmark` takes
it off. Bees need no mark of their own: they sit under their keeper, as every session started
for another does.

From the keeper, one command starts a bee:

```bash
imkr spawn api-fix --brief briefings/api-fix.md --dir ~/src/api --worktree --task 9133
imkr spawn api-fix --brief briefings/api-fix.md --host server --dir ~/src/api
```

It starts session `api-fix` with the default agent (with `--worktree` in a new git worktree
`api-fix` of the repository, as `imkr worktree` makes them), waits until the agent is ready and
sends it the briefing as its first message, from the pane it was called in, so answers come
back with `imkr msg`. On another machine the briefing travels along on stdin and is kept there,
readable only by you. It prints the bee's address (`--json` for scripts) and notes the bee in
the keeper's `bees.json`: `Ctrl+B O` shows each bee's task, and its briefing above its screen.

An agent that asks something before it starts — Claude Code's "trust this folder?" in a new
worktree — is left asking: nothing is answered for you. `imkr spawn` says where it waits and
what to send once you answered, and ends with an error.

### Other people's imkrs

Colleagues with their own imkr (their machine, their Claude account) can message your agents,
and you theirs, without ssh to each other's machines. Pair once, both sides turn on the web view
(`web = true`) in a VPN such as NetBird or Tailscale:

```bash
imkr connect invite                                 # you: prints a code, good for 10 minutes
imkr connect join http://100.97.1.2:4657/ ABCD-EFGH # they: what invite printed
imkr msg martin:api/claude "Can you look at the login bug?"
```

Without a common network, AI Brain carries the messages: it knows the people and who may write
to whom, imkr goes through `ai-brain-harness` (connected with `ai-brain-harness connect`) and signs in
nowhere itself.

```bash
imkr connect brain martin                           # both sides, each with the other's name
imkr msg martin:api/claude "Can you look at the login bug?"
```

Every imkr with such a contact asks AI Brain every 30 seconds while it runs (`imkr connect fetch`
does it now).

A contact's name is the machine part of an address. Their messages wait until you accept them:
`imkr connect inbox`, `accept ID`, `decline ID`, or `Ctrl+B i`. `imkr connect trust NAME` types
them in right away like your own, `untrust` asks again, `rm NAME` ends it. A contact can only
send messages: no screens, no keys, no shell. Answers go back the same way, the message says
how.

### MCP

`imkr mcp` serves the same as tools over MCP on stdin/stdout: `imkr_agents`, `imkr_panes`,
`imkr_read`, `imkr_send`, `imkr_message`, `imkr_run`, `imkr_wait` and `imkr_new_pane`. Each runs
imkr's own command, so addresses on other machines work as on the command line, and text never
goes through a shell. For Claude Code:

```bash
claude mcp add imkr -- imkr mcp
```

Started in an imkr pane, the server knows its session and pane by itself.

### tmux for agent teams

Programs that drive tmux, like Claude Code's agent teams, can open imkr panes instead. Turn on
*tmux for agent teams* in the settings: panes started after that find imkr as `tmux` on their
PATH, with `$TMUX` and `$TMUX_PANE` set, and `split-window`, `new-window`, `send-keys`,
`capture-pane`, `kill-pane`, `display-message`, `list-panes`, `list-windows`, `select-pane`
and `rename-window` work on imkr panes (`%3` is pane 3); layout and option commands are
accepted and do nothing, `kill-session` and `kill-server` are refused. It is off by default,
since programs that see `$TMUX` act as if inside tmux. Tested with scripts that use these
commands, not yet with every program that drives tmux.

### Running commands in a pane

With shell integration (OSC 133), imkr knows where a command's output starts and ends and how it
ended. Many prompts send it already; otherwise add one line to the shell's rc file:

```bash
eval "$(imkr shell-integration zsh)"   # or bash; does nothing outside imkr
```

Then a script or an agent can use a shell in another pane like its own:

```bash
imkr run --pane 3 -- cargo test   # types it in, waits, prints the output, exits with its code
imkr wait --pane 3                # waits for the command running there (--timeout S)
```

One word after `--` is typed as the command line it is (`imkr run --pane 3 -- 'make && make
test'`); several are the words of one command and each is quoted, as a program would get them:
`-- echo "a b;c"` prints `a b;c`.

### Showing a pane to a program

`imkr tap` shows a pane to a program instead of a terminal, for example to bring it into a web
page (AI Brain's harness uses it). It takes an address as `imkr read` does and speaks JSON lines,
bytes as base64:

```bash
imkr tap api/claude
# out: {"screen": "…", "rows": 40, "cols": 120}   what draws the pane now, first
#      {"out": "…"}                               what it printed within 150 ms (--every MS)
#      {"gap": 412000}                            more than 64 KB came at once (--max BYTES):
#                                                 left out, a fresh "screen" follows
#      {"exit": true}                             the pane ended
# in:  {"text": "ls\r"} or {"input": "G1tB"}      typed into the pane
#      {"screen": true}                           a fresh "screen", e.g. after a page reload
```

It only watches: the pane keeps its size. When that changes, a fresh `screen` comes with the new
size. A `screen` stays within `--max` too, by leaving out the oldest history; only a screen
without history that is still larger goes on as `out`.

### Where the user sits

An agent on another machine sometimes wants to show you something on your own screen, a browser
for example. `imkr operator` says which machine you sit at, as the terminal attached to the
session tells it:

```bash
imkr operator          # laptop
imkr operator --json   # {"machine": "laptop", "via": "codingmachine", "here": false}
```

`via` is the name your machine reaches this one by, e.g. for a tunnel back. If nobody is attached
to the agent's session, it asks the other sessions on the machine; with nobody anywhere it fails.
AI Brain's `agent-tunnel` uses it to open its browser where you are.

### Copying files

`imkr cp` copies like scp, and also to and from the directory a pane works in, here or in
another session or on another machine:

```bash
imkr cp notes.md @3                     # into pane 3's directory
imkr cp report.pdf @server:api/claude   # into the directory of claude in session api on server
imkr cp @api/claude::target/out.log .   # a file below that pane's directory, here
```

### Events

`imkr subscribe` prints what changes in the sessions on this machine, one JSON object per line,
for scripts, status bars and agents that wait for each other (`-s NAME` watches one session,
`--snapshot` lists every pane first):

```json
{"event":"state","session":"api","pane":1,"program":"claude","window":2,"from":"working","state":"blocked","time":1791843836}
```

Events are `state` (an agent's state changed, `null` without one), `pane-opened`, `pane-closed`
and `pane-moved` (to another window, with `from_window`).
