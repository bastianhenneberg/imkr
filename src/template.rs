//! Session templates: the windows a new session starts with, their splits,
//! directories and the commands typed into them. Each is a TOML file in
//! `templates/` next to the config:
//!
//! ```toml
//! title = "Coding"
//! root = "~/Development/{session}"   # {session} is the new session's name
//!
//! [[window]]
//! name = "claude"
//! command = "claude"
//!
//! [[window]]
//! name = "terminal"
//! [[window.pane]]
//! command = "nvim"
//! [[window.pane]]
//! split = "right"   # or "down", of the pane before, or of pane `of` (1, 2, …)
//! size = 40         # percent the new pane gets
//! cwd = "src"       # relative to root
//! ```
//!
//! A template becomes the same layout a restart or a hive brings back, so
//! it starts on any machine the way those do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::{Axis, PaneId};
use crate::persist::{Saved, SavedPane};
use crate::plugin::Broken;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// The file's name without `.toml`. Set from the file name when loading.
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The session's directory; relative directories below start there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, rename = "window")]
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// For a window of one pane: what to run in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, rename = "pane", skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<TemplatePane>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplatePane {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitTo>,
    /// The pane split, counted from 1; the one before by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<usize>,
    /// Percent of the split pane's space the new one gets, 50 by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitTo {
    #[default]
    Right,
    Down,
}

