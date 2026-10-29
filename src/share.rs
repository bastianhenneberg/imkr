//! The folder where imkr puts files meant for a person to look at (#9082):
//! screenshots and whatever comes after them.
//!
//! An agent gets its screenshot back over the wire and needs nothing of
//! this. A **person** needs the file somewhere they can open it — and
//! that place is not the agent's machine. So: one folder both the machine
//! and the user can reach. An SMB or NFS mount, Syncthing, Nextcloud,
//! iCloud, or a plain folder. imkr mounts nothing and does not care which
//! it is.
//!
//! ## The file is the truth, and it is nobody's
//!
//! It lives in `~/.config/share-dir.toml` and **not** in imkr's config:
//!
//! ```toml
//! dir = "~/Development/codingshare"
//!
//! [elsewhere]
//! laptop = "/Users/you/Development/codingshare"
//! ```
//!
//! `dir` is this machine's path. `[elsewhere]` is the same folder as the
//! other machines see it, by the name you call them — so a file written
//! here can be named the way the machine you sit at sees it.
//!
//! Both imkr and the AI Brain harness read and write it, and either works
//! on its own with it or without it. That is the reason it is a file of
//! its own rather than a setting of one program asking the other: two
//! programs that need the same answer should not need each other to get
//! it. **The field names are a contract** — `dir` and `[elsewhere]` —
//! and `imkr share --json` prints them.
//!
//! Without the file nothing is written and imkr says so. Nothing else
//! stops working.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Where screenshots go below the shared folder.
pub const SHOTS: &str = "screenshots";

/// The file both imkr and the harness read, in `$XDG_CONFIG_HOME` or
/// `~/.config`. Not under `imkr/`: it belongs to neither.
pub const FILE: &str = "share-dir.toml";

/// What the settings menu offers, and `share setup` proposes last: a
/// name, not a demand.
pub const SUGGESTION: &str = "~/codingshare";

/// The file's contents. Unknown keys are kept on writing, so a field the
/// harness adds is not thrown away by imkr saving.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Shared {
    /// This machine's path to the folder. `~` is expanded when used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// The same folder on the other machines, by the name you call them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub elsewhere: BTreeMap<String, String>,
    /// Anything another program put there.
    #[serde(flatten)]
    pub rest: BTreeMap<String, toml::Value>,
}

pub fn path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join(FILE))
}

