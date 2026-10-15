# imkr and AI Brain

[AI Brain](https://brain.proxy.peppermint-digital.com) keeps tasks, knowledge and people.
The AI Brain harness (repository `ai-brain-harness`, command `ai-brain-cli`) is its program
on each machine. imkr works with it in three ways, and needs it for none
of its own features:

- **`brain`, a conversation**: like Hermes, with Claude or Codex, the project's knowledge and AI
  Brain's tools, in an imkr window.
- **CLI mode**: an agent for an AI Brain task, in an imkr window, with AI Brain's context.
- **Messages to people**: colleagues' agents reach yours through AI Brain, without a shared
  network.

Both need `ai-brain-cli` installed and connected:

```bash
curl -fsSL https://brain.proxy.peppermint-digital.com/install.sh | sh
ai-brain-cli connect
```

## brain: a conversation, like Hermes

`ai-brain-cli chat`, short `brain`, is the harness with an input line: it signs in to AI Brain,
brings the project's knowledge and AI Brain's tools, and then hands you Claude's or Codex' own
input line. imkr is where these conversations run: each in a window of its own, with its state in
the sidebar, its questions in the inbox and on the phone, several at once on every machine.

```mermaid
flowchart LR
    K["Ctrl+B p a"] --> C{"Claude · Codex ·<br/>go on · task"}
    C --> W["new window 'brain'<br/>ai-brain-cli chat --projekt …"]
    W --> B[("AI Brain<br/>knowledge · tools")]
    W --> S["● ◆ ✦ in sidebar, inbox, phone"]
```

| From | How |
|---|---|
| imkr, plugin | `Ctrl+B p a`: Claude, Codex, the last conversation again, or a task |
| a shell | `brain`, or `ai-brain-cli chat --modell codex --projekt imkr` |

The project is the AI Brain slug in the `CLAUDE.md` of the focused pane's directory or one above
it. The conversation runs on where imkr runs, so `Ctrl+B p a` in a session on the server talks
there.

## CLI mode: an agent per task

`ai-brain-cli agent` fetches a task, writes what the agent needs to know (the task, the
project's knowledge, the rules) and starts Claude Code or Codex on it. With `--imkr` it does so
in a new imkr window instead of in your shell. The work is split on purpose:

```mermaid
flowchart LR
    subgraph brain [AI Brain]
        T[("Task 7666")]
    end
    subgraph cli [ai-brain-cli]
        C[fetch task<br/>build context<br/>register the run]
    end
    subgraph imkr [imkr]
        W[new window<br/>claude running]
        S[● working / ◆ blocked / ✦ done]
        M[imkr msg]
    end
    T --> C --> W
    W --> S
    S -->|status hooks| C
    M -->|you or another agent| W
```

`ai-brain-cli` owns context, task and sign-in; imkr owns the window, the state and the
messages. So the agent shows in the sidebar, the inbox and `Ctrl+B a` like any other.

### Starting a run

| From | How |
|---|---|
| imkr, plugin | `Ctrl+B p t` (or `Ctrl+B p a`, then *Pick a task*): the open tasks of the project in this directory to pick one, or an order in words |
| a shell | `ai-brain-cli agent --task 7666 --imkr` |
| a shell, without a task | `ai-brain-cli agent --projekt imkr "Write the changelog" --imkr` |
| AI Brain | it lays out orders for a machine: `ai-brain-cli auftraege`, then `auftrag N` |

`ai-brain-cli agents` lists the runs on this machine, `logs ID` shows what one wrote,
`stop ID` ends it.

The plugin comes from the catalog `peppermint-digital/imkr-plugins`:

```toml
# ~/.config/imkr/config.toml
plugin_catalogs = ["git@github.com:peppermint-digital/imkr-plugins.git"]
```

```bash
imkr plugin add ai-brain
```

It shows the runs in the sidebar (`2 working · 1 waiting`), `Ctrl+B p a` opens a conversation,
`Ctrl+B p t` starts an agent for a task, `Ctrl+B p l` lists the runs. The project is the AI Brain slug in the `CLAUDE.md` of the focused
pane's directory or one above it; its tasks come from `ai-brain-cli aufgaben`. With an older
`ai-brain-cli` without that command, it asks for a task number or an order instead.

## Messages to people

Colleagues with their own imkr can message your agents, and you theirs. Two ways carry them:

```mermaid
flowchart TB
    A["your agent<br/>imkr msg martin:api/claude …"] --> Q{contact martin}
    Q -->|paired over a VPN| N["HTTP to martin's imkr web<br/>(NetBird, Tailscale)"]
    Q -->|through AI Brain| B["ai-brain-cli nachricht senden<br/>→ AI Brain"]
    B -->|every 30 s| F["martin's imkr fetches<br/>ai-brain-cli nachricht abholen"]
    N --> I
    F --> I["martin's inbox<br/>Ctrl+B i"]
    I -->|Enter accepts| P[typed into his pane once idle]
    I -->|x declines| X[gone]
```

Through AI Brain:

```bash
imkr connect brain martin                         # both sides, each with the other's name
imkr msg martin:api/claude "Can you look at the login bug?"
```

Over a VPN, where both can reach each other:

```bash
imkr connect invite                                 # you: a code, ten minutes
imkr connect join http://100.97.1.2:4657/ ABCD-EFGH # they
```

Messages from a contact wait until accepted, in `Ctrl+B i` or `imkr connect inbox`.
`imkr connect trust martin` types them in right away. A contact sends text only: no screens, no
keys, no shell.

### Delivered once, even when two imkr servers fetch

AI Brain hands out each message **at least once**: a message fetched but not confirmed comes
back. Two safeguards keep it from arriving twice:

```mermaid
sequenceDiagram
    participant A as imkr server 1
    participant B as imkr server 2
    participant L as contact-brain.lock
    participant AB as AI Brain
    A->>L: lock (non-blocking)
    B->>L: lock: taken, skip this round
    A->>AB: abholen
    AB-->>A: message #5, held for 5 min
    A->>A: into the inbox
    A->>AB: bestaetigen 5
    Note over AB: unconfirmed after 5 min:<br/>handed out again
```

On each machine only one imkr server fetches at a time, and AI Brain does not hand out a
message again for five minutes after it was fetched. If a fetcher dies before confirming, the
message comes again after those five minutes.