impl Template {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }

    /// What the windows hold, for lists: "claude · terminal (2)".
    pub fn summary(&self) -> String {
        self.windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let name = w
                    .name
                    .clone()
                    .or_else(|| w.command.clone())
                    .unwrap_or_else(|| format!("{}", i + 1));
                match w.panes.len() {
                    0 | 1 => name,
                    n => format!("{name} ({n})"),
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The layout of session `session` made from this template. Its root
    /// is the template's, else `fallback`, else the home directory.
    pub fn to_saved(&self, session: &str, fallback: Option<&Path>) -> Saved {
        let root = match &self.root {
            Some(root) => Some(PathBuf::from(root.replace("{session}", session))),
            None => fallback.map(Path::to_path_buf),
        };
        let mut next: PaneId = 0;
        let mut workspaces = BTreeMap::new();
        let mut panes = BTreeMap::new();
        let one = [TemplatePane::default()];
        for (number, window) in (1..=u8::MAX).zip(&self.windows) {
            let specs: &[TemplatePane] = if window.panes.is_empty() {
                &one
            } else {
                &window.panes
            };
            let mut ids: Vec<PaneId> = Vec::new();
            let mut ws: Option<Workspace> = None;
            for (i, spec) in specs.iter().enumerate() {
                let id = next;
                next += 1;
                match &mut ws {
                    None => ws = Some(Workspace::new(id)),
                    Some(ws) => {
                        let of = spec
                            .of
                            .and_then(|n| ids.get(n.checked_sub(1)?))
                            .unwrap_or(&ids[i - 1]);
                        let axis = match spec.split.unwrap_or_default() {
                            SplitTo::Right => Axis::Row,
                            SplitTo::Down => Axis::Column,
                        };
                        let size = f32::from(spec.size.unwrap_or(50).clamp(5, 95));
                        ws.layout.split_at(*of, axis, id, 1.0 - size / 100.0);
                    }
                }
                ids.push(id);
                let cwd = spec.cwd.as_deref().or(window.cwd.as_deref());
                let command = if window.panes.is_empty() {
                    window.command.clone()
                } else {
                    spec.command.clone()
                };
                panes.insert(
                    id,
                    SavedPane {
                        cwd: match cwd {
                            Some(cwd) => Some(expand(cwd, root.as_deref())),
                            None => root.clone(),
                        },
                        name: spec.name.clone(),
                        command: command.filter(|c| !c.trim().is_empty()),
                        ..Default::default()
                    },
                );
            }
            if let Some(mut ws) = ws {
                ws.name = window.name.clone();
                ws.focus(ids[0]);
                workspaces.insert(number, ws);
            }
        }
        Saved {
            template: Some(self.title().to_owned()),
            ..Saved::new(1, workspaces, panes)
        }
    }

    /// A template of a running session's layout: its windows and splits,
    /// directories below the one they share, agents as commands. A root
    /// ending in the session's name becomes `{session}`.
    pub fn from_saved(name: &str, session: &str, saved: &Saved) -> Self {
        let cwds: Vec<&Path> = saved
            .panes
            .values()
            .filter_map(|p| p.cwd.as_deref())
            .collect();
        let root = common_dir(&cwds);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let windows = saved
            .workspaces
            .values()
            .filter_map(|ws| {
                let (first, splits) = ws.layout.splits()?;
                let mut order = vec![first];
                order.extend(splits.iter().map(|s| s.new));
                let index = |id: PaneId| order.iter().position(|&o| o == id).map(|i| i + 1);
                let mut panes = Vec::new();
                for (i, &id) in order.iter().enumerate() {
                    let spec = saved.panes.get(&id).cloned().unwrap_or_default();
                    let mut pane = TemplatePane {
                        name: spec.name.clone(),
                        command: spec.command.clone().or(spec.agent.clone()),
                        cwd: spec
                            .cwd
                            .as_deref()
                            .and_then(|cwd| relative(cwd, root.as_deref())),
                        ..Default::default()
                    };
                    if i > 0 {
                        let split = splits[i - 1];
                        pane.split = Some(match split.axis {
                            Axis::Row => SplitTo::Right,
                            Axis::Column => SplitTo::Down,
                        });
                        let size = ((1.0 - split.ratio) * 100.0).round() as u8;
                        pane.size = (size != 50).then_some(size);
                        pane.of = index(split.of).filter(|&of| of != i);
                    }
                    panes.push(pane);
                }
                let window = match panes.as_slice() {
                    [only] if only.name.is_none() => Window {
                        name: ws.name.clone(),
                        cwd: only.cwd.clone(),
                        command: only.command.clone(),
                        panes: Vec::new(),
                    },
                    _ => Window {
                        name: ws.name.clone(),
                        panes,
                        ..Default::default()
                    },
                };
                Some(window)
            })
            .collect();
        let root = root.map(|root| {
            if root.file_name().is_some_and(|n| n == session) {
                let parent = root.parent().unwrap_or(Path::new("/"));
                format!("{}/{{session}}", tilde(parent, home.as_deref()))
            } else {
                tilde(&root, home.as_deref())
            }
        });
        Template {
            name: name.to_owned(),
            title: None,
            root,
            windows,
        }
    }
}

/// `path`, below `base` when relative. `~` stays: the session may start
/// on another machine, with another home, which `home_dir` resolves.
fn expand(path: &str, base: Option<&Path>) -> PathBuf {
    let path = Path::new(path);
    match base {
        Some(base) if path.is_relative() && !path.starts_with("~") => base.join(path),
        _ => path.to_path_buf(),
    }
}

/// `dir` with a leading `~` replaced by this machine's home.
pub fn home_dir(dir: &Path) -> PathBuf {
    match (dir.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => dir.to_path_buf(),
    }
}

/// A typed directory split where the last name starts: the directory to
/// list, with its `/`, and the start of the name typed in it.
pub fn split_dir(text: &str) -> (&str, &str) {
    match text.rfind('/') {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    }
}

/// The directories in `parent`, as `split_dir` gives it, on this machine,
/// sorted; `None` when it cannot be read.
pub fn subdirs(parent: &str) -> Option<Vec<String>> {
    let listed = if parent.is_empty() {
        PathBuf::from(".")
    } else {
        home_dir(Path::new(parent))
    };
    let mut names: Vec<String> = std::fs::read_dir(listed)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Some(names)
}

/// Those of `names` that `prefix` starts, hidden ones only when it does
/// with a dot.
pub fn matching<'a>(names: &'a [String], prefix: &str) -> Vec<&'a String> {
    names
        .iter()
        .filter(|n| n.starts_with(prefix) && (prefix.starts_with('.') || !n.starts_with('.')))
        .collect()
}