/// The file as it stands, defaults when it is missing or broken: a typo
/// in it must never keep imkr from starting.
pub fn load() -> Shared {
    let Ok(path) = path() else {
        return Shared::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// How long a read is reused. The settings menu asks for every value on
/// every frame, and a file read per frame per setting is waste.
const FRESH: Duration = Duration::from_secs(1);

fn cache() -> &'static Mutex<Option<(Instant, Shared)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Shared)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// The file, read at most once a second.
pub fn cached() -> Shared {
    let mut cache = cache().lock().unwrap();
    if let Some((when, shared)) = cache.as_ref()
        && when.elapsed() < FRESH
    {
        return shared.clone();
    }
    let shared = load();
    *cache = Some((Instant::now(), shared.clone()));
    shared
}

/// Writes the file, keeping whatever else is in it.
pub fn save(shared: &Shared) -> Result<PathBuf> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = format!(
        "# Where programs put files for you to look at. Read and written by\n\
         # imkr (`imkr share`) and by the AI Brain harness; either works alone.\n\
         # `dir` is this machine's path, [elsewhere] the same folder as your\n\
         # other machines see it.\n\n{}",
        toml::to_string_pretty(shared)?
    );
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    *cache().lock().unwrap() = None;
    Ok(path)
}

/// Sets `dir`, keeping `[elsewhere]` and anything else.
pub fn set_dir(dir: Option<&str>) -> Result<PathBuf> {
    let mut shared = load();
    shared.dir = dir
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    save(&shared)
}

/// This machine's shared folder, `~` expanded. `None` when unset.
pub fn dir() -> Option<PathBuf> {
    expand(cached().dir.as_deref())
}

/// The path as set, unexpanded, for the settings menu.
pub fn dir_setting() -> Option<String> {
    cached().dir.filter(|d| !d.trim().is_empty())
}

fn expand(setting: Option<&str>) -> Option<PathBuf> {
    let text = setting?.trim();
    (!text.is_empty()).then(|| crate::template::home_dir(Path::new(text)))
}

/// What to say when there is none: the setting to make, not just that it
/// is missing.
pub fn missing() -> String {
    format!(
        "no shared folder set on {}: `imkr share setup` offers the folders it finds, or \
         put `dir = \"{SUGGESTION}\"` in ~/.config/{FILE} (an SMB or NFS mount, \
         Syncthing, Nextcloud or a plain folder both you and this machine can reach). \
         Without --save the file stays where imkr wrote it and the path is printed.",
        crate::protocol::machine_name()
    )
}

/// A file put into the shared folder: where it is here, and where the
/// same file is on the other machines.
#[derive(Debug, Clone)]
pub struct Placed {
    pub path: PathBuf,
    /// Relative to the shared folder, e.g. `screenshots/shot-1.png`.
    pub below: String,
    pub elsewhere: BTreeMap<String, String>,
}

impl Placed {
    pub fn json(&self) -> Value {
        json!({
            "path": self.path,
            "below": self.below,
            "machine": crate::protocol::machine_name(),
            "elsewhere": self.elsewhere,
        })
    }

    /// For a person: where to open it, named for their machines too — a
    /// path on a machine nobody sits at is no help.
    pub fn line(&self) -> String {
        let mut line = format!("saved to {}", self.path.display());
        for (host, path) in &self.elsewhere {
            line.push_str(&format!("\non {host}: {path}"));
        }
        line
    }
}

/// Moves `file` into `below` of the shared folder under its own name.
///
/// A move, not a copy: the file was written to a working place first
/// (that is what an agent gets back), and leaving a second copy of a
/// picture of somebody's screen lying about is not tidiness.
pub fn put(file: &Path, below: &str) -> Result<Placed> {
    let Some(dir) = dir() else {
        bail!("{}", missing());
    };
    let name = file
        .file_name()
        .context("the file has no name")?
        .to_string_lossy()
        .into_owned();
    let folder = dir.join(below);
    std::fs::create_dir_all(&folder).with_context(|| {
        format!(
            "cannot write into {} — is the shared folder mounted?",
            folder.display()
        )
    })?;
    let to = folder.join(&name);
    // Across a mount `rename` fails with EXDEV, so copy and remove.
    if std::fs::rename(file, &to).is_err() {
        std::fs::copy(file, &to)
            .with_context(|| format!("cannot copy into {}", folder.display()))?;
        let _ = std::fs::remove_file(file);
    }
    let relative = format!("{below}/{name}");
    Ok(Placed {
        path: to,
        elsewhere: elsewhere(&relative),
        below: relative,
    })
}

/// The same file as the other machines see it, from `[elsewhere]`.
pub fn elsewhere(below: &str) -> BTreeMap<String, String> {
    let here = crate::protocol::machine_name();
    cached()
        .elsewhere
        .iter()
        .filter(|(host, _)| **host != here)
        .map(|(host, root)| {
            (
                host.clone(),
                format!("{}/{below}", root.trim_end_matches('/')),
            )
        })
        .collect()
}

/// Folders worth offering, most likely first: a network mount, then a
/// folder of a syncing tool, then a name to make.
///
/// Mounts come from `/proc/self/mounts` rather than a tool, so nothing
/// has to be installed; the kinds are the ones people actually share
/// over (`cifs` is SMB).
pub fn candidates() -> Vec<(PathBuf, String)> {
    const KINDS: &[(&str, &str)] = &[
        ("cifs", "SMB mount"),
        ("smb3", "SMB mount"),
        ("nfs", "NFS mount"),
        ("nfs4", "NFS mount"),
        ("fuse.sshfs", "sshfs mount"),
        ("9p", "shared folder"),
        ("virtiofs", "shared folder"),
    ];
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_), Some(target), Some(kind)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let Some((_, what)) = KINDS.iter().find(|(k, _)| *k == kind) {
            // Mount tables escape spaces as octal.
            let target = target.replace("\\040", " ");
            found.push((PathBuf::from(target), (*what).to_owned()));
        }
    }
    // Folders of the usual syncing tools, when they are there.
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for (name, what) in [
            ("Development/codingshare", "folder"),
            ("Sync", "Syncthing"),
            ("Syncthing", "Syncthing"),
            ("Nextcloud", "Nextcloud"),
            ("ownCloud", "ownCloud"),
            ("Dropbox", "Dropbox"),
            ("Library/Mobile Documents/com~apple~CloudDocs", "iCloud"),
        ] {
            let dir = home.join(name);
            if dir.is_dir() && !found.iter().any(|(there, _)| *there == dir) {
                found.push((dir, what.to_owned()));
            }
        }
    }
    // A folder whose name says what it is for comes first: the mount
    // table's own order means nothing, and the one called `codingshare`
    // or `Sync` is the one somebody made for this.
    found.sort_by_key(|(dir, _)| {
        let name = dir.to_string_lossy().to_lowercase();
        let telling = ["share", "sync", "cloud", "drive"]
            .iter()
            .any(|word| name.contains(word));
        (!telling, name)
    });
    found
}

