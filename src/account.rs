//! Several accounts of a coding agent on one machine: Claude Code and Codex
//! subscriptions, each logged in on its own, and providers such as
//! OpenRouter or a local Ollama that Claude Code reaches by address and key.
//!
//! An account is a set of environment variables an agent starts with. A
//! subscription gets a directory of its own (`CLAUDE_CONFIG_DIR`,
//! `CODEX_HOME`) holding only its login; everything else in it links to
//! the agent's usual directory (`~/.claude`, `~/.codex`), so settings,
//! hooks, skills and above all the conversations are the same in every
//! account, and a conversation goes on under another one. The usual
//! directory is the account `standard` of each agent.
//!
//! Accounts live in `<state>/accounts/`: `accounts.toml` (0600, it holds
//! the providers' keys) and a directory per account.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The account every agent has without one of imkr's: its usual directory.
pub const STANDARD: &str = "standard";

/// The agents accounts are for.
pub const TOOLS: &[&str] = &["claude", "codex"];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub name: String,
    /// `claude` or `codex`.
    pub tool: String,
    /// A provider's address (`ANTHROPIC_BASE_URL`), for Claude Code; without
    /// one the account is a subscription, logged in in its directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The provider's key (`ANTHROPIC_AUTH_TOKEN`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The model to ask the provider for (`ANTHROPIC_MODEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Account {
    pub fn is_standard(&self) -> bool {
        self.name == STANDARD
    }

    /// What it is, for lists: subscription or provider.
    pub fn kind(&self) -> String {
        match (&self.url, &self.model) {
            (Some(url), Some(model)) => format!("{url} · {model}"),
            (Some(url), None) => url.clone(),
            _ if self.is_standard() => format!("~/.{} as it is", self.tool),
            _ => "subscription".into(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    /// In the order to fall back in when one is used up.
    #[serde(default, rename = "account")]
    accounts: Vec<Account>,
}

/// Where the accounts are kept, and the user's home with the agents'
/// usual directories.
pub struct Store {
    pub root: PathBuf,
    pub home: PathBuf,
}

impl Store {
    /// This machine's: `<state>/accounts` and `$HOME`.
    pub fn here() -> Result<Self> {
        Ok(Self {
            root: crate::persist::state_dir()?.join("accounts"),
            home: PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?),
        })
    }

    fn file(&self) -> PathBuf {
        self.root.join("accounts.toml")
    }

    /// The registered accounts, in their order.
    pub fn load(&self) -> Vec<Account> {
        std::fs::read_to_string(self.file())
            .ok()
            .and_then(|text| toml::from_str::<File>(&text).ok())
            .map(|file| file.accounts)
            .unwrap_or_default()
    }

    fn save(&self, accounts: &[Account]) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.file();
        std::fs::create_dir_all(&self.root)?;
        let text = toml::to_string(&File {
            accounts: accounts.to_vec(),
        })?;
        let tmp = path.with_extension("tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Every account of `tool`, `standard` first, then the registered ones
    /// in their order: the order to fall back in.
    pub fn of_tool(&self, tool: &str) -> Vec<Account> {
        let mut all = vec![Account {
            name: STANDARD.into(),
            tool: tool.into(),
            ..Account::default()
        }];
        all.extend(self.load().into_iter().filter(|a| a.tool == tool));
        all
    }

    /// The account called `name` of `tool`.
    pub fn find(&self, tool: &str, name: &str) -> Option<Account> {
        self.of_tool(tool).into_iter().find(|a| a.name == name)
    }

    /// The agent's usual directory, which accounts share everything but
    /// the login with.
    pub fn usual_dir(&self, tool: &str) -> PathBuf {
        match tool {
            "codex" => self.home.join(".codex"),
            _ => self.home.join(".claude"),
        }
    }

    /// The account's own directory, `None` for `standard`.
    pub fn dir_of(&self, account: &Account) -> Option<PathBuf> {
        (!account.is_standard()).then(|| self.root.join(&account.tool).join(&account.name))
    }

    /// The environment an agent starts with in `account`: names with
    /// values. `standard` sets nothing.
    pub fn env(&self, account: &Account) -> Vec<(&'static str, String)> {
        let Some(dir) = self.dir_of(account) else {
            return Vec::new();
        };
        let dir = dir.to_string_lossy().into_owned();
        if account.tool == "codex" {
            return vec![("CODEX_HOME", dir)];
        }
        let mut env = vec![("CLAUDE_CONFIG_DIR", dir)];
        if let Some(url) = &account.url {
            // As OpenRouter and Ollama say: the key as a bearer token, and
            // no Anthropic key that would win over it.
            env.push(("ANTHROPIC_BASE_URL", url.clone()));
            env.push((
                "ANTHROPIC_AUTH_TOKEN",
                account.token.clone().unwrap_or_default(),
            ));
            env.push(("ANTHROPIC_API_KEY", String::new()));
            if let Some(model) = &account.model {
                env.push(("ANTHROPIC_MODEL", model.clone()));
            }
        }
        env
    }

    /// Links everything of the usual directory into `account`'s but its
    /// login, entries added since too, and shares Claude's `.claude.json`.
    /// Returns the names linked now.
    pub fn link_shared(&self, account: &Account) -> Result<Vec<String>> {
        let Some(dir) = self.dir_of(account) else {
            return Ok(Vec::new());
        };
        let usual = self.usual_dir(&account.tool);
        // Not used here yet: made, so that what it keeps later is shared.
        std::fs::create_dir_all(&usual)?;
        std::fs::create_dir_all(&dir)?;
        let mut linked = Vec::new();
        for entry in std::fs::read_dir(&usual)
            .with_context(|| format!("cannot read {}", usual.display()))?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if own_file(&account.tool, &name) {
                continue;
            }
            let at = dir.join(&name);
            if at.symlink_metadata().is_ok() {
                continue;
            }
            std::os::unix::fs::symlink(entry.path(), &at)?;
            linked.push(name);
        }
        if account.tool == "claude" {
            self.share_claude_json(&dir)?;
        }
        Ok(linked)
    }

    /// Claude Code keeps its MCP servers, trusted projects and the like in
    /// `~/.claude.json`, next to the account's identity: a new account gets
    /// a copy without the identity and the caches that belong to it, later
    /// the MCP servers again.
    fn share_claude_json(&self, dir: &Path) -> Result<()> {
        let Ok(text) = std::fs::read_to_string(self.home.join(".claude.json")) else {
            return Ok(());
        };
        let usual: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let Some(usual) = usual.as_object() else {
            return Ok(());
        };
        let path = dir.join(".claude.json");
        let mut own: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if own.is_empty() {
            for (key, value) in usual {
                if !account_bound(key) {
                    own.insert(key.clone(), value.clone());
                }
            }
        } else if let Some(servers) = usual.get("mcpServers") {
            own.insert("mcpServers".into(), servers.clone());
        }
        std::fs::write(&path, serde_json::to_string_pretty(&own)?)?;
        Ok(())
    }

    /// Registers an account and sets up its directory. A subscription is
    /// logged in afterwards, see `login_command`.
    pub fn add(&self, account: Account) -> Result<Account> {
        if !TOOLS.contains(&account.tool.as_str()) {
            bail!("accounts are for {}", TOOLS.join(" and "));
        }
        if !valid_name(&account.name) {
            bail!("account names use letters, digits, - and _, and are not `standard`");
        }
        if account.url.is_some() && account.tool != "claude" {
            bail!("providers by address are for claude; codex sets them in its config.toml");
        }
        let mut accounts = self.load();
        if accounts
            .iter()
            .any(|a| a.tool == account.tool && a.name == account.name)
        {
            bail!(
                "there is a {} account {} already",
                account.tool,
                account.name
            );
        }
        self.link_shared(&account)?;
        accounts.push(account.clone());
        self.save(&accounts)?;
        Ok(account)
    }

    /// Forgets an account and removes its directory: its login, and links
    /// to what stays shared.
    pub fn remove(&self, tool: &str, name: &str) -> Result<()> {
        let mut accounts = self.load();
        let Some(at) = accounts
            .iter()
            .position(|a| a.tool == tool && a.name == name)
        else {
            bail!("there is no {tool} account {name}");
        };
        let account = accounts.remove(at);
        self.save(&accounts)?;
        if let Some(dir) = self.dir_of(&account) {
            // Links and the login only: remove_dir_all does not follow links.
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    fn used_up_file(&self) -> PathBuf {
        self.root.join("used-up.json")
    }

    /// Until when each account is used up, seconds since 1970, by
    /// `tool:name`; past ones left out.
    fn used_up(&self) -> std::collections::BTreeMap<String, u64> {
        let now = crate::persist::now();
        let mut all: std::collections::BTreeMap<String, u64> =
            std::fs::read_to_string(self.used_up_file())
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
        all.retain(|_, until| *until > now);
        all
    }

    /// Notes that `account` hit its limit, until `until` (seconds since
    /// 1970), so that falling back skips it till then.
    pub fn mark_used_up(&self, account: &Account, until: u64) -> Result<()> {
        let mut all = self.used_up();
        all.insert(format!("{}:{}", account.tool, account.name), until);
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.used_up_file(), serde_json::to_string(&all)?)?;
        Ok(())
    }

    /// When `account` can be used again, as local time, if it is used up.
    pub fn used_up_until(&self, account: &Account) -> Option<String> {
        crate::protocol::chrono_like(self.used_up_for(account, &self.used_up())?)
    }

    /// Until when `account` is used up: its own note, or that of another
    /// account logged in to the same subscription, as limits are the
    /// subscription's.
    fn used_up_for(
        &self,
        account: &Account,
        used_up: &std::collections::BTreeMap<String, u64>,
    ) -> Option<u64> {
        if let Some(until) = used_up.get(&format!("{}:{}", account.tool, account.name)) {
            return Some(*until);
        }
        let identity = self.identity(account)?;
        self.of_tool(&account.tool)
            .into_iter()
            .filter(|other| other.name != account.name)
            .filter(|other| self.identity(other).is_some_and(|i| i.key == identity.key))
            .find_map(|other| {
                used_up
                    .get(&format!("{}:{}", other.tool, other.name))
                    .copied()
            })
    }

    /// The account to fall back to after `current` of `tool`: the next in
    /// their order, round, that is logged in and not used up.
    pub fn next_free(&self, tool: &str, current: &str) -> Option<Account> {
        let all = self.of_tool(tool);
        let used_up = self.used_up();
        let at = all.iter().position(|a| a.name == current).unwrap_or(0);
        (1..all.len())
            .map(|step| &all[(at + step) % all.len()])
            .find(|a| self.logged_in(a) && self.used_up_for(a, &used_up).is_none())
            .cloned()
    }

    /// Who is logged in to a subscription account, as far as its login
    /// files tell: Claude Code's `oauthAccount` in `.claude.json`, the id
    /// token in Codex' `auth.json`. `None` for providers and accounts not
    /// logged in.
    pub fn identity(&self, account: &Account) -> Option<Identity> {
        if account.url.is_some() {
            return None;
        }
        match account.tool.as_str() {
            "codex" => {
                let file = match self.dir_of(account) {
                    Some(dir) => dir.join("auth.json"),
                    None => self.usual_dir("codex").join("auth.json"),
                };
                let auth: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
                let token = auth["tokens"]["id_token"].as_str()?;
                let claims: serde_json::Value =
                    serde_json::from_slice(&base64url(token.split('.').nth(1)?)?).ok()?;
                let email = claims["email"].as_str()?.to_owned();
                let id = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
                    .as_str()
                    .unwrap_or_default();
                Some(Identity {
                    key: format!("{email}|{id}"),
                    shown: email,
                })
            }
            _ => {
                let file = match self.dir_of(account) {
                    Some(dir) => dir.join(".claude.json"),
                    None => self.home.join(".claude.json"),
                };
                let config: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
                let login = &config["oauthAccount"];
                let email = login["emailAddress"].as_str()?.to_owned();
                let org = login["organizationUuid"].as_str().unwrap_or_default();
                Some(Identity {
                    key: format!("{email}|{org}"),
                    shown: email,
                })
            }
        }
    }

    /// Another account of the same agent logged in to the same
    /// subscription: a second one gains nothing, its limit is the same.
    pub fn same_login(&self, account: &Account) -> Option<Account> {
        let identity = self.identity(account)?;
        self.of_tool(&account.tool)
            .into_iter()
            .filter(|other| other.name != account.name)
            .find(|other| self.identity(other).is_some_and(|i| i.key == identity.key))
    }

    /// Whether the account can be used: a subscription needs its login.
    pub fn logged_in(&self, account: &Account) -> bool {
        if account.url.is_some() {
            return true;
        }
        let Some(dir) = self.dir_of(account) else {
            return true;
        };
        match account.tool.as_str() {
            "codex" => dir.join("auth.json").exists(),
            _ => dir.join(".credentials.json").exists(),
        }
    }
}

/// Who an account is logged in as: `shown` in lists, `key` to compare
/// (the address with the organisation or account it belongs to).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub shown: String,
    key: String,
}

/// Decodes unpadded base64url, as in the parts of a JSON web token.
fn base64url(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut bits = 0u32;
    let mut count = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

pub fn valid_name(name: &str) -> bool {
    crate::protocol::valid_session_name(name) && name != STANDARD
}

/// What stays an account's own in its directory: the login.
fn own_file(tool: &str, name: &str) -> bool {
    match tool {
        "codex" => name == "auth.json",
        _ => name == ".credentials.json" || name == ".claude.json",
    }
}

/// Keys of `.claude.json` that belong to the logged in account.
fn account_bound(key: &str) -> bool {
    matches!(
        key,
        "oauthAccount" | "userID" | "anonymousId" | "hasAvailableSubscription"
    ) || key.starts_with("cached")
        || key.ends_with("Cache")
}

/// What any account of `tool` may set: cleared before another one's are,
/// so that nothing of the one before is left.
pub fn variables(tool: &str) -> &'static [&'static str] {
    match tool {
        "codex" => &["CODEX_HOME"],
        _ => &[
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_MODEL",
        ],
    }
}

/// The command that picks `session` (the agent's conversation id from its
/// hooks) up again, else the newest conversation where it runs.
pub fn resume_command(tool: &str, session: Option<&str>) -> Vec<String> {
    let session = session.filter(|s| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    let words: &[&str] = match (tool, session) {
        ("codex", Some(id)) => &["codex", "resume", id],
        ("codex", None) => &["codex", "resume", "--last"],
        (_, Some(id)) => &["claude", "--resume", id],
        (_, None) => &["claude", "--continue"],
    };
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// Whether the end of an agent's screen (`tail`, its last lines) says its
/// account is used up, as Claude Code (“5-hour limit reached ∙ resets
/// 7pm”, “usage limit reached … reset at 3pm”) and Codex (“You've hit your
/// usage limit”) do. With when it frees up again, seconds since 1970,
/// where it says so; five hours from now where not.
pub fn used_up_in(tail: &str, now: u64) -> Option<u64> {
    let lower = tail.to_lowercase();
    // The agents' own words, not anything about limits.
    let hit = [
        "usage limit reached",
        "-hour limit reached",
        "weekly limit reached",
        "hit your usage limit",
        "hit your limit",
    ]
    .iter()
    .any(|p| lower.contains(p));
    if !hit {
        return None;
    }
    Some(reset_time(&lower, now).unwrap_or(now + 5 * 3600))
}

/// The time after “resets” or “reset at”, like `7pm`, `3:30 pm` or
/// `19:00`, as the next such moment, local time, seconds since 1970.
fn reset_time(text: &str, now: u64) -> Option<u64> {
    let at = text.find("reset")?;
    let rest = &text[at..];
    let start = rest.find(|c: char| c.is_ascii_digit())?;
    // Only a time right after the word, not one further on.
    if start > 14 {
        return None;
    }
    let rest = &rest[start..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let mut hour: u32 = digits.parse().ok()?;
    let mut minute = 0;
    let mut after = &rest[digits.len()..];
    if let Some(m) = after.strip_prefix(':') {
        let digits: String = m.chars().take_while(|c| c.is_ascii_digit()).collect();
        minute = digits.parse().ok()?;
        after = &m[digits.len()..];
    }
    let after = after.trim_start();
    if after.starts_with("pm") && hour < 12 {
        hour += 12;
    } else if after.starts_with("am") && hour == 12 {
        hour = 0;
    }
    if hour > 23 || minute > 59 {
        return None;
    }
    // The next time it is hour:minute here.
    let t = now as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let since_midnight = (tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec) as u64;
    let midnight = now - since_midnight;
    let target = midnight + u64::from(hour * 3600 + minute * 60);
    Some(if target > now {
        target
    } else {
        target + 24 * 3600
    })
}

/// The command that logs a subscription in, run in a terminal with the
/// account's environment.
pub fn login_command(account: &Account) -> Vec<String> {
    match account.tool.as_str() {
        "codex" => vec!["codex".into(), "login".into()],
        _ => vec!["claude".into(), "/login".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_login(dir: &Path, email: &str, org: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(".claude.json"),
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"{email}","organizationUuid":"{org}"}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join(".credentials.json"), "secret").unwrap();
    }

    /// Standard and a second account logged in to the same subscription:
    /// found as the same, and used up together, since the limit is the
    /// subscription's (Bastian had `standard` and `stelljes` like that).
    #[test]
    fn the_same_subscription_twice_is_found_and_used_up_together() {
        let base = std::env::temp_dir().join(format!("imkr-same-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        claude_login(&store.home, "me@x", "org1");
        std::fs::create_dir_all(store.usual_dir("claude")).unwrap();
        for name in ["twin", "other"] {
            store
                .add(Account {
                    name: name.into(),
                    tool: "claude".into(),
                    ..Account::default()
                })
                .unwrap();
        }
        let dir = |name: &str| store.root.join("claude").join(name);
        claude_login(&dir("twin"), "me@x", "org1");
        claude_login(&dir("other"), "me@x", "org2");
        let find = |name: &str| store.find("claude", name).unwrap();

        assert_eq!(store.identity(&find("twin")).unwrap().shown, "me@x");
        assert_eq!(store.same_login(&find("twin")).unwrap().name, STANDARD);
        assert_eq!(store.same_login(&find(STANDARD)).unwrap().name, "twin");
        // Same address, other organisation: another subscription.
        assert!(store.same_login(&find("other")).is_none());

        store
            .mark_used_up(&find(STANDARD), crate::persist::now() + 3600)
            .unwrap();
        assert!(store.used_up_until(&find("twin")).is_some());
        assert_eq!(store.next_free("claude", STANDARD).unwrap().name, "other");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn codex_logins_are_read_from_the_id_token() {
        let base = std::env::temp_dir().join(format!("imkr-codex-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        let usual = store.usual_dir("codex");
        std::fs::create_dir_all(&usual).unwrap();
        // {"email":"me@x","https://api.openai.com/auth":{"chatgpt_account_id":"a1"}}
        let claims = "eyJlbWFpbCI6Im1lQHgiLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYTEifX0";
        std::fs::write(
            usual.join("auth.json"),
            format!(r#"{{"tokens":{{"id_token":"x.{claims}.y"}}}}"#),
        )
        .unwrap();
        let standard = store.find("codex", STANDARD).unwrap();
        assert_eq!(store.identity(&standard).unwrap().shown, "me@x");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn accounts_share_all_but_their_login() {
        let base = std::env::temp_dir().join(format!("imkr-accounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        let usual = store.usual_dir("claude");
        std::fs::create_dir_all(usual.join("projects")).unwrap();
        std::fs::write(usual.join("settings.json"), "{}").unwrap();
        std::fs::write(usual.join(".credentials.json"), "secret").unwrap();
        std::fs::write(
            base.join("home/.claude.json"),
            r#"{"oauthAccount":{"emailAddress":"a@b"},"userID":"x","mcpServers":{"brain":{}},"projects":{"/p":{}},"cachedStatsigGates":{}}"#,
        )
        .unwrap();
        let account = store
            .add(Account {
                name: "privat".into(),
                tool: "claude".into(),
                ..Account::default()
            })
            .unwrap();
        let dir = store.dir_of(&account).unwrap();
        assert!(dir.join("projects").is_symlink());
        assert!(dir.join("settings.json").is_symlink());
        assert!(!dir.join(".credentials.json").exists());
        let own: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(".claude.json")).unwrap())
                .unwrap();
        assert!(own.get("oauthAccount").is_none() && own.get("userID").is_none());
        assert!(own.get("cachedStatsigGates").is_none());
        assert!(own["mcpServers"].get("brain").is_some());
        assert!(own["projects"].get("/p").is_some());
        assert!(!store.logged_in(&account));
        assert_eq!(
            store.env(&account),
            [("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned())]
        );
        // A provider: address and key, no Anthropic key besides.
        let router = store
            .add(Account {
                name: "router".into(),
                tool: "claude".into(),
                url: Some("https://openrouter.ai/api".into()),
                token: Some("sk-or-1".into()),
                model: Some("qwen/qwen3-coder".into()),
            })
            .unwrap();
        let env = store.env(&router);
        assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN", "sk-or-1".into())));
        assert!(env.contains(&("ANTHROPIC_API_KEY", String::new())));
        assert!(env.contains(&("ANTHROPIC_BASE_URL", "https://openrouter.ai/api".into())));
        // In order, standard first; the file keeps the key to itself.
        let names: Vec<String> = store
            .of_tool("claude")
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["standard", "privat", "router"]);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            store
                .add(Account {
                    name: "privat".into(),
                    tool: "claude".into(),
                    ..Account::default()
                })
                .is_err()
        );
        // Falling back skips what is used up and what has no login.
        std::fs::write(dir.join(".credentials.json"), "x").unwrap();
        assert_eq!(
            store.next_free("claude", "standard").unwrap().name,
            "privat"
        );
        store
            .mark_used_up(&account, crate::persist::now() + 3600)
            .unwrap();
        assert_eq!(
            store.next_free("claude", "standard").unwrap().name,
            "router"
        );
        assert_eq!(
            store.next_free("claude", "router").unwrap().name,
            "standard"
        );
        assert!(store.used_up_until(&account).is_some());
        store.remove("claude", "privat").unwrap();
        // What the agents say when an account is used up.
        let now = 1_791_900_000;
        assert!(used_up_in("> fix the tests\n  ⎿ All good.\n", now).is_none());
        let until = used_up_in(
            "5-hour limit reached ∙ resets 7pm\n/upgrade to increase",
            now,
        )
        .unwrap();
        assert!(until > now && until <= now + 24 * 3600);
        assert!(
            used_up_in(
                "Claude usage limit reached. Your limit will reset at 3:30 pm.",
                now
            )
            .is_some()
        );
        assert_eq!(
            used_up_in("■ You've hit your usage limit. Try again later.", now),
            Some(now + 5 * 3600)
        );
        assert!(!dir.exists());
        assert!(usual.join("projects").is_dir(), "the shared stays");
        assert!(usual.join(".credentials.json").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
