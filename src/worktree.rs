//! Git worktrees for windows of their own: an agent works on its own
//! branch in its own directory, out of sight of the projects (#9784).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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

/// The folder worktrees of `<parent>/<repo>` go in without `dir`:
/// `<parent>/.worktrees`, hidden from the projects beside it. Not in
/// `~/.local/share`: Claude Code lets a worktree inherit trust from the
/// folders above *its own* path or from its main checkout, not from the
/// folders above that, so out there every bee of a repository trusted only
/// through `~/Development` would stop at the trust question (#9784).
pub const HIDDEN: &str = ".worktrees";

/// The folder the worktrees of the repository at `top` go in: `setup.dir`
/// (`~` for the home), else [`HIDDEN`] beside the repository.
pub fn base_for(top: &Path, setup: &Setup) -> Option<PathBuf> {
    match setup.dir.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(dir) => Some(crate::template::home_dir(Path::new(dir.trim()))),
        None => Some(top.parent()?.join(HIDDEN)),
    }
}

/// Where the worktree `name` of the repository at `top` goes:
/// `<base>/<repo>/<name>`, with `/` in the name as `-`.
pub fn dir_for(top: &Path, name: &str, base: &Path) -> Option<PathBuf> {
    let repo = top.file_name()?;
    Some(base.join(repo).join(name.replace('/', "-")))
}

/// Where worktrees went before #9784, next to the repository as
/// `<repo>--<name>`: still found by name, made there no more.
fn old_name<'a>(main: &Path, dir: &'a str) -> Option<&'a str> {
    let repo = main.file_name()?.to_str()?;
    dir.strip_prefix(repo)?.strip_prefix("--")
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

/// The worktree `name` of the repository `from` is in, and whether it is
/// new: made on branch `name` (new, from the current commit, or the one
/// there is) where `setup` says, or the one made before, wherever it is.
pub fn create(from: &Path, name: &str, setup: &Setup) -> Result<(PathBuf, bool)> {
    if !valid_name(name) {
        bail!("{name:?} is no branch name: letters, digits, - _ . and /");
    }
    let main = main_root(from)?;
    if let Ok(made) = find(&main, name) {
        return Ok((made.dir, false));
    }
    let base = base_for(&main, setup).context("the repository has no folder above it")?;
    let dir = dir_for(&main, name, &base).context("the repository has no name")?;
    if dir.symlink_metadata().is_ok() {
        // Two repositories of one name share a `dir` of their own.
        bail!(
            "{} is there already and no worktree of {}: remove it, or set [worktree] dir",
            dir.display(),
            main.display()
        );
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot make {}", parent.display()))?;
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
    Ok((dir, true))
}

/// What a new worktree gets besides its checkout (#8567), from `[worktree]`
/// in the config and in `.imkr.toml` at the repository's root.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Setup {
    /// The folder new worktrees go in, as `<dir>/<repo>/<name>`; without
    /// it [`HIDDEN`] beside the repository. Only in imkr's config: where
    /// things go on this disk is not the project's to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Paths (globs with `*`, `?`) copied over from the main checkout, e.g.
    /// `.env`: what git does not carry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub copy: Vec<String>,
    /// Paths linked to the main checkout's instead, e.g. a build cache.
    /// Whatever writes there writes into the main checkout.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symlink: Vec<String>,
    /// Shell commands run in the new window before its program.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_create: Vec<String>,
    /// Shell commands run in the worktree before `imkr worktree rm`/`merge`
    /// removes it; one failing stops the removal.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pre_remove: Vec<String>,
}

