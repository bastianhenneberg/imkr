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
    /// Stopped by an error: a used up limit, a refused login, an API error.
    /// It waits for the user like `Blocked`, but nothing will answer it.
    Failed,
}

impl AgentState {
    pub fn symbol(self) -> &'static str {
        match self {
            AgentState::Working => "●",
            AgentState::Blocked => "◆",
            AgentState::Idle => "✓",
            AgentState::Done => "✦",
            AgentState::Failed => "✗",
        }
    }

    /// The state `name` names, as `name` gives it.
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Working,
            Self::Blocked,
            Self::Idle,
            Self::Done,
            Self::Failed,
        ]
        .into_iter()
        .find(|state| state.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
            AgentState::Failed => "failed",
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
    "pi",
];

/// Shells: a pane running one of these in the foreground has no agent.
const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "nu", "elvish", "xonsh"];

/// Output within this time counts as working.
pub const ACTIVE: Duration = Duration::from_millis(1500);

/// How long an agent with an error on its screen is quiet before it counts
/// as stopped by it: a working agent keeps drawing its spinner.
pub const QUIET_ERROR: Duration = Duration::from_secs(5);

/// The user's own additions from the config: programs that are agents,
/// and phrases that mean a question waits. See `configure`.
static EXTRA: std::sync::RwLock<(Vec<String>, Vec<String>)> =
    std::sync::RwLock::new((Vec::new(), Vec::new()));

/// Takes the config's `extra_agents` and `question_phrases`.
pub fn configure(agents: &[String], phrases: &[String]) {
    *EXTRA.write().unwrap() = (agents.to_vec(), phrases.to_vec());
}

/// Whether `name` is an agent imkr knows or one of the config's
/// `extra_agents`.
pub fn is_agent(name: &str) -> bool {
    AGENTS.contains(&name) || EXTRA.read().unwrap().0.iter().any(|a| a == name)
}

/// The agent name if `argv` (a process's command line) runs one, looking at
/// the program and, for interpreters like node, its script: one imkr knows
/// or one of the config's `extra_agents`.
pub fn agent_name(argv: &[String]) -> Option<String> {
    agent_match(argv).map(|(name, _)| name)
}

/// `agent_name` with where the name came from: `known` or `config`.
pub fn agent_match(argv: &[String]) -> Option<(String, &'static str)> {
    let extra = EXTRA.read().unwrap();
    argv.iter().take(2).find_map(|arg| {
        let base = arg.rsplit('/').next().unwrap_or(arg);
        let base = base.strip_suffix(".js").unwrap_or(base);
        if let Some(agent) = AGENTS.iter().find(|agent| **agent == base) {
            return Some(((*agent).to_owned(), "known"));
        }
        extra
            .0
            .iter()
            .find(|agent| *agent == base)
            .map(|agent| (agent.clone(), "config"))
    })
}

pub fn is_shell(argv: &[String]) -> bool {
    argv.first().is_some_and(|arg| {
        let base = arg.rsplit('/').next().unwrap_or(arg);
        SHELLS.contains(&base.trim_start_matches('-'))
    })
}

/// What an agent's transcript tells about it: the model of its last
/// answer and how many tokens of context that answer was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub model: String,
    pub context: u64,
}

impl Meta {
    /// Short, for lists: `opus 5.5 · 297k`.
    pub fn label(&self) -> String {
        format!("{} · {}", short_model(&self.model), tokens(self.context))
    }
}

/// The last answer in a Claude Code transcript (JSON lines), read from its
/// end only: transcripts grow large.
pub fn transcript_meta(path: &std::path::Path) -> Option<Meta> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(512 * 1024);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    tail.lines().rev().find_map(|line| {
        if !line.contains("\"usage\"") {
            return None;
        }
        let entry: serde_json::Value = serde_json::from_str(line).ok()?;
        let message = &entry["message"];
        let usage = &message["usage"];
        let model = message["model"].as_str()?;
        // Synthetic answers (errors, interruptions) are no model's.
        if model.starts_with('<') {
            return None;
        }
        let context = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .iter()
        .filter_map(|key| usage[key].as_u64())
        .sum();
        Some(Meta {
            model: model.to_owned(),
            context,
        })
    })
}