/// `imkr share [--json]`, `imkr share setup`, `imkr share set PATH`.
pub fn run(args: &[String]) -> Result<()> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => report(false),
        ["--json"] => report(true),
        ["setup"] => setup(false),
        ["setup", "--yes"] => setup(true),
        ["set", chosen] => {
            let path = set_dir(Some(chosen))?;
            println!("{chosen} written to {}", path.display());
            if dir().is_some_and(|dir| !dir.is_dir()) {
                println!("it is not there right now — mounted?");
            }
            Ok(())
        }
        ["unset"] => {
            let path = set_dir(None)?;
            println!("no shared folder any more ({})", path.display());
            Ok(())
        }
        _ => bail!("usage: imkr share [--json] | setup [--yes] | set PATH | unset"),
    }
}

/// Where the shared folder is, for people and for other programs.
fn report(json: bool) -> Result<()> {
    let shared = load();
    let here = crate::protocol::machine_name();
    let dir = expand(shared.dir.as_deref());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "machine": here,
                "dir": dir,
                "elsewhere": shared.elsewhere,
                "screenshots": dir.as_ref().map(|d| d.join(SHOTS)),
                "file": path().ok(),
            }))?
        );
        return Ok(());
    }
    match &dir {
        Some(dir) => {
            println!("{here}: {}", dir.display());
            if !dir.is_dir() {
                println!("  not there right now — mounted?");
            }
        }
        None => println!("{}", missing()),
    }
    for (host, root) in &shared.elsewhere {
        if *host != here {
            println!("{host}: {root}");
        }
    }
    Ok(())
}