impl Setup {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `[worktree]` of a project's `.imkr.toml`: a list given replaces the
/// global one, unless it has `"<global>"` in it, which stands for that.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProjectSetup {
    copy: Option<Vec<String>>,
    symlink: Option<Vec<String>>,
    post_create: Option<Vec<String>>,
    pre_remove: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProjectFile {
    worktree: ProjectSetup,
}

/// The project file's name, at the root of the main checkout.
pub const PROJECT_FILE: &str = ".imkr.toml";

/// `project` with `"<global>"` replaced by `global`; `global` without one.
fn merged(project: Option<Vec<String>>, global: &[String]) -> Vec<String> {
    match project {
        None => global.to_vec(),
        Some(list) => list
            .into_iter()
            .flat_map(|item| {
                if item == "<global>" {
                    global.to_vec()
                } else {
                    vec![item]
                }
            })
            .collect(),
    }
}

/// The setup for worktrees of the repository whose main checkout is
/// `main`: `global` with what its `.imkr.toml` says.
pub fn setup_for(main: &Path, global: &Setup) -> Result<Setup> {
    let path = main.join(PROJECT_FILE);
    let project = match std::fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str::<ProjectFile>(&text)
                .with_context(|| format!("cannot read {}", path.display()))?
                .worktree
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProjectSetup::default(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    Ok(Setup {
        dir: global.dir.clone(),
        copy: merged(project.copy, &global.copy),
        symlink: merged(project.symlink, &global.symlink),
        post_create: merged(project.post_create, &global.post_create),
        pre_remove: merged(project.pre_remove, &global.pre_remove),
    })
}

/// Whether `name` fits `pattern`, with `*` for any run and `?` for one
/// character.
fn fits(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|i| fits(rest, &name[i..])),
        Some(('?', rest)) => !name.is_empty() && fits(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && fits(rest, &name[1..]),
    }
}

/// The paths below `base` that `pattern` (relative, `/`-separated, globs
/// per part) names, relative to `base`.
pub fn expand(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::new()];
    for part in pattern.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." {
            return Vec::new();
        }
        let wanted: Vec<char> = part.chars().collect();
        let glob = part.contains(['*', '?']);
        let mut next = Vec::new();
        for dir in found {
            if !glob {
                let path = dir.join(part);
                if base.join(&path).symlink_metadata().is_ok() {
                    next.push(path);
                }
                continue;
            }
            let Ok(entries) = std::fs::read_dir(base.join(&dir)) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for name in names {
                let chars: Vec<char> = name.chars().collect();
                // `*` does not take hidden files unless the pattern starts with a dot.
                if name.starts_with('.') && !part.starts_with('.') {
                    continue;
                }
                if fits(&wanted, &chars) {
                    next.push(dir.join(&name));
                }
            }
        }
        found = next;
    }
    found.retain(|p| !p.as_os_str().is_empty());
    found
}

/// Copies and links into the new worktree `dir` what `setup` names from the
/// main checkout `main`; what is there already stays. Says what it did.
pub fn prepare(main: &Path, dir: &Path, setup: &Setup) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for pattern in &setup.copy {
        for rel in expand(main, pattern) {
            let target = dir.join(&rel);
            if target.symlink_metadata().is_ok() {
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let status = Command::new("cp")
                .arg("-a")
                .arg(main.join(&rel))
                .arg(&target)
                .status()
                .context("could not run cp")?;
            if !status.success() {
                bail!("could not copy {}", rel.display());
            }
            done.push(format!("copied {}", rel.display()));
        }
    }
    for pattern in &setup.symlink {
        for rel in expand(main, pattern) {
            let target = dir.join(&rel);
            if target.symlink_metadata().is_ok() {
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(main.join(&rel), &target)
                .with_context(|| format!("could not link {}", rel.display()))?;
            done.push(format!("linked {}", rel.display()));
        }
    }
    Ok(done)
}

/// A worktree of a repository, as `imkr worktree ls` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Info {
    pub name: String,
    pub dir: PathBuf,
    /// Its branch, empty when detached.
    pub branch: String,
    /// Files changed and not committed.
    pub changed: usize,
    /// Commits on it that the main checkout's branch does not have.
    pub ahead: usize,
    /// Its branch is in the main checkout's branch.
    pub merged: bool,
}

/// The worktrees of the repository `from` is in, besides the main one.
pub fn list(from: &Path) -> Result<Vec<Info>> {
    let main = main_root(from)?;
    let text = git(&main, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut dir = None;
        let mut branch = String::new();
        for line in block.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                dir = Some(PathBuf::from(path));
            } else if let Some(name) = line.strip_prefix("branch refs/heads/") {
                branch = name.to_owned();
            }
        }
        let Some(dir) = dir else { continue };
        if same_dir(&dir, &main) {
            continue;
        }
        let changed = git(&dir, &["status", "--porcelain"])
            .map(|s| s.lines().count())
            .unwrap_or(0);
        let (ahead, merged) = if branch.is_empty() {
            (0, false)
        } else {
            let ahead = git(&main, &["rev-list", "--count", &format!("HEAD..{branch}")])
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            (ahead, ahead == 0)
        };
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .map(|n| old_name(&main, &n).map(str::to_owned).unwrap_or(n))
            .unwrap_or_default();
        out.push(Info {
            name: if branch.is_empty() {
                name
            } else {
                branch.clone()
            },
            dir,
            branch,
            changed,
            ahead,
            merged,
        });
    }
    Ok(out)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.canonicalize().ok() == b.canonicalize().ok()
}

