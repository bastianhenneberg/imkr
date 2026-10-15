# The demo GIF for the README: vhs docs/demo/demo.tape (from the repo root).
# Runs in a world of its own (socket, config, state), not in your sessions.
Output docs/demo/imkr.gif
Require imkr
Set Shell bash
Set FontSize 15
Set Width 1400
Set Height 820
Set Padding 16
Set TypingSpeed 55ms
Set Theme "Catppuccin Mocha"

Hide
Type "source docs/demo/setup.sh" Enter
Show

Type "imkr" Sleep 400ms Enter
Sleep 2.5s

# The key menu after Ctrl+B
Ctrl+B
Sleep 2s
Type "%"
Sleep 1.2s

# An agent in each pane
Type "claude" Enter
Sleep 1s
Ctrl+B Left
Sleep 600ms
Type "claude 'make the start-up faster'" Enter
Sleep 7s

# Away to another window while they work
Ctrl+B Type "n"
Sleep 800ms
Type "shell" Enter
Sleep 4s

# One asks: the tab turns red, the inbox shows the question
Ctrl+B Type "i"
Sleep 3s
Type "1"
Sleep 2.5s
Escape
Sleep 800ms

# Straight to the agent that waited longest
Ctrl+B Type "a"
Sleep 3s

Hide
Type "imkr kill-server" Enter