/// Offers the folders this machine has and writes the chosen one. Asked
/// once; saying no is an answer and leaves the file alone.
fn setup(yes: bool) -> Result<()> {
    use std::io::{IsTerminal, Write};
    if let Some(dir) = dir_setting() {
        println!("already set: {dir} (imkr share set PATH changes it)");
        return Ok(());
    }
    let found = candidates();
    println!("Where should files you want to look at go — screenshots and the like?");
    println!("A folder both you and this machine can reach. Nothing is required.");
    for (i, (dir, what)) in found.iter().enumerate() {
        println!("  {}) {} ({what})", i + 1, dir.display());
    }
    println!("  {}) {SUGGESTION} (make it)", found.len() + 1);
    println!("  0) no folder — files stay on this machine");
    let pick = if yes || !std::io::stdin().is_terminal() {
        // Unattended, this picks only where there is nothing to pick
        // between. A first try took an SMB share meant for artwork
        // because it happened to be first in the mount table — a wrong
        // answer written into a config is worse than no answer, and
        // nobody would have noticed until a screenshot went missing.
        match found.len() {
            1 => 1,
            0 => {
                println!("nothing found and nobody to ask — no folder set");
                return Ok(());
            }
            n => {
                println!(
                    "{n} folders and nobody to ask — none set. \
                     `imkr share set PATH` picks one."
                );
                return Ok(());
            }
        }
    } else {
        print!("pick [1]: ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        match answer.trim() {
            "" => 1,
            text => text.parse::<usize>().unwrap_or(usize::MAX),
        }
    };
    let chosen = match pick {
        0 => {
            println!("no folder set. `imkr share setup` asks again.");
            return Ok(());
        }
        n if n >= 1 && n <= found.len() => found[n - 1].0.display().to_string(),
        n if n == found.len() + 1 => SUGGESTION.to_owned(),
        _ => bail!("no such choice"),
    };
    if let Some(dir) = expand(Some(&chosen))
        && !dir.is_dir()
    {
        std::fs::create_dir_all(&dir).with_context(|| format!("cannot make {}", dir.display()))?;
        println!("made {}", dir.display());
    }
    let path = set_dir(Some(&chosen))?;
    println!("{chosen} written to {}", path.display());
    println!(
        "On your other machines the same folder has another path: add it under \
         [elsewhere] there, so imkr can name a file the way you see it."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_read_as_a_path_with_a_home() {
        assert_eq!(expand(None), None);
        assert_eq!(expand(Some("  ")), None);
        assert_eq!(
            expand(Some("/mnt/share")),
            Some(PathBuf::from("/mnt/share"))
        );
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                expand(Some("~/codingshare")),
                Some(PathBuf::from(home).join("codingshare"))
            );
        }
    }

    /// The field names are the contract with the harness: `dir` and
    /// `[elsewhere]`, and anything else in the file survives a write.
    #[test]
    fn the_file_keeps_what_another_program_put_there() {
        let text = "dir = \"~/codingshare\"\nwritten_by = \"harness\"\n\
                    [elsewhere]\nlaptop = \"/Users/me/codingshare\"\n";
        let shared: Shared = toml::from_str(text).unwrap();
        assert_eq!(shared.dir.as_deref(), Some("~/codingshare"));
        assert_eq!(
            shared.elsewhere.get("laptop").map(String::as_str),
            Some("/Users/me/codingshare")
        );
        let again = toml::to_string_pretty(&shared).unwrap();
        assert!(
            again.contains("written_by"),
            "a foreign key was dropped: {again}"
        );
        assert!(again.contains("[elsewhere]"), "{again}");
        // A broken file is defaults, never a reason not to start.
        assert!(toml::from_str::<Shared>("dir = [1,2]").is_err());
    }

    #[test]
    fn the_missing_setting_says_what_to_set() {
        let said = missing();
        assert!(said.contains("share setup"), "{said}");
        assert!(said.contains(FILE), "{said}");
        assert!(said.contains("--save"), "{said}");
    }

    /// The line a person reads names the file on their own machine too.
    #[test]
    fn the_line_names_the_file_on_the_other_machines() {
        let placed = Placed {
            path: PathBuf::from("/home/me/codingshare/screenshots/a.png"),
            below: "screenshots/a.png".into(),
            elsewhere: [(
                "laptop".to_owned(),
                "/Users/me/codingshare/screenshots/a.png".to_owned(),
            )]
            .into_iter()
            .collect(),
        };
        let line = placed.line();
        assert!(
            line.contains("/home/me/codingshare/screenshots/a.png"),
            "{line}"
        );
        assert!(
            line.contains("on laptop: /Users/me/codingshare/screenshots/a.png"),
            "{line}"
        );
        assert_eq!(placed.json()["below"], "screenshots/a.png");
    }

    /// Mounts come out of the kernel's own table, so nothing has to be
    /// installed to find them.
    #[test]
    fn mounts_are_offered_as_candidates() {
        // Whatever this machine has, every candidate is an existing
        // folder with a word saying what it is.
        for (dir, what) in candidates() {
            assert!(!what.is_empty(), "{} has no kind", dir.display());
        }
    }
}
