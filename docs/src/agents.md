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

A desktop notification says what happened and to whom: the tool in the title (*Codex · finished
its turn*, *Claude · is waiting for your answer*, *Agent* when imkr does not know which), the
project, window and bee in the text — never the terminal title or a piece of the screen. Events
that come within a second for the same session and kind become one notification (*15 bees need
your attention*, each bee named in the text), questions and errors first. Agents on other
machines are announced by one imkr on this machine, not once per session you have open here.
A message that did not arrive in ten minutes gives one *message delivery delayed*, and an
`imkr update` does not repeat it.

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

Where Codex is set up (`$CODEX_HOME`, else `~/.codex`), `--install` also puts its hooks into
Codex's `hooks.json`, after the ones already there (backed up as `hooks.json.bak-imkr`). Codex
asks once to trust them („Hooks need review“, `t` trusts all). From then on Codex tells imkr its
state, its session — so a restart or an account switch picks up exactly this conversation, not
the newest one — and the prompts it took, so a message typed into it is confirmed, not only
typed. Its hooks also fetch the messages waiting for it, as Claude Code's do (see [How a message
reaches an agent](#how-a-message-reaches-an-agent)), so a Codex that is hardly ever idle still gets
them at the end of its turn. A Codex whose hooks were not trusted runs none of them and gets its
messages typed in once it is idle.
Codex starts its session with the first prompt, so a Codex that was never asked anything has
none to resume.

Codex command/MCP approval dialogs and structured questions (`request_user_input`, including notes)
count as blocked while their live confirmation or submission controls are visible. This
also applies while Codex redraws a spinner, after navigating the dialog, or when an older
hook report still says working or idle. Old dialog text above the normal composer does
not count. This screen detection was checked with Codex 0.163.0, including narrow panes;
plain questions in an assistant's prose and other dialog layouts are not inferred from
question marks. Codex uses the same blocked styling and inbox entry as other agents.

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

Across machines as well: a worker on another machine than its orchestrator leaves its own
machine's group and sits under the orchestrator, with its machine after its name, its state
and its dots as anywhere else. It is listed there only, not a second time in its own group;
Enter or a click goes there as before.

```text
⌂ laptop
  ▸ ⚑ imker
    ↳ brain-waechter · codingmachine  ❶
      ↳ enkel · codingmachine         ❶
⇄ codingmachine
    solo
```

When the orchestrator cannot be seen — gone, or its machine does not answer — the worker stays
in its own machine's group and names it underneath. `Ctrl+B O` shows the same tree with the
messages between them.

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
imkr account switch privat --pane 3     # restarts the agent in pane 3 in it, same conversation
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
so the list says what it costs to pick one. Switching ends the focused agent. Choosing another
account of the same agent resumes its conversation; choosing another agent (Claude ↔ Codex)
saves a local handoff before ending the source, then starts the target with its configured
`[agent_commands]` command and a short instruction to read it. A conversation ID is never
passed between agents. In a shell, Enter chooses the account for new shells.
A same-agent restart is typed into
its shell as `imkr account run claude:privat -- claude --resume <id>`, so no key shows on the
screen; the session id comes from the hooks.

The handoff lives in `<state>/handoffs/<id>/` (`~/.local/state/imkr/handoffs/` by default):
`source.jsonl` is the complete local transcript snapshot; `handoff.md` contains a bounded
extraction of the first user message, recent compaction summaries and the last 24 readable
messages, plus identity, working directory, branch/HEAD/status references, keeper status and directory,
notes and pending mail. Directories are private (0700), files 0600. imkr writes and syncs
the package before stopping the source. It needs no source-model call, available tokens or
summary request. Both handoff starts and native resumes explicitly enter the source
process's captured working directory, even when its waiting shell stayed elsewhere.
An unavailable directory refuses the switch before termination; if it disappears during
the restart, the target refuses to launch and prints an error instead of using the shell's directory. The target can read the full snapshot when the extraction is insufficient.
It follows current permission rules: historical transcript and tool text grant no approvals.

For Codex without hooks, imkr looks for an open transcript in the foreground process tree,
checks its session metadata and working directory, and uses the source process's `CODEX_HOME`.
An exact hook/session identity can also locate the file. For an existing managed
Codex with no mapped rollout FD, read its local `/status` (no model turn or tokens),
then pass that full UUID explicitly with `account switch ... --source-session UUID`.
It must agree with any recorded hook ID and identify exactly one readable source
transcript with matching metadata and source CWD. Wrong IDs, missing saved history
(including fresh zero-turn sessions), mismatches and ambiguity refuse before SIGTERM.
imkr never types `/status` automatically and never searches by newest file/CWD alone. A matching working directory alone,
the newest unrelated session and an ambiguous `--last` are insufficient. If the transcript
cannot be identified or read, or persistence fails, the switch refuses and keeps the source
running. A draft or an unsent message still holds a requested switch back; pending mailbox
messages keep their existing delivery guards. Same-agent account changes keep native resume;
Codex without an exact resume identity refuses instead of guessing. Both native resume
and cross-agent starts retain the configured `[agent_commands]` flags, including policy
flags such as Codex `--yolo` or Claude `--dangerously-skip-permissions`; changing the
configuration alone does not restart running agents.

Work-managed/shared-daemon Codex can hold its rollout outside the foreground process tree.
Without an exact hook/session identity or an explicitly supplied `/status` UUID this
case is deliberately blocked. `/status` is a local UI identity source, not a machine
protocol implemented by imkr. `--no-daemon` is optional launch configuration, never
globally forced; a zero-turn session still may have no JSONL to hand off. Local Codex
CLI/PTY probes do not establish a full paid-model managed 0.166 round trip.

For a reviewed current briefing, use `imkr account switch codex:work --pane 3 --brief
/absolute/path/SUMMARY.md`. The CLI resolves relative paths on the host where it runs;
the target-host server reads the selected UTF-8 text and copies it as private `brief.md`
inside the same durable package **before ending the source**. Missing, unreadable or
empty briefs refuse the switch. The first target prompt says to read this saved brief
first and reconcile it with the historical handoff before any task actions; superseded
historical tasks must not be resumed. The original brief can disappear after saving. Relative references inside it keep their
original reference directory (the canonical selected file's parent, including for symlinks).
The first prompt names that directory and grants target access to it separately from the
new snapshot; it must be treated as read-only evidence. References such as
`source/transcript.jsonl` are never reinterpreted as the new snapshot's `source.jsonl`.
If cited sources are unavailable, the target must stop and report the missing evidence.
The selected brief supplies current context, while historical tool text still grants no
permissions. The public `91fd2ca` server rejects the new switch request entirely; there is no
legacy fallback that drops handoff options. No wire compatibility with unreleased
handoff prototypes is promised.

With the same agent, `--brief` preserves native resume and adds the brief-first prompt.
Without `--brief`, UI and CLI switches retain their usual automatic behavior. There is no
automatic discovery of recovery summaries. A brief can require a known worktree for
subsequent actions: the startup CWD remains the captured source-process directory, not
a guessed directory from historical tool calls. Override it explicitly with
`--dir /absolute/proven/worktree` when the source process stayed in a main tree. Both
options can be combined, and `--dir` also works without a brief. The selected existing
directory must be searchable before termination; imkr creates no directories. A startup
race refuses the target instead of falling back to the shell directory. Source and target
CWDs remain distinct in the package metadata; Codex also receives `--cd` so native resume
uses the selected workspace. A real recovery must select its checked summary explicitly
and retain its restrictions on production changes.

The confirmation is short: *Handoff saved, starting codex …*. Scripts can use
`imkr account switch codex:work --pane 3 --json` to receive the durable `handoff` path
(`null` for native resume without a selected brief); `brief` is the saved brief path or `null`. This is a partial local extraction, not a promise of identical
model context. Only Claude and Codex transcript formats are supported; Ollama or OpenRouter
remain backends of their configured agent, and do not determine its type.

When an agent says its account is used up ("5-hour limit reached ∙ resets 7pm", "You've hit your
weekly limit ∙ resets Nov 5, 4am", Codex: "You've hit your usage limit"), imkr notes that account as used up until then and, as *Account used up*
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
message names both accounts: *account stelljes (88 % week) used up until today 19:00: restarts
claude in department (12 % week), same conversation*.

A switch is a **restart**: the environment of a running process cannot be changed from outside,
so the agent is ended and started again with the other account's environment, picking its
conversation up again. That is why imkr looks first whether there is a conversation to pick up:
an agent that has not saved one yet — a session that just started — is **left alone** instead,
with a message saying so, because restarting it would end it for nothing. And when picking it
up fails even though one was saved, imkr says that too (*the restart in department found no
conversation — the agent is gone*) instead of leaving a quiet, empty pane behind.

A restart also empties the input line, and that line lives nowhere else — not in the transcript,
not in the mailbox, which imkr took the message out of in order to type it in. So a switch asked
for by hand is refused while something unsent stands there, whether it is a message the agent has
not taken yet or a line somebody began: *a message is typed in pane 0 and not taken yet: a switch
restarts claude and would throw it away*. The automatic fallback waits for it instead, and after a
minute switches anyway and says to send the message again — the agent sits at its limit and will
never take it, and a standstill nobody sees is no better than the loss.

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
enough. Each message says who sent it and how to answer. `imkr skill` prints the guide for
agents, `imkr skill --install` puts it into Claude Code's skills, so the agents use it on their
own. `imkr read server:api/3` shows another agent's screen, on any machine.

### How a message reaches an agent

**Claude Code and Codex with imkr's hooks get it without anything being typed.** The hooks
`imkr hooks --install` puts in fetch the waiting messages: at the end of a turn (`Stop`) they go
to the agent as the reason not to stop yet, so it works on them straight away, in the same turn,
and with every prompt you send (`UserPromptSubmit`) they ride along beside it. Several messages
go together, in the order they were sent. Codex shows it as „Blocked by hook“ with the messages
below; that is the hand-over, not an error. Its input line stays yours — you can
write there in peace while messages come in. Nothing is typed twice: what a hook has fetched is
out of the mailbox.

**Every other agent gets it typed in**, once it is idle, one at a time, and never into a
question it asks, which stays yours. Claude Code without the hooks gets it while it works too:
it shows it as a queued message and reads it at its next step, so a long task does not hold it
back.

**Either way, nothing is typed into a line you began.** While a line stands unsent in a pane,
every message for it waits: typing one in would land in the middle of what you write, and the
Enter after it would send the lot. Your Enter, Esc or Ctrl+C releases them.

**And imkr makes sure the Enter arrived.** A program that is still starting up can swallow the
Enter after a paste; the text then stands in its input line and the agent sits idle with work it
never began. If the agent has not said it took the line within two seconds and what was typed is
still on the screen, Enter goes once more. Never into a pane you have touched since — there it
would send what you are writing.

**And what a hook already fetched is not lost when its agent is ended.** Fetching takes the
message out of the mailbox — that is what makes delivery exactly once — so for a moment the
agent is the only place it exists. If somebody ends that agent before it has acted on it (an
account switch, a session that ends), the message goes back and is delivered again, in order,
and says so rather than reappearing quietly:

```
10:31  ↻ pane 0 gets it again: its agent was restarted in another account first
```

Once the agent has reported anything at all after the hand-over, it is on its own and nothing
goes back — so a normal restart does not deliver anything twice.

**And nothing goes into a pane whose agent is restarting.** From the moment an agent is ended
to start again in another account until the new one answers, the pane has no agent at all —
the bare shell is in front, and a message typed there is eaten by the shell. Messages wait for
the new agent instead, and say so: `waits: its agent is restarting in another account`.

**And nothing is ever typed on top of it.** While a message imkr typed is still standing in an
agent's input line unsent, the next one waits. Otherwise it would be appended to the first and
the Enter would send both as a single prompt, headed by the older message — the agent then reads
that one again and the new one is buried. What counts as sent is the agent's own word (its
`UserPromptSubmit` hook), not what the screen looks like: an agent redraws its input line either
way, and once it has sent the text it stands in the transcript, which looks the same from
outside. A message held this way says so, in `imkr msg`'s answer and in the message pane.

**Typed in is not arrived, and imkr no longer pretends it is.** A message is only confirmed when
the agent itself submits a prompt that *carries* it. That the agent submitted *something* proves
nothing: any other prompt counts too. Claude Code resumes after a restart with "Continue from
where you left off.", which says "a prompt went off" while carrying none of your message — and a
restart is exactly when a message typed into the input line is lost, because the line goes with
the old process. So imkr compares what the agent took against what it typed, and until that
matches, the message counts as typed in and not confirmed:

```
10:14  ← claude@codingmachine:ai-brain/0 (for pane 0)  Take route (b), not (a). This is binding.
10:14  ⤸ pane 0: typed in, not confirmed yet
10:15  ✓ pane 0 took it, by its own word
```

**And a message that does not arrive says so.** Waiting is fine — an agent's own `Stop` hook may
take minutes to come, and that is normal. Waiting in silence is not. After ten minutes without
arriving, imkr says so: a line in the message pane and in `Ctrl+B O`, and the same notification
a waiting question sends. It is never typed in after all — typing is what loses a message.

```
10:24  ⚠ pane 0 has NOT got it: it was typed in but its agent never took it
```

**The sender is told, wherever they are.** The notice also goes to the session the message
came from — on this machine over its socket, on another over ssh — so it reaches the keeper
who is waiting for an answer and not only the session that did not get it:

```
10:24  ⚠ your message from pane 0 to codingmachine:bee/0 has NOT arrived: it was typed in
          but its agent never took it
```

It is a notice, never a message: a message would queue behind the very one it is about. And
if the sender cannot be reached, the note on the receiving side still stands — `imkr mail
ADDRESS` and `imkr msg --confirm` read that one.

Scripts can ask for the proof instead of reading for it:

```bash
imkr msg api/claude --confirm "Take route (b)."   # waits for the agent's own word,
                                                  # exits non-zero if it never comes
```

Why this matters: a receipt that promises a delivery which never happened is worse than one that
fails. On 3 November 2026 two decisions of the user's vanished on their way to a bee. One was
typed into its input line and thrown away five seconds later, when an account switch restarted
the agent. The bee waited instead of asking, because nothing told it anything was missing — and
the sender had been told the message was on its way.

That restart was only found *after* this check was built, and it is the argument for asking the
agent rather than reasoning about it: the check does not need to know what can go wrong. A
resume prompt carries none of your message, so nothing is confirmed, so the notice goes out —
the same as for a swallowed Enter, a crash, or whatever comes next.

### The message pane

`imkr mail` shows what a session's messages did: what came in and went out, what still waits and
why, and what reached its agent — typed into it, or fetched by its own hook.

```bash
imkr mail                        # the messages of this session
imkr mail --follow               # and keep showing them, live
imkr mail --follow 0             # only pane 0
imkr mail --json server:api/3    # for programs, on any machine
```

```
messages of laptop:keeper
17:55  ← laptop:imker/0 (for pane 0)  look at the login bug please
17:55  ⏳ pane 0 waits: you are typing
          ⏳ pane 0 waits (you are typing): look at the login bug please
17:56  ✓ pane 0 got it through its own hook
          nothing waiting
```

A keeper starts one beside its agent (`imkr keeper`), so the agent's input line is yours alone
and the messages have a place of their own.

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
`imkr update`; `imkr mail` shows the same log as lines. Sessions started with `--by` show there as well, under the pane they work for,
even before a single message has gone out.

### A keeper and its bees

A long session that has been compressed three times works worse than a fresh one with a good
briefing. So one session only steers — the **keeper** — and every task gets a session of its
own — a **bee** — that starts with a fresh context and its briefing.

```bash
imkr keeper                      # session `keeper` (`keeper-2` … when taken), goes there
imkr keeper --host server        # the same on another machine
imkr keeper --account work       # its agent in account `work` (or claude:work)
```

Without a terminal at hand: start a new session (`n` in `Ctrl+B S`), and after its name the list
of what it starts with offers **⚑ keeper** below `empty` and your templates. Then comes the
directory, as for every new session, and — on this machine, with more than one account logged
in — the account, as for `Ctrl+B k`. imkr waits in the background until the agent is ready and
then takes you there. The entry is there where the `keeper` skill is; without it and without
templates, a new session still asks for its directory right away.

Beside the agent, `imkr keeper` opens a narrow second pane with `imkr mail --follow`: what comes
in for the agent, what waits and what reached it. That way the agent's input line belongs to you
alone — you write there, and read the mail next to it.

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
it off; in `Ctrl+B S`, `f` does both for the selected session, on other machines too. Bees need no mark of their own: they sit under their keeper, as every session started
for another does.

From the keeper, one command starts a bee:

```bash
imkr spawn api-fix --brief briefings/api-fix.md --dir ~/src/api --worktree --task 9133
imkr spawn api-fix --brief briefings/api-fix.md --host server --dir ~/src/api
```

It starts session `api-fix` with the default agent (with `--worktree` in a new git worktree
`api-fix` of the repository, as `imkr worktree` makes them), waits until the agent is ready and
sends it the briefing as its first message, from the pane it was called in, so answers come
back with `imkr msg`. `--account NAME` starts the bee in that account — a used-up default
account does not stop the keeper from starting the next bee.

Keeper and bee are pane 0 of window 1: the agent is typed into the session's first shell, which
waits under it. So its address is `machine:NAME/0`, and it can move to another account like any
agent started by hand (`Ctrl+B s c`, or when its account is used up). The same holds for
`imkr new --start --window 0 -- PROGRAM`. A session of that name that was stopped (crash,
reboot) comes back with its windows instead; `imkr new` says so — `imkr forget NAME` first if
it should start empty. On another machine the briefing travels along on stdin and is kept there,
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
for example. `imkr operator` says which machine you sit at: where you last typed, clicked or
pasted into an imkr terminal.

```bash
imkr operator          # laptop
imkr operator --json   # {"machine": "laptop", "via": "codingmachine", "here": false, "idle": 4, "unanswered": []}
```

It asks every session on this machine and, only then, each machine imkr knows, with one ssh call
each and at most four seconds in all: your terminal may show a session elsewhere while the agent
runs here. The answer with the most recent input wins. A terminal that just attached or changed
its size does not count as you — every terminal does that when it reconnects, also on a machine
nobody sits at. `idle` is how many seconds ago you typed (`null`: in no terminal yet).

`via` is the name your machine reaches this one by, e.g. for a tunnel back; it is left out when
the answer came from another machine. Machines that did not answer are named in `unanswered` and
on stderr, never left out silently. `--here` asks only the sessions on this machine. With nobody
attached anywhere it fails. AI Brain's `agent-tunnel` uses it to open its browser where you are;
when you tell an agent where you sit, that comes first.

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
