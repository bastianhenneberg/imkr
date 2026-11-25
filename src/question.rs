//! The question board of a session (#10254): what its keeper still has to
//! ask the user, numbered F1, F2 … per session, each with where it comes
//! from, a recommendation and, once answered, the answer word for word.
//!
//! A file per session, `<state>/questions/<session>.json`, written under a
//! lock, so the board does not depend on the agent the keeper runs —
//! Claude, Codex or one on Ollama write it the same way, with `imkr
//! question`. The keeper's side pane shows the open ones
//! (`imkr question ls --follow`), so they are not lost up in its
//! conversation, between pings and answers.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Where a question stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Open,
    Answered,
    /// Taken over by another question, or no longer needed.
    Superseded,
}

/// One question to the user.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// The number after the F: never given twice in a session.
    pub id: u32,
    /// The question, in a sentence.
    pub text: String,
    /// Where it comes from: a bee, a task, a bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// What the keeper recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommend: Option<String>,
    #[serde(default)]
    pub state: State,
    /// The user's answer, word for word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Seconds since 1970.
    pub at: u64,
    /// When it was answered or superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
    /// The question that took this one over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<u32>,
    /// Other questions this one is about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<u32>,
}

/// All questions of a session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    /// The number the next question gets.
    #[serde(default)]
    pub next: u32,
    #[serde(default)]
    pub questions: Vec<Question>,
}

impl Board {
    /// The open questions, oldest first.
    pub fn open(&self) -> impl Iterator<Item = &Question> {
        self.questions.iter().filter(|q| q.state == State::Open)
    }

    /// Adds a question and returns its number.
    pub fn add(&mut self, mut question: Question) -> u32 {
        let id = self
            .next
            .max(self.questions.iter().map(|q| q.id).max().unwrap_or(0) + 1)
            .max(1);
        self.next = id + 1;
        question.id = id;
        question.state = State::Open;
        self.questions.push(question);
        id
    }

    fn get(&mut self, id: u32) -> Result<&mut Question> {
        self.questions
            .iter_mut()
            .find(|q| q.id == id)
            .with_context(|| format!("there is no F{id}"))
    }

    /// Notes the user's `answer` to question `id`, word for word. An
    /// answered question can be answered again: the user corrected it.
    pub fn answer(&mut self, id: u32, answer: &str, at: u64) -> Result<()> {
        if answer.trim().is_empty() {
            bail!("an answer needs its text");
        }
        let q = self.get(id)?;
        q.state = State::Answered;
        q.answer = Some(answer.to_owned());
        q.closed_at = Some(at);
        q.by = None;
        Ok(())
    }

    /// Takes question `id` off the board without an answer, taken over by
    /// question `by` if there is one.
    pub fn drop(&mut self, id: u32, by: Option<u32>, at: u64) -> Result<()> {
        if by == Some(id) {
            bail!("F{id} cannot take itself over");
        }
        if let Some(by) = by {
            self.get(by)?;
        }
        let q = self.get(id)?;
        q.state = State::Superseded;
        q.closed_at = Some(at);
        q.by = by;
        Ok(())
    }
}

/// `F12`, `f12` or `12`.
pub fn parse_id(word: &str) -> Result<u32> {
    let digits = word
        .strip_prefix('F')
        .or_else(|| word.strip_prefix('f'))
        .unwrap_or(word);
    digits
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .with_context(|| format!("{word:?} is no question number, like F3"))
}

/// The board file of `session`.
pub fn path(session: &str) -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?
        .join("questions")
        .join(format!("{session}.json")))
}

/// How many questions of `session` are open; 0 without a board.
pub fn open_count(session: &str) -> usize {
    path(session)
        .ok()
        .and_then(|file| read(&file).ok())
        .map_or(0, |board| board.open().count())
}

/// The board in `file`; empty when there is none yet. A file that cannot
/// be read is an error, not an empty board: writing over it would lose
/// every question in it.
pub fn read(file: &Path) -> Result<Board> {
    match std::fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is no question board", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Board::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

/// Changes the board in `file` under its lock and writes it back whole,
/// so two keepers' commands at once lose nothing.
pub fn change<T>(file: &Path, f: impl FnOnce(&mut Board) -> Result<T>) -> Result<T> {
    use std::os::fd::AsRawFd;
    let dir = file.parent().context("a board needs a folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let lock_path = file.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open {}", lock_path.display()))?;
    // SAFETY: flock on a descriptor we own; released when `lock` closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("cannot lock {}", lock_path.display());
    }
    let mut board = read(file)?;
    let out = f(&mut board)?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&board)? + "\n")
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("cannot write {}", file.display()))?;
    Ok(out)
}