/// The worktree `name` of the repository `from` is in.
pub fn find(from: &Path, name: &str) -> Result<Info> {
    list(from)?
        .into_iter()
        .find(|w| w.name == name || w.branch == name)
        .with_context(|| format!("no worktree {name}"))
}

/// Removes the worktree `name` and its branch, after `pre_remove`. Without
/// `force` only one with nothing uncommitted and its branch merged.
pub fn remove(from: &Path, name: &str, setup: &Setup, force: bool) -> Result<Info> {
    let main = main_root(from)?;
    let info = find(from, name)?;
    if !force {
        if info.changed > 0 {
            bail!(
                "{name} has {} uncommitted change(s): commit them, or --force drops them",
                info.changed
            );
        }
        if !info.branch.is_empty() && !info.merged {
            bail!(
                "{name} has {} commit(s) not merged: imkr worktree merge {name}, or --force drops them",
                info.ahead
            );
        }
    }
    for command in &setup.pre_remove {
        let status = Command::new("sh")
            .args(["-c", command])
            .current_dir(&info.dir)
            .status()
            .context("could not run sh")?;
        if !status.success() {
            bail!("pre_remove {command:?} failed, {name} stays");
        }
    }
    let path = info.dir.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    git(&main, &args)?;
    if !info.branch.is_empty() {
        git(
            &main,
            &["branch", if force { "-D" } else { "-d" }, &info.branch],
        )?;
    }
    // The repository's folder of them, and `.worktrees`, once empty; the
    // folder above an old `<repo>--<name>` is the projects' own.
    if let Some(base) = base_for(&main, setup)
        && let Some(repo) = main.file_name()
        && info.dir.parent() == Some(&base.join(repo))
    {
        let _ = std::fs::remove_dir(base.join(repo));
        if setup.dir.is_none() {
            let _ = std::fs::remove_dir(&base);
        }
    }
    Ok(info)
}

