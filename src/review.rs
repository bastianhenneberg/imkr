//! Reviewing an agent's changes: the diff of its repository, notes on
//! lines, sent to the agent together as one prompt.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `diff --git …`: a file starts.
    File,
    /// `@@ … @@`
    Hunk,
    Added,
    Removed,
    Context,
    /// Index lines, `---`/`+++`, mode changes.
    Meta,
}

/// A line of the diff, with the file it belongs to and its number there
/// (the new file's, the old one's for removed lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    pub text: String,
    pub file: String,
    pub number: Option<u32>,
}

/// A note on a line, kept by what it is on, so it finds its line again
/// when the diff is read anew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub file: String,
    pub number: Option<u32>,
    pub code: String,
    pub text: String,
}

impl Note {
    pub fn on(line: &Line, text: String) -> Self {
        Note {
            file: line.file.clone(),
            number: line.number,
            code: line.text.clone(),
            text,
        }
    }

    pub fn is_on(&self, line: &Line) -> bool {
        self.file == line.file && self.number == line.number && self.code == line.text
    }
}

/// The changes in the repository at `root` against its last commit,
/// staged or not, and new files. Only reads: the index stays as it is.
pub fn diff(root: &Path) -> Result<Vec<Line>> {
    let git = |args: &[&str]| -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .context("could not run git")?;
        // `diff --no-index` exits with 1 when there are differences.
        if !output.status.success() && output.status.code() != Some(1) {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let mut text = git(&["diff", "--no-color", "--no-ext-diff", "HEAD"])?;
    let new = git(&["ls-files", "--others", "--exclude-standard"])?;
    for file in new.lines().take(50) {
        text.push_str(&git(&[
            "diff",
            "--no-color",
            "--no-index",
            "--",
            "/dev/null",
            file,
        ])?);
    }
    Ok(parse(&text))
}

/// The lines of a unified diff, see `Line`.
pub fn parse(diff: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut file = String::new();
    let (mut old, mut new) = (0u32, 0u32);
    for text in diff.lines() {
        let (kind, number) = if let Some(rest) = text.strip_prefix("diff --git ") {
            // `a/path b/path`: the new path.
            file = rest
                .rsplit_once(" b/")
                .map_or(rest, |(_, path)| path)
                .to_owned();
            (Kind::File, None)
        } else if text.starts_with("@@") {
            // `@@ -12,3 +14,5 @@`
            let numbers: Vec<u32> = text
                .split_whitespace()
                .skip(1)
                .take(2)
                .filter_map(|range| range[1..].split(',').next().and_then(|n| n.parse().ok()))
                .collect();
            if let [o, n] = numbers[..] {
                (old, new) = (o, n);
            }
            (Kind::Hunk, None)
        } else if text.starts_with("+++")
            || text.starts_with("---")
            || !text.starts_with(['+', '-', ' '])
        {
            (Kind::Meta, None)
        } else if text.starts_with('+') {
            new += 1;
            (Kind::Added, Some(new - 1))
        } else if text.starts_with('-') {
            old += 1;
            (Kind::Removed, Some(old - 1))
        } else {
            (old, new) = (old + 1, new + 1);
            (Kind::Context, Some(new - 1))
        };
        out.push(Line {
            kind,
            text: text.to_owned(),
            file: file.clone(),
            number,
        });
    }
    out
}

/// The notes as one prompt for the agent.
pub fn prompt(notes: &[Note]) -> String {
    let mut text = String::from("Review notes on your current changes, please address them:");
    for note in notes {
        let at = match note.number {
            Some(n) => format!("{}:{n}", note.file),
            None => note.file.clone(),
        };
        let code = note.code.get(1..).unwrap_or_default().trim();
        if code.is_empty() {
            text.push_str(&format!("\n- {at}: {}", note.text));
        } else {
            text.push_str(&format!("\n- {at} (`{code}`): {}", note.text));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@ fn main() {
     let a = 1;
-    let b = 2;
+    let b = 3;
+    let c = 4;
     done();
";

    #[test]
    fn numbers_lines_by_file() {
        let lines = parse(DIFF);
        let kinds: Vec<Kind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::File,
                Kind::Meta,
                Kind::Meta,
                Kind::Meta,
                Kind::Hunk,
                Kind::Context,
                Kind::Removed,
                Kind::Added,
                Kind::Added,
                Kind::Context
            ]
        );
        assert!(lines.iter().all(|l| l.file == "src/a.rs"));
        let numbers: Vec<Option<u32>> = lines[5..].iter().map(|l| l.number).collect();
        assert_eq!(numbers, [Some(10), Some(11), Some(11), Some(12), Some(13)]);
    }

    #[test]
    fn notes_become_one_prompt() {
        let lines = parse(DIFF);
        let note = Note::on(&lines[8], "c is never used".into());
        assert!(note.is_on(&lines[8]) && !note.is_on(&lines[7]));
        assert_eq!(
            prompt(&[note]),
            "Review notes on your current changes, please address them:\n\
             - src/a.rs:12 (`let c = 4;`): c is never used"
        );
    }
}
