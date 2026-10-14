//! Settings shared between machines (`imkr settings sync`): the config
//! without what belongs to one machine, session templates, saved layouts
//! and the user's own plugins. Against the state both had after the last
//! sync (the base), what one side changed goes to the other: the config
//! setting by setting, the other files whole. What both changed: the
//! config keeps this machine's value, a file the newer one. Without a
//! base, this machine's config values win and files the newer one. The
//! file replaced is backed up first.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Config keys of one machine: they never travel, and the ones there stay.
pub const LOCAL_KEYS: &[&str] = &["name", "hosts", "web", "web_listen"];

/// The config, as an item's path, whatever the file is called here.
const CONFIG: &str = "config.toml";
const LAYOUTS: &str = "layouts.json";

/// One settings file: its path relative to the config's folder, when it
/// last changed (seconds since the epoch) and its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub path: String,
    pub modified: u64,
    pub text: String,
}

/// Which way a file goes in a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// From the other machine to this one.
    Pull,
    /// From this machine to the other one.
    Push,
}

/// A file to copy, and why. `text` is set for the config: the merged
/// settings to write instead of either side's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub path: String,
    pub way: Way,
    pub why: String,
    pub text: Option<String>,
}

/// The settings files here, as `imkr settings export` sends them.
pub fn collect() -> Result<Vec<Item>> {
    let config = crate::config::path()?;
    let dir = config.parent().context("the config has no folder")?;
    Ok(collect_in(dir, &config))
}

fn collect_in(dir: &Path, config: &Path) -> Vec<Item> {
    let mut items = Vec::new();
    let mut add = |path: &str, file: &Path| {
        // Links are plugins from a source (`imkr plugin add`), which every
        // machine installs for itself.
        let Ok(meta) = fs::symlink_metadata(file) else {
            return;
        };
        if !meta.is_file() {
            return;
        }
        let Ok(text) = fs::read_to_string(file) else {
            return;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        items.push(Item {
            path: path.to_owned(),
            modified,
            text,
        });
    };
    add(CONFIG, config);
    add(LAYOUTS, &dir.join(LAYOUTS));
    for folder in ["templates", "plugins"] {
        let Ok(entries) = fs::read_dir(dir.join(folder)) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| allowed(&format!("{folder}/{name}")))
            .collect();
        names.sort();
        for name in names {
            add(&format!("{folder}/{name}"), &dir.join(folder).join(&name));
        }
    }
    items
}

/// Whether an item may be written here: only the files a sync is about,
/// never a path out of the config's folder.
pub fn allowed(path: &str) -> bool {
    if path == CONFIG || path == LAYOUTS {
        return true;
    }
    let Some((folder, name)) = path.split_once('/') else {
        return false;
    };
    matches!(folder, "templates" | "plugins")
        && name.ends_with(".toml")
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && name != ".toml"
}

/// What to copy between here (`local`) and another machine (`remote`),
/// given their state after the last sync (`base`, `None` before the
/// first). Also returns the files both changed to the same age, left
/// alone.
pub fn plan(local: &[Item], remote: &[Item], base: Option<&[Item]>) -> (Vec<Step>, Vec<String>) {
    let find = |items: &[Item], path: &str| items.iter().find(|i| i.path == path).cloned();
    let mut paths: Vec<&str> = local
        .iter()
        .chain(remote)
        .map(|i| i.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let mut steps = Vec::new();
    let mut undecided = Vec::new();
    for path in paths {
        let (here, there) = (find(local, path), find(remote, path));
        let was = base.and_then(|b| find(b, path));
        if path == CONFIG {
            steps.extend(config_steps(
                here.as_ref(),
                there.as_ref(),
                was.as_ref(),
                base.is_some(),
            ));
            continue;
        }
        let step = |way, why: &str| Step {
            path: path.to_owned(),
            way,
            why: why.to_owned(),
            text: None,
        };
        match (here, there) {
            (Some(_), None) => steps.push(step(Way::Push, "only here")),
            (None, Some(_)) => steps.push(step(Way::Pull, "only there")),
            (Some(here), Some(there)) => {
                if here.text == there.text {
                    continue;
                }
                match was.map(|w| w.text) {
                    Some(w) if w == here.text => steps.push(step(Way::Pull, "changed there")),
                    Some(w) if w == there.text => steps.push(step(Way::Push, "changed here")),
                    _ => match here.modified.cmp(&there.modified) {
                        std::cmp::Ordering::Greater => steps.push(step(Way::Push, "newer here")),
                        std::cmp::Ordering::Less => steps.push(step(Way::Pull, "newer there")),
                        std::cmp::Ordering::Equal => undecided.push(path.to_owned()),
                    },
                }
            }
            (None, None) => {}
        }
    }
    (steps, undecided)
}

/// The shared settings of a config: without this machine's own keys.
fn shared_table(item: Option<&Item>) -> toml::Table {
    let mut table: toml::Table = item.and_then(|i| i.text.parse().ok()).unwrap_or_default();
    for key in LOCAL_KEYS {
        table.remove(*key);
    }
    table
}

/// The config, setting by setting: what one side changed since `was`
/// wins; what both changed, or without a base what differs, keeps this
/// machine's value. Steps to write the result where it differs.
fn config_steps(
    here: Option<&Item>,
    there: Option<&Item>,
    was: Option<&Item>,
    synced: bool,
) -> Vec<Step> {
    let (mine, theirs, base) = (shared_table(here), shared_table(there), shared_table(was));
    let mut merged = mine.clone();
    let mut taken = Vec::new();
    let mut kept = Vec::new();
    let mut keys: Vec<&String> = mine.keys().chain(theirs.keys()).collect();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        let (m, t, b) = (mine.get(key), theirs.get(key), base.get(key));
        if m == t {
            continue;
        }
        if synced && m == b {
            match t {
                Some(value) => merged.insert(key.clone(), value.clone()),
                None => merged.remove(key),
            };
            taken.push(key.clone());
        } else {
            kept.push(key.clone());
        }
    }
    let text = toml::to_string(&merged).unwrap_or_default();
    let mut steps = Vec::new();
    if here.is_some() && merged != mine {
        steps.push(Step {
            path: CONFIG.to_owned(),
            way: Way::Pull,
            why: format!("changed there: {}", taken.join(", ")),
            text: Some(text.clone()),
        });
    }
    if there.is_some() && merged != theirs || there.is_none() && here.is_some() {
        let why = match (kept.is_empty(), synced) {
            (true, _) => "only here".to_owned(),
            (false, true) => format!("changed here: {}", kept.join(", ")),
            (false, false) => format!("first sync, this machine's: {}", kept.join(", ")),
        };
        steps.push(Step {
            path: CONFIG.to_owned(),
            way: Way::Push,
            why,
            text: Some(text),
        });
    }
    steps
}

