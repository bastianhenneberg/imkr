//! Fetching a published release and putting it in place of this binary.
//!
//! The other half lives in `upgrade.rs`: that one hands a *running server* over
//! to a binary that is already on disk. This one gets the binary there, so
//! `imkr upgrade` is the two of them in a row.
//!
//! No HTTP crate on purpose — `curl`/`wget`, `tar` and `sha256sum` do it, the
//! same tools install.sh uses, and a terminal multiplexer has no business
//! pulling in a TLS stack for one request a month.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "bastianhenneberg/imkr";

/// The release archive to ask for. Linux only, like the rest of imkr.
pub fn target_triple() -> Result<&'static str> {
    Ok(match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-musl",
        "aarch64" => "aarch64-unknown-linux-musl",
        other => bail!(
            "no release binaries for {other}; build from source instead:\n  \
             cargo install --git https://github.com/{REPO} --locked"
        ),
    })
}

/// `v0.1.2` is the tag, `0.1.2` is what `imkr version` prints.
pub fn tag_version(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// The `tag_name` of the newest release, out of the API's answer.
pub fn latest_tag_from_json(body: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(body).context("the release API did not answer with JSON")?;
    match value.get("tag_name").and_then(|t| t.as_str()) {
        Some(tag) if !tag.is_empty() => Ok(tag.to_string()),
        // A repository without releases answers 404 with a message, not a tag.
        _ => bail!(
            "no release found: {}",
            value
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("the answer carried no tag_name")
        ),
    }
}

fn downloader() -> Result<&'static str> {
    for tool in ["curl", "wget"] {
        if Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            return Ok(tool);
        }
    }
    bail!("needs curl or wget to download a release")
}

fn fetch(url: &str, into: Option<&Path>) -> Result<Vec<u8>> {
    let mut cmd = match (downloader()?, into) {
        ("curl", Some(path)) => {
            let mut c = Command::new("curl");
            c.args(["-fsSL", url, "-o"]).arg(path);
            c
        }
        ("curl", None) => {
            let mut c = Command::new("curl");
            c.args(["-fsSL", url]);
            c
        }
        (_, Some(path)) => {
            let mut c = Command::new("wget");
            c.arg("-qO").arg(path).arg(url);
            c
        }
        (_, None) => {
            let mut c = Command::new("wget");
            c.args(["-qO-", url]);
            c
        }
    };
    let out = cmd
        .output()
        .with_context(|| format!("could not run the downloader for {url}"))?;
    if !out.status.success() {
        bail!("could not download {url}");
    }
    Ok(out.stdout)
}

/// Replace `target` with `new`, keeping the old file readable for processes
/// that still have it open: rename inside the same directory is atomic, so no
/// one ever sees a half-written binary.
pub fn replace_binary(target: &Path, new: &Path) -> Result<()> {
    let dir = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory", target.display()))?;
    let staged = dir.join(format!(
        ".{}.new-{}",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("imkr"),
        std::process::id()
    ));
    std::fs::copy(new, &staged).with_context(|| {
        format!(
            "cannot write to {} — install somewhere you own, or re-run with the rights for it",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staged, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })?;
    Ok(())
}

/// `imkr upgrade`: fetch the newest release and become it.
pub fn upgrade() -> Result<()> {
    let target = target_triple()?;
    let here = env!("CARGO_PKG_VERSION");

    // Where to write has to be settled before anything is replaced: after the
    // rename `/proc/self/exe` points at the unlinked inode and reads back as
    // `… (deleted)`, which is no path to hand anyone.
    let exe = std::env::current_exe()?.canonicalize()?;

    let body = String::from_utf8_lossy(&fetch(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        None,
    )?)
    .into_owned();
    let tag = latest_tag_from_json(&body)?;
    let there = tag_version(&tag);

    if there == here {
        println!("imkr {here} is already the latest release");
        return Ok(());
    }
    println!("imkr {here} -> {there}");

    let dir = tempdir()?;
    let archive_name = format!("imkr-{target}.tar.gz");
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{archive_name}");
    let archive = dir.join(&archive_name);

    println!("downloading {archive_name}");
    fetch(&url, Some(&archive))?;
    verify_checksum(&dir, &archive_name, &format!("{url}.sha256"))?;

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&dir)
        .status()?;
    if !status.success() {
        bail!("could not unpack {archive_name}");
    }
    let fresh = dir.join("imkr");
    if !fresh.is_file() {
        bail!("no imkr in {archive_name}");
    }

    replace_binary(&exe, &fresh)?;
    let _ = std::fs::remove_dir_all(&dir);
    println!("replaced {}", exe.display());

    // Only worth saying something about if a session is actually running.
    match crate::cli::update_to(exe) {
        Ok(()) => {}
        Err(_) => println!("no running session to move over"),
    }
    Ok(())
}

fn verify_checksum(dir: &Path, archive_name: &str, url: &str) -> Result<()> {
    let sums = dir.join(format!("{archive_name}.sha256"));
    if fetch(url, Some(&sums)).is_err() {
        println!("this release carries no checksum, not verified");
        return Ok(());
    }
    for tool in [["sha256sum", "-c"], ["shasum", "-c"]] {
        let run = Command::new(tool[0])
            .arg(tool[1])
            .arg(sums.file_name().unwrap())
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .status();
        match run {
            Ok(status) if status.success() => {
                println!("checksum ok");
                return Ok(());
            }
            // The tool ran and said no — that is an answer, not a missing tool.
            Ok(_) => bail!("checksum does not match, nothing was replaced"),
            Err(_) => continue,
        }
    }
    println!("no sha256sum or shasum, checksum not verified");
    Ok(())
}

fn tempdir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("imkr-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_loses_its_v() {
        assert_eq!(tag_version("v0.1.2"), "0.1.2");
        assert_eq!(tag_version("0.1.2"), "0.1.2");
        assert_eq!(tag_version("v1.0.0-rc1"), "1.0.0-rc1");
    }

    #[test]
    fn the_triple_matches_this_machine() {
        let triple = target_triple().unwrap();
        assert!(triple.starts_with(std::env::consts::ARCH));
        assert!(triple.ends_with("-unknown-linux-musl"));
    }

    #[test]
    fn the_tag_comes_out_of_the_answer() {
        let tag = latest_tag_from_json(r#"{"tag_name": "v0.2.0", "draft": false}"#).unwrap();
        assert_eq!(tag, "v0.2.0");
    }

    #[test]
    fn a_repository_without_releases_says_so() {
        let err = latest_tag_from_json(r#"{"message": "Not Found"}"#).unwrap_err();
        assert!(err.to_string().contains("Not Found"), "{err}");
        assert!(latest_tag_from_json("<html>").is_err());
    }

    #[test]
    fn replacing_swaps_the_file_and_leaves_it_runnable() {
        let dir = std::env::temp_dir().join(format!("imkr-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("imkr");
        let fresh = dir.join("fresh");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&fresh, b"new").unwrap();

        replace_binary(&target, &fresh).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // Nothing staged is left lying around.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".imkr"))
            .collect();
        assert!(leftovers.is_empty(), "staged file left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unwritable_directory_says_where_the_problem_is() {
        let missing = Path::new("/this/does/not/exist/imkr");
        let fresh = std::env::temp_dir().join(format!("imkr-fresh-{}", std::process::id()));
        std::fs::write(&fresh, b"new").unwrap();
        let err = replace_binary(missing, &fresh).unwrap_err();
        assert!(err.to_string().contains("cannot write to"), "{err}");
        std::fs::remove_file(&fresh).unwrap();
    }
}
