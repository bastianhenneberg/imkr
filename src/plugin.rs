//! Plugins: programs of your own, at hand in the sidebar and on a key,
//! without a pane or window of their own. Each is a TOML file in
//! `plugins/` next to the config:
//!
//! ```toml
//! title = "Peppermint"
//! command = "peppermint-tui"       # run with `sh -c`, as a popup
//! key = "p"                        # Ctrl+B p p, optional
//! status = "peppermint-tui status" # lines for the sidebar, optional
//! interval = 30                    # seconds between them
//! bar = true                       # its first status line in the bar too, with the title
//! mode = "popup"                   # or "fullscreen"
//!
//! [[actions]]                      # more commands in Ctrl+B p, optional
//! key = "d"
//! title = "Deploy"
//! run = "peppermint-tui deploy"    # in the background, or `popup = true`
//!
//! [[hooks]]                        # as in the config, optional
//! on = "done"
//! run = "peppermint-tui notify"
//! ```
//!
//! A plugin is a program like any other, so it can be written in any
//! language and cannot take imkr down with it. Broken files are listed with
//! what is wrong, the rest work.
//!
//! `imkr plugin add` brings plugins from a git repository or a directory:
//! the clone is kept in `plugins/.sources/`, and each plugin file of it is
//! linked into `plugins/`. `{dir}` in a command is the directory its file is
//! in, for scripts that come with it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Over the active window, most of its size.
    #[default]
    Popup,
    /// Over everything but the bars.
    Fullscreen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    /// The file's name without `.toml`, also the popup's name. Set from the
    /// file name when loading; carried along when the plugin travels with
    /// the user's client to another machine.
    #[serde(default)]
    pub name: String,
    /// The user's own machine it came from, when it is not this one's.
    #[serde(skip)]
    pub from: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub command: String,
    #[serde(default)]
    pub key: Option<char>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// Show the first line of `status` in the bar as well, not only in the
    /// sidebar. Left out of the serialized form while it is off, so that
    /// plugins travelling to an older imkr do not trip its parser.
    #[serde(default, skip_serializing_if = "is_off")]
    pub bar: bool,
    #[serde(default)]
    pub mode: Mode,
    /// `false` keeps the file but leaves the plugin out.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// More commands of the plugin, on keys of their own in `Ctrl+B p`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Commands run when something happens, as `[[hooks]]` in the config.
    /// Only where the plugin's file is, not where it was brought along to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<crate::config::Hook>,
}

/// A command of a plugin besides its main one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub key: char,
    pub title: String,
    /// Run with `sh -c`, with `IMKR_PLUGIN` set to the plugin's name.
    pub run: String,
    /// In a popup of its own, to watch it, instead of in the background.
    #[serde(default)]
    pub popup: bool,
}

fn default_interval() -> u64 {
    30
}

fn yes() -> bool {
    true
}

fn is_off(bar: &bool) -> bool {
    !bar
}

impl Plugin {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }

    /// Whether the program it runs is on this machine: the first word of
    /// its command, looked up on the PATH.
    pub fn installed(&self) -> bool {
        let Some(program) = self.command.split_whitespace().next() else {
            return false;
        };
        Command::new("sh")
            .args(["-c", "command -v \"$0\" >/dev/null", program])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// A plugin file that could not be used, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broken {
    pub name: String,
    pub problem: String,
}

/// Where plugin files live: `plugins/` next to the config file.
pub fn dir() -> Option<PathBuf> {
    Some(crate::config::path().ok()?.parent()?.join("plugins"))
}

/// The plugins in `dir`, by name, and the files that are broken. Disabled
/// ones are left out.
pub fn load_from(dir: &Path) -> (Vec<Plugin>, Vec<Broken>) {
    let mut plugins = Vec::new();
    let mut broken = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (plugins, broken);
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match parse(&path) {
            Ok(plugin) if !plugin.enabled => {}
            Ok(plugin) => plugins.push(Plugin { name, ..plugin }),
            Err(problem) => broken.push(Broken { name, problem }),
        }
    }
    (plugins, broken)
}