/// `claude-opus-5-5` → `opus 5.5`, `claude-sonnet-4-5-20250929` →
/// `sonnet 4.5`; other names as they are.
pub fn short_model(id: &str) -> String {
    let id = id.strip_prefix("claude-").unwrap_or(id);
    let id = id.split('[').next().unwrap_or(id);
    let mut parts: Vec<&str> = id.split('-').collect();
    // A date at the end.
    if parts
        .last()
        .is_some_and(|p| p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    let Some(digits) = parts
        .iter()
        .position(|p| p.chars().all(|c| c.is_ascii_digit()))
    else {
        return id.to_owned();
    };
    format!(
        "{} {}",
        parts[..digits].join("-"),
        parts[digits..].join(".")
    )
}

/// `296636` → `297k`, `1234567` → `1.2M`.
pub fn tokens(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..999_500 => format!("{}k", (n + 500) / 1000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
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
pub fn asks_question(screen: &crate::term::Parser) -> bool {
    question_phrase(screen).is_some()
}

/// The phrase that makes `asks_question` see a question, if one does.
pub fn question_phrase(screen: &crate::term::Parser) -> Option<String> {
    let (_, cols) = screen.size();
    let last = last_row(screen);
    let text = screen.contents_between(last.saturating_sub(14), 0, last, cols);
    if let Some(q) = QUESTIONS.iter().find(|q| text.contains(**q)) {
        return Some((*q).to_owned());
    }
    let extra = EXTRA.read().unwrap();
    extra
        .1
        .iter()
        .find(|q| !q.is_empty() && text.contains(q.as_str()))
        .cloned()
}

/// What agents print when a turn ends in an error, with the reason shown
/// for it. Looked for only at the bottom of a quiet agent's screen.
const FAILURES: &[(&str, &str)] = &[
    ("usage limit", "limit reached"),
    ("hit your limit", "limit reached"),
    ("session limit", "limit reached"),
    ("weekly limit", "limit reached"),
    ("limit reached", "limit reached"),
    ("rate limit", "limit reached"),
    ("Rate limit", "limit reached"),
    ("rate_limit", "limit reached"),
    ("credit balance is too low", "no credit"),
    ("Please run /login", "login refused"),
    ("OAuth token has expired", "login refused"),
    ("authentication_error", "login refused"),
    ("overloaded", "overloaded"),
    ("Overloaded", "overloaded"),
    ("API Error", "API error"),
    ("stream error", "API error"),
    ("stream disconnected", "API error"),
];

/// Why the agent stopped, when the bottom of its screen shows an error.
pub fn failure(screen: &crate::term::Parser) -> Option<&'static str> {
    let (_, cols) = screen.size();
    let last = last_row(screen);
    let text = screen.contents_between(last.saturating_sub(7), 0, last, cols);
    FAILURES
        .iter()
        .find(|(phrase, _)| text.contains(phrase))
        .map(|(_, reason)| *reason)
}

/// The reason shown for an error Claude Code's `StopFailure` hook names.
pub fn failure_reason(error: &str) -> &'static str {
    match error {
        "rate_limit" => "limit reached",
        "overloaded" => "overloaded",
        "authentication_failed" | "oauth_org_not_allowed" => "login refused",
        "billing_error" => "no credit",
        "max_output_tokens" => "answer too long",
        _ => "API error",
    }
}

/// The last row of the screen with text on it.
fn last_row(screen: &crate::term::Parser) -> u16 {
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
pub fn tail(screen: &crate::term::Parser, max: usize) -> String {
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
    failed: bool,
) -> Option<AgentState> {
    // A script run by a shell, like `bash /usr/bin/claude`, is the agent,
    // not the shell.
    let agent = agent_name(argv);
    if argv.is_empty() || (agent.is_none() && is_shell(argv)) {
        return None;
    }
    // Working, said or guessed, but quiet with an error at the bottom: the
    // turn ended in it, and no hook says so (Codex, or an older Claude).
    let quiet_error = failed && since_output >= QUIET_ERROR;
    match reported {
        Some(AgentState::Working) if quiet_error => return Some(AgentState::Failed),
        Some(_) => return reported,
        None => {}
    }
    agent?;
    Some(if since_output < ACTIVE {
        AgentState::Working
    } else if quiet_error {
        AgentState::Failed
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

    fn screen(text: &str) -> crate::term::Parser {
        let mut parser = crate::term::Parser::new(10, 60, 0);
        parser.process(text.as_bytes());
        parser
    }

    #[test]
    fn reads_model_and_context_from_the_transcript_end() {
        let dir = std::env::temp_dir().join(format!("imkr-transcript-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let lines = [
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":3}}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":2866,"cache_read_input_tokens":296636,"output_tokens":830}}}"#,
            r#"{"type":"assistant","message":{"model":"<synthetic>","usage":{"input_tokens":0}}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let meta = transcript_meta(&path).unwrap();
        assert_eq!(meta.model, "claude-opus-5-5");
        assert_eq!(meta.context, 299_504);
        assert_eq!(meta.label(), "opus 5.5 · 300k");
        assert_eq!(short_model("claude-sonnet-4-5-20250929"), "sonnet 4.5");
        assert_eq!(short_model("gpt-5"), "gpt 5");
        assert_eq!(tokens(1_234_567), "1.2M");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_config_adds_agents_and_question_phrases() {
        assert_eq!(agent_name(&argv(&["imkr-test-agent"])), None);
        configure(&["imkr-test-agent".into()], &["Shall I go on".into()]);
        assert_eq!(
            agent_match(&argv(&["/opt/imkr-test-agent", "-v"])),
            Some(("imkr-test-agent".into(), "config"))
        );
        assert!(asks_question(screen("Shall I go on, then?\r\n").screen()));
        configure(&[], &[]);
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
        let mut tall = crate::term::Parser::new(40, 80, 0);
        tall.process(b"Edit main.rs\r\nDo you want to proceed?\r\n 1. Yes\r\n");
        assert!(asks_question(tall.screen()));
    }

    #[test]
    fn recognises_agents_directly_and_behind_node() {
        assert_eq!(agent_name(&argv(&["claude"])).as_deref(), Some("claude"));
        assert_eq!(
            agent_name(&argv(&["/home/me/.local/bin/claude", "--resume"])).as_deref(),
            Some("claude")
        );
        assert_eq!(
            agent_name(&argv(&["node", "/usr/lib/node_modules/gemini.js"])).as_deref(),
            Some("gemini")
        );
        // pi, as mise runs it.
        assert_eq!(
            agent_name(&argv(&[
                "/home/me/.local/share/mise/installs/pi/0/pi/pi",
                "-c"
            ]))
            .as_deref(),
            Some("pi")
        );
        assert_eq!(agent_name(&argv(&["nvim", "claude.md"])), None);
        assert_eq!(agent_name(&argv(&["vim"])), None);
        assert_eq!(agent_name(&argv(&["pip", "install"])), None);
    }

    #[test]
    fn shells_have_no_state_even_when_reported() {
        assert_eq!(
            state(
                &argv(&["-zsh"]),
                Some(AgentState::Working),
                Duration::ZERO,
                false,
                false
            ),
            None
        );
        assert_eq!(
            state(&argv(&["/bin/bash"]), None, Duration::ZERO, false, false),
            None
        );
    }

    #[test]
    fn agents_run_as_shell_scripts_are_agents() {
        let script = argv(&["/bin/bash", "/home/me/bin/claude"]);
        assert_eq!(
            state(&script, None, Duration::ZERO, false, false),
            Some(AgentState::Working)
        );
    }

    #[test]
    fn reported_state_wins_for_any_program() {
        let reported = Some(AgentState::Blocked);
        assert_eq!(
            state(&argv(&["my-tool"]), reported, Duration::ZERO, false, false),
            reported
        );
    }

    #[test]
    fn guesses_from_output_and_questions() {
        let quiet = Duration::from_secs(5);
        let claude = argv(&["claude"]);
        assert_eq!(
            state(&claude, None, Duration::ZERO, false, false),
            Some(AgentState::Working)
        );
        assert_eq!(
            state(&claude, None, quiet, false, false),
            Some(AgentState::Idle)
        );

        assert_eq!(
            state(&claude, None, quiet, true, false),
            Some(AgentState::Blocked)
        );

        assert_eq!(state(&argv(&["htop"]), None, quiet, true, false), None);
    }

    #[test]
    fn a_quiet_agent_with_an_error_at_the_bottom_failed() {
        let claude = argv(&["claude"]);
        let quiet = QUIET_ERROR;
        let working = Some(AgentState::Working);
        // Its hook said working, then the turn died: no hook said more.
        assert_eq!(
            state(&claude, working, quiet, false, true),
            Some(AgentState::Failed)
        );
        // Still drawing: working, whatever the screen says.
        assert_eq!(
            state(&claude, working, Duration::ZERO, false, true),
            working
        );
        // Done or waiting as its hook says: an old error does not count.
        for said in [AgentState::Idle, AgentState::Blocked] {
            assert_eq!(state(&claude, Some(said), quiet, false, true), Some(said));
        }
        // Without hooks, like Codex.
        assert_eq!(
            state(&argv(&["codex"]), None, quiet, false, true),
            Some(AgentState::Failed)
        );
    }

    #[test]
    fn errors_are_read_from_the_bottom_of_the_screen() {
        let limit = screen("> fix it\r\n\r\n  ⎿  You've hit your session limit · resets 5pm\r\n");
        assert_eq!(failure(&limit), Some("limit reached"));
        let api = screen("  ⎿  API Error: 529 {\"type\":\"error\"}\r\n");
        assert_eq!(failure(&api), Some("API error"));
        assert_eq!(failure(&screen("all good\r\n> ")), None);
        // Far above the bottom it is history, not the end of the turn.
        let old = format!("API Error: 500\r\n{}", "line\r\n".repeat(9));
        assert_eq!(failure(&screen(&old)), None);
    }

    #[test]
    fn hook_errors_get_short_reasons() {
        assert_eq!(failure_reason("rate_limit"), "limit reached");
        assert_eq!(failure_reason("authentication_failed"), "login refused");
        assert_eq!(failure_reason("server_error"), "API error");
        assert_eq!(failure_reason(""), "API error");
    }
}
