# How imkr works

A look under the hood: what runs where, and why things survive what they survive.

## Server and client

```mermaid
flowchart LR
    subgraph term [your terminal]
        C[imkr client<br/>raw mode, keys in,<br/>screen out]
    end
    subgraph srv [imkr server, one per session]
        L[layout<br/>windows, splits, floats]
        R[renderer]
        P1[pane 1<br/>pty + terminal emulator]
        P2[pane 2]
        A[agent watcher]
    end
    C <-->|unix socket<br/>$XDG_RUNTIME_DIR/imkr/| R
    L --> R
    P1 --> R
    P2 --> R
    P1 --> A
    P2 --> A
    S1[zsh / claude] <--> P1
    S2[nvim] <--> P2
```

The first `imkr` starts a server in the background. The server owns every pty, parses what the
programs write (with `alacritty_terminal`), lays out the panes and draws the screen. The client
is thin: it sends key and mouse events and writes what the server drew to your terminal. Detach,
and only the client goes. Several clients can attach to one session, as in tmux.

Scripts and agents talk to the same socket: `imkr send`, `imkr read`, `imkr msg` and the MCP
server are requests over it, answered and done.

## Other machines

```mermaid
flowchart LR
    subgraph laptop [⌂ laptop, the operator]
        C[imkr client]
    end
    subgraph server [⇄ server]
        PX[imkr proxy]
        S[imkr server<br/>session api]
    end
    C -->|ssh -C server imkr proxy| PX
    PX <-->|socket| S
```

On another machine, the client stays where you sit: keys, clipboard, images and links are local.
ssh starts `imkr proxy` there, which passes the same protocol through. Every connection starts
from the machine you sit at, with your ssh keys: servers never need ssh back to your laptop.
Every 5 seconds imkr asks each known machine what runs there (`imkr ls --json`), which fills the
sidebar and the session list.

## Agent states

```mermaid
flowchart TB
    subgraph sources [what imkr knows]
        H[hooks: imkr status<br/>exact, from the agent]
        P["/proc: which program runs"]
        O[screen: output, questions]
    end
    H --> D{state}
    P --> D
    O --> D
    D --> W[● working]
    D --> B[◆ blocked]
    D --> N[✦ done]
    D --> I[✓ idle]
    B --> X[bell, red tab,<br/>notification, inbox]
    N --> X
```

imkr reads the program from `/proc` and guesses from the screen. Hooks (`imkr hooks --install`)
report the exact state and win over guesses. Each tick (500 ms) imkr compares, rings on a new
`blocked`, and keeps the time a pane has been in its state.

## Messages between agents

```mermaid
sequenceDiagram
    participant A as agent on laptop
    participant L as imkr (laptop)
    participant S as imkr (server)
    participant B as agent on server
    A->>L: imkr msg server:api/claude "…"
    L->>S: over ssh
    S->>S: mailbox
    Note over S: waits until the agent is idle,<br/>never into its question
    S->>B: typed in, with sender and how to answer
    B->>S: imkr msg laptop:imkr/0 "done"
    S-->>L: server cannot reach the laptop:<br/>kept in its outbox
    L->>S: next poll: imkr outbox take
    L->>A: typed in
```

When a machine is out of reach, the message waits in the sender's outbox for 24 hours, and the
machine it is for collects it the next time it asks. So answers find their way back without
every machine reaching every other.

## Updating without a restart

```mermaid
sequenceDiagram
    participant U as you
    participant S as imkr server (old)
    participant N as imkr server (new)
    U->>S: imkr update
    S->>S: write layout, screens, history<br/>to a handover file
    S->>N: exec the new binary, same process<br/>ptys, socket, clients stay open
    N->>N: read the handover
    Note over N: shells and agents never noticed,<br/>clients just redraw
```

The server execs the new binary in its own process. The open file descriptors (every pane's
pty, the socket, every attached client) stay open across `exec`; everything else goes through a
file. `imkr hosts sync` does this on every machine.

## When the machine restarts

```mermaid
flowchart LR
    R[server runs] -->|every second, on change| J[(session.json<br/>layout, dirs,<br/>agent session ids)]
    R -->|every 20 s| H[(scrollback/<br/>3000 lines per pane)]
    J --> B[next imkr after a reboot]
    H --> B
    B --> W[windows and splits back<br/>shells in their directories<br/>claude --resume id]
```

Processes cannot survive a reboot, but the layout can: imkr saves it while it runs. The next
`imkr` builds the windows again, starts shells in their old directories, puts each pane's
history above them and resumes the agents by their session ids. Ending a session on purpose
deletes the file and keeps the layout in the hive `last`.

## Hives

```mermaid
flowchart TB
    H[hive work] --> L[⌂ laptop<br/>imkr, notes]
    H --> S[⇄ server<br/>api]
    H --> D[⇄ desktop<br/>offline: kept as it was]
    L -.->|hive export / import,<br/>once a minute| S
```

A hive is a saved map of sessions on every machine. Every machine keeps a copy and fetches newer
ones from the others, so `imkr hive restore work` works from any of them, and a new machine joins
by restoring one.