/// A plugin file, with `{dir}` in its commands made the directory it is in
/// (behind links), quoted for `sh`.
fn parse(path: &Path) -> Result<Plugin, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut plugin = toml::from_str::<Plugin>(&text).map_err(|e| e.message().to_owned())?;
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if let Some(dir) = real.parent() {
        let dir = crate::client::sh_quote(&dir.to_string_lossy());
        let fill = |command: &mut String| *command = command.replace("{dir}", &dir);
        fill(&mut plugin.command);
        if let Some(status) = &mut plugin.status {
            fill(status);
        }
        plugin.actions.iter_mut().for_each(|a| fill(&mut a.run));
        plugin.hooks.iter_mut().for_each(|h| fill(&mut h.run));
    }
    Ok(plugin)
}

/// Where plugins come from that `imkr plugin add` brought: a git
/// repository, cloned into `plugins/.sources/NAME`, or a directory, used
/// where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    /// The repository's URL, or the directory.
    pub from: String,
    pub git: bool,
    /// The directory in the repository its plugins are in, when it has
    /// more than one, like a catalog with its plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
}

impl Source {
    /// Where its files are.
    pub fn root(&self, plugins: &Path) -> PathBuf {
        match &self.sub {
            Some(sub) => self.clone_dir(plugins).join(sub),
            None => self.clone_dir(plugins),
        }
    }

    /// The whole repository, or the directory.
    pub fn clone_dir(&self, plugins: &Path) -> PathBuf {
        if self.git {
            sources_dir(plugins).join(&self.name)
        } else {
            PathBuf::from(&self.from)
        }
    }
}

fn sources_dir(plugins: &Path) -> PathBuf {
    plugins.join(".sources")
}

pub fn load_sources(plugins: &Path) -> Vec<Source> {
    std::fs::read_to_string(sources_dir(plugins).join("sources.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_sources(plugins: &Path, sources: &[Source]) -> std::io::Result<()> {
    let dir = sources_dir(plugins);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(sources).unwrap_or_default();
    std::fs::write(dir.join("sources.json"), json)
}

/// What `imkr plugin add` was given: a directory or plugin file here, a
/// git URL, or `owner/repo` on GitHub; a directory in the repository after
/// it, as `owner/repo/dir` or `URL#dir`.
pub fn parse_source(given: &str) -> Result<Source, String> {
    let path = Path::new(given);
    if path.exists() {
        let real = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let name = real
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(Source {
            name,
            from: real.to_string_lossy().into_owned(),
            git: false,
            sub: None,
        });
    }
    let (url, sub) = if given.contains("://") || given.starts_with("git@") {
        match given.split_once('#') {
            Some((url, sub)) => (url.to_owned(), Some(sub.to_owned())),
            None => (given.to_owned(), None),
        }
    } else {
        let parts: Vec<&str> = given.trim_end_matches('/').split('/').collect();
        if parts.len() < 2
            || given.starts_with('.')
            || given.contains(' ')
            || parts.iter().any(|p| p.is_empty())
        {
            return Err(format!("{given} is no directory, git URL or owner/repo"));
        }
        let sub = (parts.len() > 2).then(|| parts[2..].join("/"));
        (format!("https://github.com/{}/{}", parts[0], parts[1]), sub)
    };
    let sub = sub
        .map(|s| s.trim_matches('/').to_owned())
        .filter(|s| !s.is_empty());
    if sub
        .as_deref()
        .is_some_and(|s| s.split('/').any(|p| p == ".." || p == "."))
    {
        return Err(format!(
            "{given}: the directory must be inside the repository"
        ));
    }
    let name = match &sub {
        Some(sub) => sub.rsplit('/').next().unwrap_or_default().to_owned(),
        None => url
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .next()
            .unwrap_or_default()
            .trim_end_matches(".git")
            .to_owned(),
    };
    if name.is_empty() || name.starts_with('.') {
        return Err(format!("cannot name a plugin source after {given}"));
    }
    Ok(Source {
        name,
        from: url,
        git: true,
        sub,
    })
}

// --- Catalogs ---

/// A plugin a catalog offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Where it is: a directory of the catalog (the default, its name), or
    /// anything `imkr plugin add` takes.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    #[serde(default, rename = "plugin")]
    plugins: Vec<CatalogEntry>,
}

fn catalogs_dir(plugins: &Path) -> PathBuf {
    plugins.join(".catalogs")
}

/// Brings catalog `given` here: a repository is cloned once into
/// `plugins/.catalogs/NAME` and, with `refresh`, brought up to date (it
/// only lists, installing still shows what runs); a directory is read
/// where it is. Returns where the catalog came from and its directory.
pub fn fetch_catalog(
    plugins: &Path,
    given: &str,
    refresh: bool,
) -> Result<(Source, PathBuf), String> {
    let source = parse_source(given)?;
    if !source.git {
        let root = source.root(plugins);
        return Ok((source, root));
    }
    let dir = catalogs_dir(plugins).join(&source.name);
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if !dir.join(".git").exists() {
        let _ = std::fs::create_dir_all(catalogs_dir(plugins));
        let cloned = Command::new("git")
            .args(["clone", "--quiet", "--depth", "1", "--", &source.from])
            .arg(&dir)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !cloned {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("could not clone the catalog {given}"));
        }
    } else if refresh
        && !(git(&["fetch", "--quiet", "--depth", "1", "origin"])
            && git(&["reset", "--quiet", "--hard", "FETCH_HEAD"]))
    {
        // Offline: what was fetched last still lists.
    }
    let root = match &source.sub {
        Some(sub) => dir.join(sub),
        None => dir,
    };
    Ok((source, root))
}