/// Merges the worktree `name`'s branch into the branch checked out in the
/// main checkout, which must be clean; a conflict is undone. The
/// worktree's own changes must be committed first.
pub fn merge(from: &Path, name: &str) -> Result<Info> {
    let main = main_root(from)?;
    let info = find(from, name)?;
    if info.branch.is_empty() {
        bail!("{name} is on no branch");
    }
    if info.changed > 0 {
        bail!(
            "{name} has {} uncommitted change(s): commit them first",
            info.changed
        );
    }
    let dirty = git(&main, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty.is_empty() {
        bail!(
            "{} has uncommitted changes: commit or put them aside first",
            main.display()
        );
    }
    if let Err(e) = git(&main, &["merge", "--no-edit", &info.branch]) {
        let _ = git(&main, &["merge", "--abort"]);
        bail!("{name} does not merge cleanly, nothing changed: {e:#}");
    }
    find(from, name)
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
        let base = base_for(Path::new("/src/api"), &Setup::default()).unwrap();
        assert_eq!(base, PathBuf::from("/src/.worktrees"));
        assert_eq!(
            dir_for(Path::new("/src/api"), "fix/login", &base),
            Some(PathBuf::from("/src/.worktrees/api/fix-login"))
        );
        let own = Setup {
            dir: Some("/var/wt".into()),
            ..Setup::default()
        };
        assert_eq!(
            base_for(Path::new("/src/api"), &own),
            Some(PathBuf::from("/var/wt"))
        );
        assert_eq!(
            old_name(Path::new("/src/api"), "api--fix-login"),
            Some("fix-login")
        );
        assert_eq!(old_name(Path::new("/src/api"), "api-fix"), None);
    }

    #[test]
    fn makes_a_worktree_out_of_the_projects_once() {
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
        let setup = Setup::default();
        let (dir, fresh) = create(&repo.join("."), "feature", &setup).unwrap();
        assert!(fresh);
        let base = base.canonicalize().unwrap();
        assert_eq!(dir, base.join(".worktrees/api/feature"));
        assert!(dir.join(".git").exists());
        // Again, also from inside the worktree: the same one.
        assert_eq!(
            create(&dir, "feature", &setup).unwrap(),
            (dir.clone(), false)
        );
        assert!(create(&repo, "a b", &setup).is_err());

        // One made next to the repository before #9784 is still the one.
        run(&["worktree", "add", "-q", "-b", "old", "../api--old"]);
        let (old, fresh) = create(&repo, "old", &setup).unwrap();
        assert_eq!((old.clone(), fresh), (base.join("api--old"), false));
        let names: Vec<_> = list(&repo).unwrap().into_iter().map(|w| w.name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"old".to_owned()));

        // An own folder: there, and a second repository of the same name
        // is not handed the first one's worktree.
        let own = Setup {
            dir: Some(base.join("own").to_string_lossy().into_owned()),
            ..Setup::default()
        };
        let (mine, _) = create(&repo, "mine", &own).unwrap();
        assert_eq!(mine, base.join("own/api/mine"));
        let other = base.join("elsewhere/api");
        std::fs::create_dir_all(&other).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "s",
            ],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&other)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let clash = create(&other, "mine", &own).unwrap_err();
        assert!(
            format!("{clash:#}").contains("is there already"),
            "{clash:#}"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn globs_fit() {
        let f = |p: &str, n: &str| {
            fits(
                &p.chars().collect::<Vec<_>>(),
                &n.chars().collect::<Vec<_>>(),
            )
        };
        assert!(f(".env*", ".env.local"));
        assert!(f("*.json", "a.json"));
        assert!(!f("*.json", "a.jsonl"));
        assert!(f("?", "a"));
    }

    #[test]
    fn project_lists_take_the_global_ones_in() {
        let global = vec!["a".to_owned()];
        assert_eq!(merged(None, &global), ["a"]);
        assert_eq!(merged(Some(vec!["b".into()]), &global), ["b"]);
        assert_eq!(
            merged(Some(vec!["<global>".into(), "b".into()]), &global),
            ["a", "b"]
        );
    }

    #[test]
    fn worktrees_are_set_up_listed_merged_and_removed() {
        let base = std::env::temp_dir().join(format!("imkr-wt2-{}", std::process::id()));
        let repo = base.join("web");
        std::fs::create_dir_all(repo.join("config")).unwrap();
        let git_in = |dir: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git_in(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), ".env*\ncache\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-q", "-m", "start"]);
        std::fs::write(repo.join(".env"), "SECRET=1").unwrap();
        std::fs::write(repo.join(".env.local"), "X=1").unwrap();
        std::fs::create_dir_all(repo.join("cache")).unwrap();
        std::fs::write(
            repo.join(PROJECT_FILE),
            "[worktree]\ncopy = [\"<global>\", \".env*\"]\nsymlink = [\"cache\"]\n",
        )
        .unwrap();
        let global = Setup {
            copy: vec!["config/none".into()],
            ..Setup::default()
        };
        let setup = setup_for(&repo, &global).unwrap();
        assert_eq!(setup.copy, ["config/none", ".env*"]);
        assert_eq!(setup.dir, None);
        let (dir, _) = create(&repo, "feature", &setup).unwrap();
        let done = prepare(&repo, &dir, &setup).unwrap();
        assert_eq!(done, ["copied .env", "copied .env.local", "linked cache"]);
        assert_eq!(
            std::fs::read_to_string(dir.join(".env")).unwrap(),
            "SECRET=1"
        );
        assert!(dir.join("cache").symlink_metadata().unwrap().is_symlink());

        // A commit there: not merged, so rm refuses.
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        git_in(&dir, &["add", "new.txt"]);
        git_in(&dir, &["commit", "-q", "-m", "work"]);
        let info = find(&repo, "feature").unwrap();
        assert_eq!((info.ahead, info.merged, info.changed), (1, false, 0));
        assert!(remove(&repo, "feature", &Setup::default(), false).is_err());

        // Merged, then removed with its branch.
        let info = merge(&repo, "feature").unwrap();
        assert!(info.merged);
        assert!(repo.join("new.txt").exists());
        remove(&repo, "feature", &Setup::default(), false).unwrap();
        assert!(!dir.exists());
        assert!(!base.join(HIDDEN).exists(), "empty folders are left");
        assert!(list(&repo).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(base);
    }
}
