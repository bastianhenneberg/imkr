//! Git worktrees for windows of their own: an agent works on its own
//! branch in its own directory, next to the others.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Whether `name` can be a branch and a directory: letters, digits, `-`,
/// `_`, `.` and `/`, not starting with `-` or `.`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.', '/'])
        && !name.ends_with('/')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// Where the worktree `name` of the repository at `top` goes: next to it,
/// `<repo>--<name>`, with `/` in the name as `-`.
pub fn dir_for(top: &Path, name: &str) -> Option<PathBuf> {
    let repo = top.file_name()?.to_string_lossy();
    Some(top.with_file_name(format!("{repo}--{}", name.replace('/', "-"))))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("could not run git")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The main working tree of the repository `from` is in, also from inside
/// one of its worktrees: worktrees go next to it.
pub fn main_root(from: &Path) -> Result<PathBuf> {
    let top = git(from, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .with_context(|| format!("{} is in no git repository", from.display()))?;
    let common = git(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(Path::new(&common)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(top))
}

/// The worktree `name` of the repository `from` is in: made on branch
/// `name` (new, from the current commit, or the one there is), or the one
/// made before.
pub fn create(from: &Path, name: &str) -> Result<PathBuf> {
    if !valid_name(name) {
        bail!("{name:?} is no branch name: letters, digits, - _ . and /");
    }
    let main = main_root(from)?;
    let dir = dir_for(&main, name).context("the repository has no name")?;
    if dir.join(".git").exists() {
        return Ok(dir);
    }
    let path = dir.to_string_lossy().into_owned();
    let branch_exists = git(
        &main,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok();
    if branch_exists {
        git(&main, &["worktree", "add", &path, name])?;
    } else {
        git(&main, &["worktree", "add", "-b", name, &path])?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_places() {
        assert!(valid_name("fix/login"));
        assert!(!valid_name("-rf"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("../x"));
        assert_eq!(
            dir_for(Path::new("/src/api"), "fix/login"),
            Some(PathBuf::from("/src/api--fix-login"))
        );
    }

    #[test]
    fn makes_a_worktree_next_to_the_repository_once() {
        let base = std::env::temp_dir().join(format!("imkr-wt-{}", std::process::id()));
        let repo = base.join("api");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ]);
        let dir = create(&repo.join("."), "feature").unwrap();
        assert_eq!(dir, base.canonicalize().unwrap().join("api--feature"));
        assert!(dir.join(".git").exists());
        // Again, also from inside the worktree: the same one.
        assert_eq!(create(&dir, "feature").unwrap(), dir);
        assert!(create(&repo, "a b").is_err());
        let _ = std::fs::remove_dir_all(base);
    }
}