/// The plugins catalog `root` offers: its `catalog.toml`, else every
/// directory (in `plugins/` if it has one) with plugin files, described by
/// their titles.
pub fn catalog_entries(root: &Path) -> Result<Vec<CatalogEntry>, String> {
    if let Ok(text) = std::fs::read_to_string(root.join("catalog.toml")) {
        let file: CatalogFile =
            toml::from_str(&text).map_err(|e| format!("catalog.toml: {}", e.message()))?;
        return Ok(file.plugins);
    }
    let base = if root.join("plugins").is_dir() {
        root.join("plugins")
    } else {
        root.to_owned()
    };
    let mut entries = Vec::new();
    for dir in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let path = dir.path();
        let name = dir.file_name().to_string_lossy().into_owned();
        if !path.is_dir() || name.starts_with('.') {
            continue;
        }
        let (found, _) = read_source(&path);
        if found.is_empty() {
            continue;
        }
        let description = found
            .iter()
            .map(|(p, _)| p.title().to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        let source = (base != *root).then(|| format!("plugins/{name}"));
        entries.push(CatalogEntry {
            name,
            description,
            source,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// What `imkr plugin add` is given for `entry` of `catalog`, whose files
/// are in `root`: a directory of the catalog in its repository (or in
/// the directory it is), or the entry's own source.
pub fn entry_source(entry: &CatalogEntry, catalog: &Source, root: &Path) -> String {
    let inside = entry.source.clone().unwrap_or_else(|| entry.name.clone());
    let inside = inside.trim_start_matches("./").to_owned();
    if !root.join(&inside).is_dir() {
        return inside;
    }
    let sub = match &catalog.sub {
        Some(sub) => format!("{sub}/{inside}"),
        None => inside,
    };
    if catalog.git {
        format!("{}#{sub}", catalog.from)
    } else {
        Path::new(&catalog.from)
            .join(sub)
            .to_string_lossy()
            .into_owned()
    }
}

/// The plugin files a source brings: those in its `plugins/` directory if
/// it has one, else at its top; a single file if the source is one.
pub fn source_files(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_owned()];
    }
    let tomls = |dir: &Path| -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        files
    };
    let inner = tomls(&root.join("plugins"));
    if inner.is_empty() { tomls(root) } else { inner }
}

/// What linking a source's files did, see `link_source`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Linked {
    /// The plugins of the source, with their files.
    pub plugins: Vec<(Plugin, PathBuf)>,
    /// Files that are no plugins, and why.
    pub skipped: Vec<(String, String)>,
    /// Names taken by a file that is not this source's.
    pub taken: Vec<String>,
    /// Links removed because their file left the source.
    pub gone: Vec<String>,
}

/// A source's plugins with their files, and its files that are none, with
/// why.
pub type Found = (Vec<(Plugin, PathBuf)>, Vec<(String, String)>);

/// The plugins of the files in `root`, and those that are none.
pub fn read_source(root: &Path) -> Found {
    let mut plugins = Vec::new();
    let mut skipped = Vec::new();
    for file in source_files(root) {
        let name = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match parse(&file) {
            Ok(plugin) => plugins.push((Plugin { name, ..plugin }, file)),
            Err(problem) => skipped.push((name, problem)),
        }
    }
    (plugins, skipped)
}