/// Writes `items` here, each after a backup of the file it replaces, and
/// keeps their age. The config keeps this machine's own keys. Returns the
/// paths written.
pub fn apply(items: &[Item]) -> Result<Vec<String>> {
    let config = crate::config::path()?;
    let dir = config.parent().context("the config has no folder")?;
    apply_in(dir, &config, items)
}

fn apply_in(dir: &Path, config: &Path, items: &[Item]) -> Result<Vec<String>> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backup = dir.join(".sync-backup").join(stamp.to_string());
    let mut written = Vec::new();
    for item in items {
        if !allowed(&item.path) {
            bail!("refusing to write {}: not a settings file", item.path);
        }
        let target = if item.path == CONFIG {
            config.to_path_buf()
        } else {
            dir.join(&item.path)
        };
        let text = if item.path == CONFIG {
            let here = fs::read_to_string(&target).unwrap_or_default();
            let merged = merge_config(&here, &item.text)?;
            crate::config::Config::parse(&merged).with_context(|| {
                format!("the config from there does not load here: {}", item.path)
            })?;
            merged
        } else {
            item.text.clone()
        };
        if target.exists() {
            let copy = backup.join(&item.path);
            if let Some(parent) = copy.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&target, &copy)
                .with_context(|| format!("could not back up {}", target.display()))?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, text)
            .with_context(|| format!("could not write {}", target.display()))?;
        let when = UNIX_EPOCH + Duration::from_secs(item.modified);
        if let Ok(file) = fs::File::options().write(true).open(&target) {
            let _ = file.set_modified(when);
        }
        written.push(item.path.clone());
    }
    Ok(written)
}

/// The config from there with this machine's own keys from `here`.
fn merge_config(here: &str, there: &str) -> Result<String> {
    let mut table: toml::Table = there.parse().context("the config from there is not TOML")?;
    let mine: toml::Table = here.parse().unwrap_or_default();
    for key in LOCAL_KEYS {
        table.remove(*key);
        if let Some(value) = mine.get(*key) {
            table.insert((*key).to_owned(), value.clone());
        }
    }
    Ok(toml::to_string(&table)?)
}

/// The files that `steps` move the given way, from `items`; the config as
/// merged.
pub fn picked(items: &[Item], steps: &[Step], way: Way) -> Vec<Item> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    steps
        .iter()
        .filter(|s| s.way == way)
        .filter_map(|s| match &s.text {
            Some(text) => Some(Item {
                path: s.path.clone(),
                modified: now,
                text: text.clone(),
            }),
            None => items.iter().find(|i| i.path == s.path).cloned(),
        })
        .collect()
}

