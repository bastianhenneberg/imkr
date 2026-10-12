//! What the program in a pane is doing, for coding agents: working, waiting
//! for the user, or done.
//!
//! Agents can report it themselves through `imkr status` (from their
//! hooks). Without that, imkr guesses from the foreground program, how
//! recently it wrote output, and questions on the screen.

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    /// Busy, writing output or thinking.
    Working,
    /// Waiting for the user: a permission prompt or a question.
    Blocked,
    /// Done with its turn, ready for the next prompt.
    Idle,
    /// Finished while the user was elsewhere and not looked at since. Only
    /// shown, never reported: it is `Idle` the user has not seen yet.
    Done,
}

impl AgentState {
    pub fn symbol(self) -> &'static str {
        match self {
            AgentState::Working => "●",
            AgentState::Blocked => "◆",
            AgentState::Idle => "✓",
            AgentState::Done => "✦",
        }
    }

    /// The state `name` names, as `name` gives it.
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Working, Self::Blocked, Self::Idle, Self::Done]
            .into_iter()
            .find(|state| state.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
        }
    }
}

/// Programs recognised as coding agents, by the name they run under.
const AGENTS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "aider",
    "gemini",
    "crush",
    "goose",
    "amp",
    "cursor-agent",
    "qwen",
    "kilo",
];

/// Shells: a pane running one of these in the foreground has no agent.
const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "nu", "elvish", "xonsh"];

/// Output within this time counts as working.
pub const ACTIVE: Duration = Duration::from_millis(1500);

/// The agent name if `argv` (a process's command line) runs one, looking at
/// the program and, for interpreters like node, its script.
pub fn agent_name(argv: &[String]) -> Option<&'static str> {
    argv.iter().take(2).find_map(|arg| {
        let base = arg.rsplit('/').next().unwrap_or(arg);
        let base = base.strip_suffix(".js").unwrap_or(base);
        AGENTS.iter().copied().find(|agent| *agent == base)
    })
}

pub fn is_shell(argv: &[String]) -> bool {
    argv.first().is_some_and(|arg| {
        let base = arg.rsplit('/').next().unwrap_or(arg);
        SHELLS.contains(&base.trim_start_matches('-'))
    })
}

/// Phrases agents and other tools show when they wait for an answer.
const QUESTIONS: &[&str] = &[
    "Do you want to",
    "Would you like to",
    "(y/n)",
    "[y/N]",
    "[Y/n]",
    "Allow this",
    "Press Enter to continue",
    "Waiting for your approval",
];

/// Whether the last lines written show a question: the bottom of the
/// screen, or above the empty rows of a screen not yet full.
pub fn asks_question(screen: &vt100::Screen) -> bool {
    let (_, cols) = screen.size();
    let last = last_row(screen);
    let text = screen.contents_between(last.saturating_sub(14), 0, last, cols);
    QUESTIONS.iter().any(|q| text.contains(q))
}

/// The last row of the screen with text on it.
fn last_row(screen: &vt100::Screen) -> u16 {
    let (rows, cols) = screen.size();
    screen
        .rows(0, cols)
        .take(rows as usize)
        .enumerate()
        .filter(|(_, row)| !row.trim().is_empty())
        .map(|(i, _)| i as u16)
        .last()
        .unwrap_or(rows.saturating_sub(1))
}

/// The last `max` lines with text at the bottom of the screen, without
/// the frames agents draw around them: what a waiting agent asks, or what
/// a finished one said last.
pub fn tail(screen: &vt100::Screen, max: usize) -> String {
    let (_, cols) = screen.size();
    let last = last_row(screen);
    let text = screen.contents_between(last.saturating_sub(29), 0, last, cols);
    let frame: &[char] = &['│', '╭', '╮', '╰', '╯', '─', '┃', '|'];
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.trim_end().trim_matches(frame).trim_end())
        .filter(|line| !line.trim().is_empty() && !line.chars().all(|c| frame.contains(&c)))
        .collect();
    lines[lines.len().saturating_sub(max)..].join("\n")
}