/// Links each plugin file of the source at `root` into `plugins` as
/// `NAME.toml`, leaving files of other sources and the user's own alone,
/// and removes links into `root` whose file is gone.
pub fn link_source(plugins: &Path, root: &Path) -> std::io::Result<Linked> {
    std::fs::create_dir_all(plugins)?;
    let (found, skipped) = read_source(root);
    let mut linked = Linked {
        skipped,
        ..Linked::default()
    };
    for (plugin, file) in found {
        let link = plugins.join(format!("{}.toml", plugin.name));
        match std::fs::read_link(&link) {
            Ok(target) if target == file => {}
            Ok(target) if target.starts_with(root) => {
                std::fs::remove_file(&link)?;
                std::os::unix::fs::symlink(&file, &link)?;
            }
            Err(_) if !link.exists() && link.symlink_metadata().is_err() => {
                std::os::unix::fs::symlink(&file, &link)?;
            }
            _ => {
                linked.taken.push(plugin.name.clone());
                continue;
            }
        }
        linked.plugins.push((plugin, file));
    }
    for entry in std::fs::read_dir(plugins)?.flatten() {
        let path = entry.path();
        if let Ok(target) = std::fs::read_link(&path)
            && target.starts_with(root)
            && !target.exists()
        {
            std::fs::remove_file(&path)?;
            linked.gone.push(
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        }
    }
    Ok(linked)
}

/// Removes the links into `root` from `plugins`; returns their names.
pub fn unlink_source(plugins: &Path, root: &Path) -> std::io::Result<Vec<String>> {
    let mut removed = Vec::new();
    for entry in std::fs::read_dir(plugins)?.flatten() {
        let path = entry.path();
        if std::fs::read_link(&path).is_ok_and(|target| target.starts_with(root)) {
            std::fs::remove_file(&path)?;
            removed.extend(path.file_stem().map(|s| s.to_string_lossy().into_owned()));
        }
    }
    removed.sort();
    Ok(removed)
}

/// What a plugin will run, to look at before it is added: every command
/// with what starts it.
pub fn describe(plugin: &Plugin) -> Vec<String> {
    let mut lines = vec![format!("{}: {}", plugin.title(), plugin.command)];
    if let Some(status) = &plugin.status {
        lines.push(format!("  status every {}s: {status}", plugin.interval));
    }
    for action in &plugin.actions {
        lines.push(format!(
            "  action {} ({}): {}",
            action.key, action.title, action.run
        ));
    }
    for hook in &plugin.hooks {
        lines.push(format!("  on {}, by itself: {}", hook.on, hook.run));
    }
    lines
}

/// The plugins of this machine's config, see `load_from`.
pub fn load() -> (Vec<Plugin>, Vec<Broken>) {
    match dir() {
        Some(dir) => load_from(&dir),
        None => (Vec::new(), Vec::new()),
    }
}

/// The status lines of each plugin by name, as its `status` command last
/// printed them, at most three.
pub type Statuses = Arc<Mutex<HashMap<String, Vec<String>>>>;

/// Runs each plugin's `status` command every `interval` seconds into
/// `statuses`. A command that fails or hangs past 10 s leaves one dim line
/// saying so; the plugin itself works on.
pub fn watch_statuses(plugins: Arc<Mutex<Vec<Plugin>>>, statuses: Statuses) {
    let _ = thread::Builder::new()
        .name("plugin-status".into())
        .spawn(move || {
            let mut last: HashMap<String, Instant> = HashMap::new();
            loop {
                let due: Vec<(String, String)> = plugins
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|p| {
                        let command = p.status.clone()?;
                        let every = Duration::from_secs(p.interval.max(1));
                        let ran = last.get(&p.name);
                        ran.is_none_or(|at| at.elapsed() >= every)
                            .then(|| (p.name.clone(), command))
                    })
                    .collect();
                for (name, command) in due {
                    last.insert(name.clone(), Instant::now());
                    let lines = run_status(&command);
                    statuses.lock().unwrap().insert(name, lines);
                }
                thread::sleep(Duration::from_secs(1));
            }
        });
}