/// The lines that show `board` in a pane `width` wide: the open questions
/// with origin, recommendation and references, or all with `all`.
pub fn lines(board: &Board, session: &str, width: usize, all: bool) -> Vec<String> {
    let width = width.max(20);
    let open = board.open().count();
    let head = match open {
        0 => format!("questions of {session}: none open"),
        n => format!("questions of {session}: {n} open"),
    };
    let mut out = wrap(&head, width, "");
    for q in &board.questions {
        if !all && q.state != State::Open {
            continue;
        }
        out.push(String::new());
        let mut head = format!("F{}", q.id);
        match q.state {
            State::Open => {}
            State::Answered => head.push_str(" ✓"),
            State::Superseded => head.push_str(" ✗"),
        }
        if let Some(from) = &q.from {
            head.push_str(&format!(" · {from}"));
        }
        if let Some(at) = crate::protocol::chrono_like(q.at) {
            head.push_str(&format!(" · {at}"));
        }
        out.extend(wrap(&head, width, ""));
        out.extend(wrap(&q.text, width, "  "));
        if let Some(recommend) = &q.recommend {
            out.extend(wrap(&format!("→ {recommend}"), width, "  "));
        }
        if !q.refs.is_empty() {
            let refs: Vec<String> = q.refs.iter().map(|r| format!("F{r}")).collect();
            out.extend(wrap(&format!("see {}", refs.join(", ")), width, "  "));
        }
        if let Some(answer) = &q.answer {
            out.extend(wrap(&format!("answer: {answer}"), width, "  "));
        }
        if let Some(by) = q.by {
            out.extend(wrap(&format!("taken over by F{by}"), width, "  "));
        }
    }
    out
}

/// `text` broken into lines of at most `width` characters, each starting
/// with `indent`; a word longer than a line is cut.
fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let room = width.saturating_sub(indent.chars().count()).max(8);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word: String = word.to_owned();
        while word.chars().count() > room {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let head: String = word.chars().take(room).collect();
            word = word.chars().skip(room).collect();
            lines.push(head);
        }
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > room {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines.into_iter().map(|l| format!("{indent}{l}")).collect()
}

const USAGE: &str =
    "usage: imkr question add [--from WHERE] [--recommend TEXT] [--ref F3]… [--stdin | TEXT]
       imkr question answer F12 [--stdin | ANSWER]
       imkr question drop F12 [--by F14]
       imkr question ls [--all] [--json] [--follow]
       imkr question pane
  the questions this session's keeper still has to ask; -s picks another session";

/// `imkr question …` (alias `frage`).
pub fn cli(args: &[String]) -> Result<()> {
    let session = crate::protocol::session_name();
    let file = path(&session)?;
    let mut rest = args.to_vec();
    if rest.is_empty() || rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let sub = rest.remove(0);
    let now = crate::persist::now();
    match sub.as_str() {
        "add" | "new" | "neu" => {
            let from = crate::cli::take_value(&mut rest, "--from")?;
            let recommend = crate::cli::take_value(&mut rest, "--recommend")?;
            let mut refs = Vec::new();
            while let Some(r) = crate::cli::take_value(&mut rest, "--ref")? {
                refs.push(parse_id(&r)?);
            }
            let text = text_of(&mut rest)?;
            let id = change(&file, |board| {
                for r in &refs {
                    board.get(*r)?;
                }
                Ok(board.add(Question {
                    text,
                    from,
                    recommend,
                    refs,
                    at: now,
                    ..Question::default()
                }))
            })?;
            println!("F{id}");
        }
        "answer" | "answered" | "beantwortet" => {
            if rest.is_empty() {
                bail!(USAGE);
            }
            let id = parse_id(&rest.remove(0))?;
            let answer = text_of(&mut rest)?;
            change(&file, |board| board.answer(id, &answer, now))?;
            println!("F{id} answered");
        }
        "drop" | "superseded" | "ueberholt" => {
            let by = crate::cli::take_value(&mut rest, "--by")?
                .map(|b| parse_id(&b))
                .transpose()?;
            let [id] = rest.as_slice() else {
                bail!(USAGE);
            };
            let id = parse_id(id)?;
            change(&file, |board| board.drop(id, by, now))?;
            match by {
                Some(by) => println!("F{id} taken over by F{by}"),
                None => println!("F{id} dropped"),
            }
        }
        "ls" | "list" => {
            let all = crate::cli::take_flag(&mut rest, "--all");
            let json = crate::cli::take_flag(&mut rest, "--json");
            let follow = crate::cli::take_flag(&mut rest, "--follow")
                || crate::cli::take_flag(&mut rest, "-f");
            if !rest.is_empty() {
                bail!(USAGE);
            }
            if follow {
                return follow_board(&file, &session, all);
            }
            let board = read(&file)?;
            if json {
                let shown: Vec<&Question> = board
                    .questions
                    .iter()
                    .filter(|q| all || q.state == State::Open)
                    .collect();
                println!("{}", serde_json::to_string_pretty(&shown)?);
            } else {
                for line in lines(&board, &session, 100, all) {
                    println!("{line}");
                }
            }
        }
        "pane" => open_pane(&session)?,
        _ => bail!(USAGE),
    }
    Ok(())
}

