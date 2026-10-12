//! Checkpoints: the state of an agent's repository after each of its
//! turns, as commits on refs of their own (`refs/imkr/checkpoints/…`).
//! Taking one changes nothing the user sees: not the branch, not the
//! index, not a file.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Where the checkpoints of one agent go.
pub const REFS: &str = "refs/imkr/checkpoints";

fn git(dir: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    // A checkpoint needs an author: imkr, where git knows of no one.
    if args.first() == Some(&"commit-tree") {
        let known = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.email"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !known {
            for var in ["GIT_AUTHOR", "GIT_COMMITTER"] {
                command.env(format!("{var}_NAME"), "imkr");
                command.env(format!("{var}_EMAIL"), "imkr@localhost");
            }
        }
    }
    let output = command.output().context("could not run git")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Takes a checkpoint of the repository `dir` is in, as `name` (e.g.
/// `api-3`), with `message`: a commit of every file there, new ones too,
/// following the last checkpoint of that name, or the current commit for
/// the first. `None` when nothing changed since the last one.
pub fn take(dir: &Path, name: &str, message: &str) -> Result<Option<String>> {
    let top = PathBuf::from(git(dir, None, &["rev-parse", "--show-toplevel"])?);
    let head = git(&top, None, &["rev-parse", "--verify", "HEAD"])?;
    let git_dir = PathBuf::from(git(&top, None, &["rev-parse", "--absolute-git-dir"])?);
    // A throwaway index, so the user's stays as it is.
    let index = git_dir.join(format!("imkr-checkpoint-{}.index", std::process::id()));
    let tree = (|| {
        // From a copy of the user's index, which knows which files did not
        // change since, so only changed ones are read again.
        if std::fs::copy(git_dir.join("index"), &index).is_err() {
            git(&top, Some(&index), &["read-tree", "HEAD"])?;
        }
        git(&top, Some(&index), &["add", "--all", "--", "."])?;
        git(&top, Some(&index), &["write-tree"])
    })();
    let _ = std::fs::remove_file(&index);
    let tree = tree?;
    let reference = format!("{REFS}/{name}");
    let parent = git(
        &top,
        None,
        &["rev-parse", "--verify", "--quiet", &reference],
    )
    .ok();
    let last_tree = match &parent {
        Some(parent) => git(&top, None, &["rev-parse", &format!("{parent}^{{tree}}")])?,
        None => git(&top, None, &["rev-parse", "HEAD^{tree}"])?,
    };
    if tree == last_tree {
        return Ok(None);
    }
    let parent = parent.unwrap_or(head);
    let commit = git(
        &top,
        None,
        &["commit-tree", &tree, "-p", &parent, "-m", message],
    )?;
    git(&top, None, &["update-ref", &reference, &commit])?;
    Ok(Some(commit))
}

/// The checkpoints of the repository `dir` is in, newest first per agent,
/// as lines for the terminal.
pub fn list(dir: &Path) -> Result<String> {
    let refs = git(dir, None, &["for-each-ref", "--format=%(refname)", REFS])?;
    if refs.is_empty() {
        return Ok("no checkpoints in this repository yet".into());
    }
    let mut out = String::new();
    for reference in refs.lines() {
        let name = reference.trim_start_matches(&format!("{REFS}/"));
        out.push_str(&format!("{name}\n"));
        let log = git(
            dir,
            None,
            &[
                "log",
                "--format=%h  %cd  %s",
                "--date=format:%d.%m. %H:%M",
                "-n",
                "20",
                "--first-parent",
                reference,
                "--not",
                "HEAD",
            ],
        )?;
        if log.is_empty() {
            out.push_str("  (none since the current commit)\n");
        }
        for line in log.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.push_str(
        "\ngit diff <id>                       what changed since that checkpoint\n\
         git restore --source=<id> -- <path>   bring a file back as it was then\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_checkpoints_without_touching_index_or_branch() {
        let repo = std::env::temp_dir().join(format!("imkr-cp-{}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| git(&repo, None, args).unwrap();
        run(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        run(&["add", "a.txt"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "start",
        ]);
        // Nothing changed yet: no checkpoint.
        assert_eq!(take(&repo, "api-1", "turn").unwrap(), None);
        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();
        let status = run(&["status", "--porcelain"]);
        let branch = run(&["symbolic-ref", "HEAD"]);
        let first = take(&repo, "api-1", "turn 1")
            .unwrap()
            .expect("a checkpoint");
        // The user's index and branch are as before.
        assert_eq!(run(&["status", "--porcelain"]), status);
        assert_eq!(run(&["symbolic-ref", "HEAD"]), branch);
        assert_eq!(run(&["show", &format!("{first}:new.txt")]), "fresh");
        // Again without changes: none; with one: it follows the first.
        assert_eq!(take(&repo, "api-1", "turn 2").unwrap(), None);
        std::fs::write(repo.join("a.txt"), "three\n").unwrap();
        let second = take(&repo, "api-1", "turn 3").unwrap().unwrap();
        assert_eq!(run(&["rev-parse", &format!("{second}^")]), first);
        assert!(list(&repo).unwrap().contains("turn 3"));
        let _ = std::fs::remove_dir_all(repo);
    }
}