/// The first three lines `command` prints, or why there are none.
fn run_status(command: &str) -> Vec<String> {
    let child = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(child) = child else {
        return vec!["status command did not start".into()];
    };
    let (send, receive) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = send.send(child.wait_with_output());
    });
    match receive.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(output)) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .take(3)
            .map(str::to_owned)
            .collect(),
        Ok(_) => vec!["status command failed".into()],
        Err(_) => vec!["status command takes too long".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_is_off_unless_asked_for_and_stays_out_of_the_wire() {
        let dir = std::env::temp_dir().join(format!("imkr-plugin-bar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plain.toml"),
            "command = \"tui\"\nstatus = \"tui status\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("shown.toml"),
            "command = \"tui\"\nstatus = \"tui status\"\nbar = true\n",
        )
        .unwrap();
        let (plugins, broken) = load_from(&dir);
        assert!(broken.is_empty(), "{broken:?}");
        let plain = plugins.iter().find(|p| p.name == "plain").unwrap();
        let shown = plugins.iter().find(|p| p.name == "shown").unwrap();
        assert!(!plain.bar);
        assert!(shown.bar);
        // Off is left out of the serialized form, so plugins travelling to
        // an older imkr do not trip its `deny_unknown_fields`.
        let wire = serde_json::to_string(plain).unwrap();
        assert!(!wire.contains("bar"), "{wire}");
        assert!(
            serde_json::to_string(shown)
                .unwrap()
                .contains("\"bar\":true")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_are_told_apart() {
        let source = parse_source("someone/imkr-plugins").unwrap();
        assert_eq!(source.name, "imkr-plugins");
        assert_eq!(source.from, "https://github.com/someone/imkr-plugins");
        assert!(source.git);
        assert_eq!(source.sub, None);
        let source = parse_source("git@github.com:someone/tools.git").unwrap();
        assert_eq!((source.name.as_str(), source.git), ("tools", true));
        let source = parse_source("https://example.com/x/more-plugins/").unwrap();
        assert_eq!(source.name, "more-plugins");
        // A plugin in a directory of a repository, like a catalog's.
        let source = parse_source("someone/catalog/plugins/brain").unwrap();
        assert_eq!(source.from, "https://github.com/someone/catalog");
        assert_eq!(source.sub.as_deref(), Some("plugins/brain"));
        assert_eq!(source.name, "brain");
        let source = parse_source("git@github.com:someone/catalog.git#tui").unwrap();
        assert_eq!(source.from, "git@github.com:someone/catalog.git");
        assert_eq!(
            (source.name.as_str(), source.sub.as_deref()),
            ("tui", Some("tui"))
        );
        assert!(parse_source("someone/catalog/../x").is_err());
        assert!(parse_source("no such thing here").is_err());
        assert!(parse_source("./missing/dir").is_err());
        let dir = std::env::temp_dir();
        let source = parse_source(&dir.to_string_lossy()).unwrap();
        assert!(!source.git);
        assert!(Path::new(&source.from).is_absolute());
    }

    #[test]
    fn catalogs_list_their_plugins() {
        let base = std::env::temp_dir().join(format!("imkr-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // Directories of plugins, no catalog.toml.
        std::fs::create_dir_all(base.join("brain")).unwrap();
        std::fs::create_dir_all(base.join("empty")).unwrap();
        std::fs::write(
            base.join("brain/brain.toml"),
            "title = \"AI Brain\"\ncommand = \"brain\"\n",
        )
        .unwrap();
        let entries = catalog_entries(&base).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].name.as_str(), entries[0].description.as_str()),
            ("brain", "AI Brain")
        );
        let git = Source {
            name: "catalog".into(),
            from: "git@github.com:someone/catalog.git".into(),
            git: true,
            sub: None,
        };
        assert_eq!(
            entry_source(&entries[0], &git, &base),
            "git@github.com:someone/catalog.git#brain"
        );
        let here = Source {
            name: "catalog".into(),
            from: base.to_string_lossy().into_owned(),
            git: false,
            sub: None,
        };
        assert_eq!(
            entry_source(&entries[0], &here, &base),
            base.join("brain").to_string_lossy()
        );
        // A catalog.toml wins, and may point elsewhere.
        std::fs::write(
            base.join("catalog.toml"),
            "[[plugin]]\nname = \"tui\"\ndescription = \"The TUI\"\nsource = \"someone/tui\"\n",
        )
        .unwrap();
        let entries = catalog_entries(&base).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entry_source(&entries[0], &git, &base), "someone/tui");
        std::fs::write(base.join("catalog.toml"), "[[plugin]]\nnam = 1\n").unwrap();
        assert!(catalog_entries(&base).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sources_link_their_plugins_and_take_them_back() {
        let base = std::env::temp_dir().join(format!("imkr-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let plugins = base.join("plugins");
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("plugins")).unwrap();
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(
            repo.join("plugins/deploy.toml"),
            "command = \"{dir}/bin/deploy\"\nkey = \"d\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("plugins/mine.toml"), "command = \"theirs\"\n").unwrap();
        std::fs::write(repo.join("plugins/typo.toml"), "comand = \"x\"\n").unwrap();
        // The user's own plugin of the same name stays.
        std::fs::write(plugins.join("mine.toml"), "command = \"mine\"\n").unwrap();

        let linked = link_source(&plugins, &repo).unwrap();
        let names: Vec<&str> = linked
            .plugins
            .iter()
            .map(|(p, _)| p.name.as_str())
            .collect();
        assert_eq!(names, ["deploy"]);
        assert_eq!(linked.taken, ["mine"]);
        assert_eq!(linked.skipped.len(), 1);
        let (loaded, _) = load_from(&plugins);
        let deploy = loaded.iter().find(|p| p.name == "deploy").unwrap();
        let real = std::fs::canonicalize(repo.join("plugins")).unwrap();
        assert_eq!(
            deploy.command,
            format!(
                "{}/bin/deploy",
                crate::client::sh_quote(&real.to_string_lossy())
            )
        );
        assert_eq!(
            loaded.iter().find(|p| p.name == "mine").unwrap().command,
            "mine"
        );

        // Linking again changes nothing; a file gone from the source goes.
        assert_eq!(link_source(&plugins, &repo).unwrap().taken, ["mine"]);
        std::fs::remove_file(repo.join("plugins/deploy.toml")).unwrap();
        assert_eq!(link_source(&plugins, &repo).unwrap().gone, ["deploy"]);
        std::fs::write(repo.join("plugins/deploy.toml"), "command = \"d\"\n").unwrap();
        link_source(&plugins, &repo).unwrap();
        assert_eq!(unlink_source(&plugins, &repo).unwrap(), ["deploy"]);
        assert!(plugins.join("mine.toml").exists());
        assert!(!plugins.join("deploy.toml").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn plugin_files_load_and_broken_ones_are_named() {
        let dir = std::env::temp_dir().join(format!("imkr-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("peppermint.toml"),
            "title = \"Peppermint\"\ncommand = \"peppermint-tui\"\nkey = \"p\"\nmode = \"fullscreen\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("off.toml"), "command = \"x\"\nenabled = false\n").unwrap();
        std::fs::write(dir.join("broken.toml"), "titel = \"typo\"\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a plugin").unwrap();
        let (plugins, broken) = load_from(&dir);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "peppermint");
        assert_eq!(plugins[0].title(), "Peppermint");
        assert_eq!(plugins[0].key, Some('p'));
        assert_eq!(plugins[0].mode, Mode::Fullscreen);
        assert_eq!(plugins[0].interval, 30);
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].name, "broken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_bring_actions_and_hooks() {
        let plugin: Plugin = toml::from_str(
            "command = \"tui\"\n\n[[actions]]\nkey = \"d\"\ntitle = \"Deploy\"\nrun = \"tui deploy\"\n\n[[hooks]]\non = \"done\"\nrun = \"tui notify\"\n",
        )
        .unwrap();
        assert_eq!(plugin.actions[0].key, 'd');
        assert!(!plugin.actions[0].popup);
        assert_eq!(plugin.hooks[0].on, "done");
        // A typo is an error, not a plugin that silently does less.
        assert!(toml::from_str::<Plugin>("command = \"x\"\n[[actions]]\nkey = \"d\"\ntitle = \"D\"\nrun = \"y\"\npopop = true\n").is_err());
    }

    #[test]
    fn status_lines_are_the_first_three_printed() {
        assert_eq!(
            run_status("printf 'a\\n\\nb\\nc\\nd\\n'"),
            vec!["a", "b", "c"]
        );
        assert_eq!(run_status("exit 3"), vec!["status command failed"]);
    }
}
