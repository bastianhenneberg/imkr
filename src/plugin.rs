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
//! ```
//!
//! A plugin is a program like any other, so it can be written in any
//! language and cannot take imkr down with it. Broken files are listed with
//! what is wrong, the rest work.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Over the active window, most of its size.
    #[default]
    Popup,
    /// Over everything but the bars.
    Fullscreen,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    /// The file's name without `.toml`, also the popup's name.
    #[serde(skip)]
    pub name: String,
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
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| toml::from_str::<Plugin>(&text).map_err(|e| e.message().to_owned()));
        match parsed {
            Ok(plugin) if !plugin.enabled => {}
            Ok(plugin) => plugins.push(Plugin { name, ..plugin }),
            Err(problem) => broken.push(Broken { name, problem }),
        }
    }
    (plugins, broken)
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
    fn status_lines_are_the_first_three_printed() {
        assert_eq!(
            run_status("printf 'a\\n\\nb\\nc\\nd\\n'"),
            vec!["a", "b", "c"]
        );
        assert_eq!(run_status("exit 3"), vec!["status command failed"]);
    }
}
