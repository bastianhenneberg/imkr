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
}

impl Source {
    /// Where its files are.
    pub fn root(&self, plugins: &Path) -> PathBuf {
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
/// git URL, or `owner/repo` on GitHub. Returns the source's name, where
/// it is and whether it is a git repository.
pub fn parse_source(given: &str) -> Result<(String, String, bool), String> {
    let path = Path::new(given);
    if path.exists() {
        let real = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let name = real
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok((name, real.to_string_lossy().into_owned(), false));
    }
    let url = if given.contains("://") || given.starts_with("git@") {
        given.to_owned()
    } else if given.split('/').count() == 2 && !given.starts_with('.') && !given.contains(' ') {
        format!("https://github.com/{given}")
    } else {
        return Err(format!("{given} is no directory, git URL or owner/repo"));
    };
    let name = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_owned();
    if name.is_empty() || name.starts_with('.') {
        return Err(format!("cannot name a plugin source after {given}"));
    }
    Ok((name, url, true))
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
    fn sources_are_told_apart() {
        let (name, from, git) = parse_source("someone/imkr-plugins").unwrap();
        assert_eq!(name, "imkr-plugins");
        assert_eq!(from, "https://github.com/someone/imkr-plugins");
        assert!(git);
        let (name, _, git) = parse_source("git@github.com:someone/tools.git").unwrap();
        assert_eq!((name.as_str(), git), ("tools", true));
        let (name, _, _) = parse_source("https://example.com/x/more-plugins/").unwrap();
        assert_eq!(name, "more-plugins");
        assert!(parse_source("no such thing here").is_err());
        assert!(parse_source("./missing/dir").is_err());
        let dir = std::env::temp_dir();
        let (_, from, git) = parse_source(&dir.to_string_lossy()).unwrap();
        assert!(!git);
        assert!(Path::new(&from).is_absolute());
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
