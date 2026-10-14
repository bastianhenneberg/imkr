//! Settings shared between machines (`imkr settings sync`): the config
//! without what belongs to one machine, session templates, saved layouts
//! and the user's own plugins. Each file goes where it is older or
//! missing; the one replaced is backed up first.

use std::collections::BTreeMap;
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

/// A file to copy, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub path: String,
    pub way: Way,
    pub why: &'static str,
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

/// What a file says, apart from what belongs to one machine.
fn shared_text(item: &Item) -> String {
    if item.path != CONFIG {
        return item.text.clone();
    }
    match item.text.parse::<toml::Table>() {
        Ok(mut table) => {
            for key in LOCAL_KEYS {
                table.remove(*key);
            }
            toml::to_string(&table).unwrap_or_else(|_| item.text.clone())
        }
        Err(_) => item.text.clone(),
    }
}

/// What to copy between here (`local`) and another machine (`remote`):
/// the newer of two that differ, and whatever only one side has. Files of
/// the same age that differ are left alone.
pub fn plan(local: &[Item], remote: &[Item]) -> (Vec<Step>, Vec<String>) {
    let mut paths: BTreeMap<&str, (Option<&Item>, Option<&Item>)> = BTreeMap::new();
    for item in local {
        paths.entry(&item.path).or_default().0 = Some(item);
    }
    for item in remote {
        paths.entry(&item.path).or_default().1 = Some(item);
    }
    let mut steps = Vec::new();
    let mut undecided = Vec::new();
    for (path, sides) in paths {
        let step = |way, why| Step {
            path: path.to_owned(),
            way,
            why,
        };
        match sides {
            (Some(_), None) => steps.push(step(Way::Push, "only here")),
            (None, Some(_)) => steps.push(step(Way::Pull, "only there")),
            (Some(here), Some(there)) => {
                if shared_text(here) == shared_text(there) {
                    continue;
                }
                match here.modified.cmp(&there.modified) {
                    std::cmp::Ordering::Greater => steps.push(step(Way::Push, "newer here")),
                    std::cmp::Ordering::Less => steps.push(step(Way::Pull, "newer there")),
                    std::cmp::Ordering::Equal => undecided.push(path.to_owned()),
                }
            }
            (None, None) => {}
        }
    }
    (steps, undecided)
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

/// The files of `items` that `steps` move the given way.
pub fn picked<'a>(items: &'a [Item], steps: &[Step], way: Way) -> Vec<&'a Item> {
    steps
        .iter()
        .filter(|s| s.way == way)
        .filter_map(|s| items.iter().find(|i| i.path == s.path))
        .collect()
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

    #[test]
    fn newer_wins_and_missing_ones_spread() {
        let here = [
            item("config.toml", 20, "theme = \"a\"\nname = \"desk\"\n"),
            item("templates/coding.toml", 5, "x"),
            item("plugins/mine.toml", 9, "same"),
        ];
        let there = [
            item("config.toml", 10, "theme = \"b\"\nname = \"server\"\n"),
            item("templates/coding.toml", 8, "y"),
            item("plugins/mine.toml", 1, "same"),
            item("layouts.json", 3, "{}"),
        ];
        let (steps, undecided) = plan(&here, &there);
        assert!(undecided.is_empty());
        let got: Vec<(&str, Way)> = steps.iter().map(|s| (s.path.as_str(), s.way)).collect();
        assert_eq!(
            got,
            [
                ("config.toml", Way::Push),
                ("layouts.json", Way::Pull),
                ("templates/coding.toml", Way::Pull),
            ]
        );
    }

    #[test]
    fn machine_keys_alone_are_no_difference() {
        let here = [item(
            "config.toml",
            20,
            "theme = \"a\"\nname = \"desk\"\nhosts = [\"s\"]\n",
        )];
        let there = [item(
            "config.toml",
            10,
            "theme = \"a\"\nname = \"server\"\n",
        )];
        assert!(plan(&here, &there).0.is_empty());
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