/// The state both had after the last sync with `host`, kept here.
pub fn base(host: &str) -> Option<Vec<Item>> {
    let text = fs::read_to_string(base_file(host).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remembers the settings here as the state after a sync with `host`.
pub fn remember_base(host: &str) -> Result<()> {
    let file = base_file(host)?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&file, serde_json::to_string(&collect()?)?)?;
    Ok(())
}

fn base_file(host: &str) -> Result<std::path::PathBuf> {
    let config = crate::config::path()?;
    let dir = config.parent().context("the config has no folder")?;
    let name: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.@".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir.join(".sync-base").join(format!("{name}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str, modified: u64, text: &str) -> Item {
        Item {
            path: path.into(),
            modified,
            text: text.into(),
        }
    }

    fn paths(steps: &[Step]) -> Vec<(&str, Way)> {
        steps.iter().map(|s| (s.path.as_str(), s.way)).collect()
    }

    #[test]
    fn files_follow_the_side_that_changed_them() {
        let base = [
            item("templates/a.toml", 1, "old"),
            item("templates/b.toml", 1, "old"),
        ];
        let here = [
            item("templates/a.toml", 1, "old"),
            item("templates/b.toml", 5, "mine"),
            item("plugins/p.toml", 9, "p"),
        ];
        let there = [
            item("templates/a.toml", 3, "theirs"),
            item("templates/b.toml", 9, "old"),
            item("layouts.json", 3, "{}"),
        ];
        let (steps, undecided) = plan(&here, &there, Some(&base));
        assert!(undecided.is_empty());
        // b is newer there, but only here was it changed.
        assert_eq!(
            paths(&steps),
            [
                ("layouts.json", Way::Pull),
                ("plugins/p.toml", Way::Push),
                ("templates/a.toml", Way::Pull),
                ("templates/b.toml", Way::Push),
            ]
        );
    }

    #[test]
    fn config_merges_setting_by_setting() {
        let base = [item("config.toml", 1, "a = 1\nb = 1\nc = 1\n")];
        let here = [item(
            "config.toml",
            1,
            "a = 2\nb = 1\nc = 1\nname = \"desk\"\n",
        )];
        let there = [item(
            "config.toml",
            9,
            "a = 1\nb = 3\nc = 1\nname = \"srv\"\n",
        )];
        let (steps, _) = plan(&here, &there, Some(&base));
        assert_eq!(
            paths(&steps),
            [("config.toml", Way::Pull), ("config.toml", Way::Push)]
        );
        let merged: toml::Table = steps[0].text.as_deref().unwrap().parse().unwrap();
        assert_eq!(merged["a"].as_integer(), Some(2));
        assert_eq!(merged["b"].as_integer(), Some(3));
        assert!(merged.get("name").is_none());
    }

    #[test]
    fn first_sync_keeps_this_machines_settings() {
        let here = [item(
            "config.toml",
            1,
            "catalogs = [\"x\"]\nname = \"desk\"\n",
        )];
        let there = [item("config.toml", 9, "catalogs = []\nname = \"srv\"\n")];
        let (steps, _) = plan(&here, &there, None);
        assert_eq!(paths(&steps), [("config.toml", Way::Push)]);
        assert!(steps[0].text.as_deref().unwrap().contains("\"x\""));
    }

    #[test]
    fn merged_config_keeps_this_machines_keys() {
        let merged = merge_config(
            "name = \"desk\"\nhosts = [\"server\"]\ntheme = \"old\"\n",
            "name = \"server\"\nweb = true\ntheme = \"new\"\n",
        )
        .unwrap();
        let table: toml::Table = merged.parse().unwrap();
        assert_eq!(table["name"].as_str(), Some("desk"));
        assert_eq!(table["theme"].as_str(), Some("new"));
        assert!(table.get("web").is_none());
        assert_eq!(table["hosts"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn only_settings_files_are_written() {
        assert!(allowed("config.toml"));
        assert!(allowed("templates/coding.toml"));
        assert!(allowed("plugins/peppermint.toml"));
        assert!(!allowed("../.bashrc"));
        assert!(!allowed("templates/../../x.toml"));
        assert!(!allowed("plugins/.sources/x.toml"));
        assert!(!allowed("plugins/.hidden.toml"));
        assert!(!allowed("templates/notes.txt"));
        assert!(!allowed("/etc/passwd"));
    }

    #[test]
    fn apply_backs_up_and_keeps_age() {
        let root = std::env::temp_dir().join(format!("imkr-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("templates")).unwrap();
        let config = root.join("config.toml");
        fs::write(&config, "name = \"desk\"\ntheme = \"old\"\n").unwrap();
        fs::write(root.join("templates/a.toml"), "old").unwrap();
        let written = apply_in(
            &root,
            &config,
            &[
                item(
                    "config.toml",
                    1_000_000,
                    "name = \"server\"\ntheme = \"new\"\n",
                ),
                item("templates/a.toml", 2_000_000, "new"),
            ],
        )
        .unwrap();
        assert_eq!(written.len(), 2);
        let now = fs::read_to_string(&config).unwrap();
        assert!(now.contains("desk") && now.contains("new"), "{now}");
        assert_eq!(
            fs::read_to_string(root.join("templates/a.toml")).unwrap(),
            "new"
        );
        let age = fs::metadata(root.join("templates/a.toml"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(age, UNIX_EPOCH + Duration::from_secs(2_000_000));
        let backups: Vec<_> = fs::read_dir(root.join(".sync-backup")).unwrap().collect();
        assert_eq!(backups.len(), 1);
        let collected = collect_in(&root, &config);
        assert_eq!(collected.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}