/// The state to show, from what is known about a pane. `reported` comes
/// from `imkr status`, `since_output` is the time since the last output,
/// `asking` whether an unanswered question is on the screen.
pub fn state(
    argv: &[String],
    reported: Option<AgentState>,
    since_output: Duration,
    asking: bool,
) -> Option<AgentState> {
    // A script run by a shell, like `bash /usr/bin/claude`, is the agent,
    // not the shell.
    let agent = agent_name(argv);
    if argv.is_empty() || (agent.is_none() && is_shell(argv)) {
        return None;
    }
    if reported.is_some() {
        return reported;
    }
    agent?;
    Some(if since_output < ACTIVE {
        AgentState::Working
    } else if asking {
        AgentState::Blocked
    } else {
        AgentState::Idle
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn screen(text: &str) -> vt100::Parser {
        let mut parser = vt100::Parser::new(10, 60, 0);
        parser.process(text.as_bytes());
        parser
    }

    #[test]
    fn tail_drops_frames_and_blank_lines() {
        let parser = screen(
            "one\r\n\r\n╭────╮\r\n│ Do you want to proceed? │\r\n│ ❯ 1. Yes │\r\n╰────╯\r\n",
        );
        assert_eq!(
            tail(parser.screen(), 2),
            " Do you want to proceed?\n ❯ 1. Yes"
        );
        assert_eq!(tail(parser.screen(), 10).lines().count(), 3);
    }

    #[test]
    fn spots_questions_at_the_bottom() {
        let asking = screen("Edit file src/main.rs\r\nDo you want to make this edit?\r\n❯ 1. Yes");
        assert!(asks_question(asking.screen()));
        assert!(!asks_question(screen("Here is the plan.\r\n> ").screen()));
        // A question on a screen not yet full, above its empty rows.
        let mut tall = vt100::Parser::new(40, 80, 0);
        tall.process(b"Edit main.rs\r\nDo you want to proceed?\r\n 1. Yes\r\n");
        assert!(asks_question(tall.screen()));
    }

    #[test]
    fn recognises_agents_directly_and_behind_node() {
        assert_eq!(agent_name(&argv(&["claude"])), Some("claude"));
        assert_eq!(
            agent_name(&argv(&["/home/me/.local/bin/claude", "--resume"])),
            Some("claude")
        );
        assert_eq!(
            agent_name(&argv(&["node", "/usr/lib/node_modules/gemini.js"])),
            Some("gemini")
        );
        assert_eq!(agent_name(&argv(&["nvim", "claude.md"])), None);
        assert_eq!(agent_name(&argv(&["vim"])), None);
    }

    #[test]
    fn shells_have_no_state_even_when_reported() {
        assert_eq!(
            state(
                &argv(&["-zsh"]),
                Some(AgentState::Working),
                Duration::ZERO,
                false
            ),
            None
        );
        assert_eq!(
            state(&argv(&["/bin/bash"]), None, Duration::ZERO, false),
            None
        );
    }

    #[test]
    fn agents_run_as_shell_scripts_are_agents() {
        let script = argv(&["/bin/bash", "/home/me/bin/claude"]);
        assert_eq!(
            state(&script, None, Duration::ZERO, false),
            Some(AgentState::Working)
        );
    }

    #[test]
    fn reported_state_wins_for_any_program() {
        let reported = Some(AgentState::Blocked);
        assert_eq!(
            state(&argv(&["my-tool"]), reported, Duration::ZERO, false),
            reported
        );
    }

    #[test]
    fn guesses_from_output_and_questions() {
        let quiet = Duration::from_secs(5);
        let claude = argv(&["claude"]);
        assert_eq!(
            state(&claude, None, Duration::ZERO, false),
            Some(AgentState::Working)
        );
        assert_eq!(state(&claude, None, quiet, false), Some(AgentState::Idle));

        assert_eq!(state(&claude, None, quiet, true), Some(AgentState::Blocked));

        assert_eq!(state(&argv(&["htop"]), None, quiet, true), None);
    }
}