/// `text` completed from `names`, the directories where it is typed, like
/// a shell's Tab: the one it starts, with a `/`, or what all of them share.
pub fn complete_with(text: &str, names: &[String]) -> String {
    if text == "~" {
        return "~/".into();
    }
    let (parent, prefix) = split_dir(text);
    match matching(names, prefix).as_slice() {
        [] => text.to_owned(),
        [one] => format!("{parent}{one}/"),
        [first, rest @ ..] => {
            let mut shared = first.to_string();
            for name in rest {
                let len = shared
                    .chars()
                    .zip(name.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                shared = shared.chars().take(len).collect();
            }
            format!("{parent}{shared}")
        }
    }
}

/// `text` completed as a directory on this machine, see `complete_with`.
/// `~` stays as typed.
#[cfg(test)]
pub fn complete_dir(text: &str) -> String {
    let names = subdirs(split_dir(text).0).unwrap_or_default();
    complete_with(text, &names)
}

/// `path` with the home directory as `~`, as typed in a prompt.
pub fn with_tilde(path: &Path) -> String {
    tilde(path, std::env::var_os("HOME").as_deref().map(Path::new))
}

fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `path` below `root`, `None` when it is `root` itself.
fn relative(path: &Path, root: Option<&Path>) -> Option<String> {
    match root.and_then(|root| path.strip_prefix(root).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => None,
        Some(rest) => Some(rest.display().to_string()),
        None => Some(path.display().to_string()),
    }
}

/// The deepest directory all of `dirs` are in.
fn common_dir(dirs: &[&Path]) -> Option<PathBuf> {
    let (first, rest) = dirs.split_first()?;
    let mut common = first.to_path_buf();
    for dir in rest {
        while !dir.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// Where template files live: `templates/` next to the config file.
pub fn dir() -> Option<PathBuf> {
    Some(crate::config::path().ok()?.parent()?.join("templates"))
}

/// The templates in `dir`, by name, and the files that are broken.
pub fn load_from(dir: &Path) -> (Vec<Template>, Vec<Broken>) {
    let mut templates = Vec::new();
    let mut broken = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (templates, broken);
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
            .and_then(|text| toml::from_str::<Template>(&text).map_err(|e| e.message().to_owned()));
        match parsed {
            Ok(template) => templates.push(Template { name, ..template }),
            Err(problem) => broken.push(Broken { name, problem }),
        }
    }
    (templates, broken)
}

/// The templates of this machine's config, see `load_from`.
pub fn load() -> (Vec<Template>, Vec<Broken>) {
    match dir() {
        Some(dir) => load_from(&dir),
        None => (Vec::new(), Vec::new()),
    }
}

/// Writes `template` as `templates/<name>.toml`, returning the path.
pub fn write(template: &Template) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let dir = dir().context("no config directory")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", template.name));
    let text = toml::to_string_pretty(&Template {
        name: String::new(),
        ..template.clone()
    })?;
    // The name comes from the file, not from a line in it.
    let text = text.trim_start_matches("name = \"\"\n").to_owned();
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_complete_like_a_shell() {
        let dir = std::env::temp_dir().join(format!("imkr-complete-{}", std::process::id()));
        for sub in ["alpha", "alpine", "beta", ".hidden"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("alfile"), "").unwrap();
        let base = format!("{}/", dir.display());
        // Shared start, then the only one, files left out.
        assert_eq!(complete_dir(&format!("{base}al")), format!("{base}alp"));
        assert_eq!(
            complete_dir(&format!("{base}alph")),
            format!("{base}alpha/")
        );
        assert_eq!(complete_dir(&format!("{base}b")), format!("{base}beta/"));
        assert_eq!(complete_dir(&format!("{base}x")), format!("{base}x"));
        assert_eq!(
            complete_dir(&format!("{base}.h")),
            format!("{base}.hidden/")
        );
        assert_eq!(complete_dir("~"), "~/");
        let _ = std::fs::remove_dir_all(dir);
    }

    const CODING: &str = r#"
title = "Coding"
root = "/work/{session}"

[[window]]
name = "claude"
command = "claude"

[[window]]
name = "terminal"
[[window.pane]]
command = "nvim"
[[window.pane]]
split = "right"
size = 40
cwd = "src"
[[window.pane]]
split = "down"
of = 1
"#;

    #[test]
    fn a_template_becomes_a_layout_in_its_root() {
        let template: Template = toml::from_str(CODING).unwrap();
        assert_eq!(template.summary(), "claude · terminal (3)");
        let saved = template.to_saved("imkr", None);
        assert_eq!(saved.workspaces.len(), 2);
        assert_eq!(saved.workspaces[&1].name.as_deref(), Some("claude"));
        assert_eq!(saved.panes[&0].command.as_deref(), Some("claude"));
        assert_eq!(
            saved.panes[&0].cwd.as_deref(),
            Some(Path::new("/work/imkr"))
        );
        let terminal = &saved.workspaces[&2];
        assert_eq!(terminal.panes().len(), 3);
        assert_eq!(saved.panes[&1].command.as_deref(), Some("nvim"));
        assert_eq!(
            saved.panes[&2].cwd.as_deref(),
            Some(Path::new("/work/imkr/src"))
        );
        assert_eq!(saved.panes[&3].command, None);
        // Pane 3 splits pane 1 (nvim) downwards, pane 2 is to their right.
        let (first, splits) = terminal.layout.splits().unwrap();
        assert_eq!(first, 1);
        assert_eq!(splits[0].new, 2);
        assert!((splits[0].ratio - 0.6).abs() < 0.01);
        assert_eq!((splits[1].of, splits[1].new), (1, 3));
    }

    #[test]
    fn a_running_layout_becomes_the_same_template_again() {
        let template: Template = toml::from_str(CODING).unwrap();
        let saved = template.to_saved("imkr", None);
        let back = Template::from_saved("coding", "imkr", &saved);
        assert_eq!(back.root.as_deref(), Some("/work/{session}"));
        let again = back.to_saved("imkr", None);
        assert_eq!(again.template.as_deref(), Some("coding"));
        assert_eq!(
            (again.workspaces, again.panes),
            (saved.workspaces, saved.panes)
        );
        assert_eq!(
            back.to_saved("api", None).panes[&2].cwd.as_deref(),
            Some(Path::new("/work/api/src"))
        );
    }

    #[test]
    fn home_stays_for_the_machine_the_session_starts_on() {
        let template: Template =
            toml::from_str("root = \"~/dev/{session}\"\n[[window]]\ncwd = \"src\"\n").unwrap();
        let cwd = template.to_saved("api", None).panes[&0]
            .cwd
            .clone()
            .unwrap();
        assert_eq!(cwd, Path::new("~/dev/api/src"));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(home_dir(&cwd), Path::new(&home).join("dev/api/src"));
        assert_eq!(home_dir(Path::new("/abs")), Path::new("/abs"));
    }

    #[test]
    fn without_a_root_the_fallback_is_the_root() {
        let template: Template = toml::from_str("[[window]]\ncwd = \"x\"\n").unwrap();
        let saved = template.to_saved("s", Some(Path::new("/here")));
        assert_eq!(saved.panes[&0].cwd.as_deref(), Some(Path::new("/here/x")));
        assert!(toml::from_str::<Template>("[[window]]\nfloat = true\n").is_err());
    }
}