/// The text of a question or answer: the words left, or stdin with
/// `--stdin`.
fn text_of(rest: &mut Vec<String>) -> Result<String> {
    let stdin = crate::cli::take_flag(rest, "--stdin");
    if let Some(option) = rest.iter().find(|w| w.starts_with("--")) {
        bail!("unknown option {option}\n{USAGE}");
    }
    let text = if stdin {
        if !rest.is_empty() {
            bail!("--stdin or text, not both");
        }
        std::io::read_to_string(std::io::stdin())?
    } else {
        rest.join(" ")
    };
    let text = text.trim().to_owned();
    if text.is_empty() {
        bail!("no text\n{USAGE}");
    }
    Ok(text)
}

/// Shows the board in a pane and draws it again when it changes or the
/// pane gets another size, until the pane is closed.
fn follow_board(file: &Path, session: &str, all: bool) -> Result<()> {
    let mut shown: Option<(Vec<String>, (u16, u16))> = None;
    let mut out = std::io::stdout().lock();
    // No cursor blinking in a board nobody types into.
    let _ = write!(out, "\x1b[?25l");
    loop {
        let size = crossterm::terminal::size().unwrap_or((40, 20));
        let board = match read(file) {
            Ok(board) => lines(&board, session, size.0 as usize, all),
            Err(e) => vec![format!("{e:#}")],
        };
        if shown.as_ref() != Some(&(board.clone(), size)) {
            let mut screen = String::from("\x1b[H\x1b[2J");
            for (i, line) in board.iter().enumerate() {
                if i > 0 {
                    screen.push_str("\r\n");
                }
                // The heading and each question's number stand out.
                if i == 0 || line.starts_with('F') {
                    screen.push_str(&format!("\x1b[1m{line}\x1b[0m"));
                } else {
                    screen.push_str(line);
                }
            }
            if write!(out, "{screen}").and_then(|()| out.flush()).is_err() {
                return Ok(());
            }
            shown = Some((board, size));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// The share of the right column the question board leaves the message
/// pane under it.
const BOARD_SHARE: f32 = 0.55;

/// `imkr question pane`: puts the board of this session above its message
/// pane (the pane named `mail`, as `imkr keeper` makes it), else beside the
/// focused pane.
pub fn open_pane(session: &str) -> Result<()> {
    use crate::protocol::Request;
    let socket = crate::protocol::session_socket(session)?;
    let panes = crate::client::request_at(&socket, Request::List)?;
    let list = panes.as_array().cloned().unwrap_or_default();
    if list.iter().any(|p| p["name"].as_str() == Some("questions")) {
        println!("this session has its question pane already");
        return Ok(());
    }
    let mail = list
        .iter()
        .find(|p| p["name"].as_str() == Some("mail"))
        .and_then(|p| p["pane"].as_u64());
    let back = list
        .iter()
        .find(|p| p["focused"].as_bool() == Some(true))
        .and_then(|p| p["pane"].as_u64());
    board_pane(&socket, mail.map(|m| m as crate::layout::PaneId))?;
    if let Some(back) = back {
        let _ = crate::client::request_at(&socket, Request::Reveal { pane: back as _ });
    }
    Ok(())
}

/// Opens the board pane in the session at `socket`: above pane `mail`
/// when there is one (it is focused first, the board goes on top), else
/// beside the focused pane. Returns its pane.
pub fn board_pane(socket: &Path, mail: Option<crate::layout::PaneId>) -> Result<u64> {
    use crate::protocol::Request;
    let imkr = std::env::current_exe()
        .map(|exe| exe.display().to_string())
        .unwrap_or_else(|_| "imkr".to_owned());
    if let Some(mail) = mail {
        crate::client::request_at(socket, Request::Reveal { pane: mail })?;
    }
    let reply = crate::client::request_at(
        socket,
        Request::New {
            command: vec![
                imkr,
                "question".to_owned(),
                "ls".to_owned(),
                "--follow".to_owned(),
            ],
            float: false,
            workspace: None,
            cwd: None,
            fresh: false,
            by: None,
            ratio: Some(if mail.is_some() { BOARD_SHARE } else { 0.7 }),
            above: mail.is_some(),
        },
    )?;
    let pane = reply["pane"].as_u64().context("no pane for the board")?;
    let _ = crate::client::request_at(
        socket,
        Request::Rename {
            target: crate::protocol::RenameTarget::Pane(pane as _),
            name: Some("questions".to_owned()),
        },
    );
    Ok(pane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_count_on_and_are_never_given_twice() {
        let mut board = Board::default();
        let q = |text: &str| Question {
            text: text.to_owned(),
            ..Question::default()
        };
        assert_eq!(board.add(q("a")), 1);
        assert_eq!(board.add(q("b")), 2);
        board.drop(2, None, 5).unwrap();
        board.questions.retain(|q| q.id != 2);
        assert_eq!(board.add(q("c")), 3, "a removed number stays used");
    }

    #[test]
    fn answer_and_drop_take_a_question_off_the_open_ones() {
        let mut board = Board::default();
        for text in ["one", "two", "three"] {
            board.add(Question {
                text: text.to_owned(),
                ..Question::default()
            });
        }
        board.answer(1, "Ja, so machen.", 10).unwrap();
        board.drop(2, Some(3), 11).unwrap();
        let open: Vec<u32> = board.open().map(|q| q.id).collect();
        assert_eq!(open, vec![3]);
        assert_eq!(board.questions[0].answer.as_deref(), Some("Ja, so machen."));
        assert_eq!(board.questions[1].by, Some(3));
        assert!(board.answer(9, "x", 1).is_err());
        assert!(board.answer(3, "  ", 1).is_err());
        assert!(board.drop(3, Some(3), 1).is_err());
        assert!(board.drop(3, Some(8), 1).is_err());
    }

    #[test]
    fn question_numbers_are_read_with_or_without_the_f() {
        assert_eq!(parse_id("F12").unwrap(), 12);
        assert_eq!(parse_id("f3").unwrap(), 3);
        assert_eq!(parse_id("7").unwrap(), 7);
        assert!(parse_id("F0").is_err());
        assert!(parse_id("Fx").is_err());
    }

    #[test]
    fn the_pane_shows_only_open_questions_wrapped_to_its_width() {
        let mut board = Board::default();
        board.add(Question {
            text: "Soll die Biene den alten Zweig loeschen oder behalten?".to_owned(),
            from: Some("bee kunden-x".to_owned()),
            recommend: Some("behalten".to_owned()),
            ..Question::default()
        });
        let second = board.add(Question {
            text: "Schon beantwortet".to_owned(),
            ..Question::default()
        });
        board.answer(second, "ja", 1).unwrap();
        board.add(Question {
            text: "Dritte".to_owned(),
            refs: vec![1],
            ..Question::default()
        });
        let shown = lines(&board, "keeper", 24, false);
        assert_eq!(shown[..2], ["questions of keeper: 2", "open"]);
        assert!(shown.iter().all(|l| l.chars().count() <= 24), "{shown:?}");
        let text = shown.join("\n");
        assert!(text.contains("F1 · bee kunden-x"), "{text}");
        assert!(text.contains("→ behalten"), "{text}");
        assert!(text.contains("see F1"), "{text}");
        assert!(!text.contains("F2"), "answered ones stay off: {text}");
        let everything = lines(&board, "keeper", 80, true).join("\n");
        assert!(everything.contains("F2 ✓"), "{everything}");
        assert!(everything.contains("answer: ja"), "{everything}");
    }

    #[test]
    fn a_board_survives_being_changed_under_its_lock() {
        let dir = std::env::temp_dir().join(format!("imkr-questions-{}", std::process::id()));
        let file = dir.join("s.json");
        let _ = std::fs::remove_dir_all(&dir);
        let id = change(&file, |b| {
            Ok(b.add(Question {
                text: "a".to_owned(),
                ..Question::default()
            }))
        })
        .unwrap();
        change(&file, |b| b.answer(id, "yes", 2)).unwrap();
        let board = read(&file).unwrap();
        assert_eq!(board.questions[0].state, State::Answered);
        std::fs::write(&file, "not json").unwrap();
        assert!(
            change(&file, |b| Ok(b.next)).is_err(),
            "a broken file is not overwritten with an empty board"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
