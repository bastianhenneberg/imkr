# Keys

Every key, as `imkr keys` prints it. Generated from `src/bindings.rs`.

```text
Press Ctrl+B, then:

Panes
  % # |          split side by side
  " -            split top and bottom
  x              close pane
  z              zoom pane / popup
  { }            swap with prev/next
  =              equal sizes
  r              name pane
  t              notepad of the pane…
  >              move pane to window…
  f              floating…

Go to
  ←↑↓→           pane there
  o ;            next / last pane
  g              any pane…
  /              search all…
  a              waiting agent
  i              inbox: answer agents…
  v              review changes…
  A              all agents…
  W              all windows…
  P              all panes…
  m M            mark pane / marks…
  1-9            window
  w              windows…
  n N            new window / elsewhere…

View
  [ y            copy mode
  j              jump to text on screen
  u              page up
  E              history in $EDITOR
  b              sidebar on/off
  e              into the sidebar
  F              focus mode
  Space          popup show/hide
  !              popup ⇄ window

Session
  q              quit…
  d              detach
  s              session…
  c              commands…
  p              plugins…
  ,              settings
  ?              all keys

Floating panes (Ctrl+B, f, then)
  f              float / tile this pane
  n              new floating pane
  o              next floating pane
  ←↑↓→           move
  Ctrl ←↑↓→      resize by 1
  Alt ←↑↓→       resize by 5
  ⌫              back
  Esc            cancel

Windows (Ctrl+B, w, then)
  1-9            go to window
  r              name window
  c              new window
  t              new window in a git worktree…
  h              new window on another machine…
  m              move to session
  l              layout…
  =              renumber windows
  n p ←→         next / previous
  ⌫              back
  Esc            cancel

Move this pane to (Ctrl+B, >, then)
  1-9            that window
  n              a new window
  Esc            cancel

Sessions and hives (Ctrl+B, s, then)
  s S            switch…
  n              new session…
  r              rename this session
  x              end this session
  d              detach
  c              accounts of the agents…
  h H            hives…
  a              save all as hive…
  l              while you were away…
  ⌫              back
  Esc            cancel

Your commands (Ctrl+B, c, then)
  ⌫              back
  Esc            cancel

Plugins (Ctrl+B, p, then)
  +              marketplace…
  ⌫              back
  Esc            cancel

More
  S              session list
  H              hives
  Ctrl/Alt ←↑↓→  resize
  Shift ←↑↓→     move float
  Q              quit imkr
  R              reload config
  Ctrl+B         sends Ctrl+B
  Esc            cancel

Session list (Ctrl+B, S), in the list
  ↑↓             select
  Enter          or key switch
  x              end
  r              rename
  s              save all as hive
  h              hives
  o              hide/show old
  Esc            cancel

Hives (Ctrl+B, H), in the list
  ↑↓             select
  Enter          or digit bring back
  s              save new
  u              update
  w              switch: only this hive runs
  r              rename
  x              delete
  Esc            cancel

Accounts (Ctrl+B, s c), in the list
  ↑↓             select
  Enter          use: the focused agent goes on in it, else new shells
  u              new shells of this session
  Esc            cancel

Move window (Ctrl+B, w m), in the list
  ↑↓             select
  Enter          or key move there
  Esc            cancel

Agents (Ctrl+B, A), in the list
  ↑↓             select
  Enter          or key go there
  m              mark for Alt+1-9
  Esc            cancel

Inbox (answer waiting agents) (Ctrl+B, i), in the list
  ↑↓             select
  digit          choose that option
  text Enter     answer
  Enter          go there
  ✉ Enter x      accept, decline a contact's message
  Esc            cancel

Windows everywhere (Ctrl+B, W), in the list
  ↑↓             select
  Enter          or key choose
  Esc            cancel

Panes everywhere (Ctrl+B, P), in the list
  ↑↓             select
  Enter          or key choose
  Esc            cancel

Marks (Alt+1 … 9 jumps) (Ctrl+B, M), in the list
  ↑↓             select
  Enter          or key go there
  J K            move down/up
  x              remove
  Esc            cancel

Copy mode (Ctrl+B, y)
  Move
    hjkl         by one
    w b e        words
    0 $          line ends
    g G          top, bottom
    ^U ^D        half page
    ^B ^F        page
  Jump & search
    s            jump (again: end there)
    / ?          search down, up
    n N          next, previous
  Select
    v            start
    V            whole lines
    y            copy
    i            hide keys
    q            leave

Focus, move and resize repeat: for a moment afterwards, the same keys work without the prefix.
```
