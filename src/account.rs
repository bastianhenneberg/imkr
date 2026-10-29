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
    /// The account new shells start in, by agent, when not `standard`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    default: std::collections::BTreeMap<String, String>,
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

    fn read(&self) -> File {
        std::fs::read_to_string(self.file())
            .ok()
            .and_then(|text| toml::from_str::<File>(&text).ok())
            .unwrap_or_default()
    }

    /// The registered accounts, in their order.
    pub fn load(&self) -> Vec<Account> {
        self.read().accounts
    }

    /// The account new shells of `tool` start in: the one set with
    /// `imkr account default`, else `standard`. One not logged in here
    /// (synced from another machine, see `merge_in`, or logged out) does not
    /// count: then the first that is logged in, so that a new agent does
    /// not start at a login screen.
    pub fn default_of(&self, tool: &str) -> String {
        let set = self.read().default.get(tool).cloned();
        if let Some(name) = &set
            && self.find(tool, name).is_some_and(|a| self.logged_in(&a))
        {
            return name.clone();
        }
        self.of_tool(tool)
            .into_iter()
            .find(|a| self.logged_in(a))
            .map(|a| a.name)
            .unwrap_or_else(|| STANDARD.to_owned())
    }

    /// The list of accounts as a file, for `imkr settings sync`: names,
    /// order, default and providers, never a login.
    pub fn list_file(&self) -> PathBuf {
        self.file()
    }

    /// Takes the list of another machine (`text`, its accounts.toml): every
    /// account it has that is missing here is added at its end, with its
    /// directory, so a subscription shows as not logged in until
    /// `imkr account login`. Nothing here is removed or changed; the default
    /// comes along when this machine has none. Returns the accounts added.
    pub fn merge_in(&self, text: &str) -> Result<Vec<String>> {
        let there: File =
            toml::from_str(text).context("the accounts from there are not readable")?;
        let mut file = self.read();
        let mut added = Vec::new();
        for account in there.accounts {
            if !TOOLS.contains(&account.tool.as_str()) || !valid_name(&account.name) {
                continue;
            }
            if file
                .accounts
                .iter()
                .any(|a| a.tool == account.tool && a.name == account.name)
            {
                continue;
            }
            // Named like the login `standard` has here (henneberg's machine
            // gets the account `henneberg`): that subscription is there.
            let standard = Account {
                name: STANDARD.into(),
                tool: account.tool.clone(),
                ..Account::default()
            };
            if account.url.is_none()
                && self
                    .identity(&standard)
                    .is_some_and(|_| shown(self, &standard) == account.name)
            {
                continue;
            }
            self.link_shared(&account)?;
            added.push(format!("{}:{}", account.tool, account.name));
            file.accounts.push(account);
        }
        for (tool, name) in there.default {
            file.default.entry(tool).or_insert(name);
        }
        self.write(&file)?;
        Ok(added)
    }

    /// Makes `account` the one new shells of its agent start in.
    pub fn set_default(&self, account: &Account) -> Result<()> {
        let mut file = self.read();
        if account.is_standard() {
            file.default.remove(&account.tool);
        } else {
            file.default
                .insert(account.tool.clone(), account.name.clone());
        }
        self.write(&file)
    }

    /// The account of `tool` called `given`, by its own name or by the
    /// subscription it is logged in to (`shown`): `stelljes` finds
    /// `standard` when that one is stelljes' login.
    pub fn named(&self, tool: &str, given: &str) -> Option<Account> {
        self.find(tool, given).or_else(|| {
            self.of_tool(tool)
                .into_iter()
                .find(|a| a.url.is_none() && shown(self, a) == given)
        })
    }

    fn save(&self, accounts: &[Account]) -> Result<()> {
        let mut file = self.read();
        file.accounts = accounts.to_vec();
        self.write(&file)
    }

    fn write(&self, file: &File) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.file();
        std::fs::create_dir_all(&self.root)?;
        let text = toml::to_string(file)?;
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
        if self.read().default.get(tool) == Some(&account.name) {
            let mut file = self.read();
            file.default.remove(tool);
            self.write(&file)?;
        }
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

    /// The accounts that could take over from `current` of `tool`: those
    /// logged in and not used up, the next after `current` first, round.
    /// `freest` picks among them; on its own this order decides only
    /// where no numbers do.
    fn free_after(&self, tool: &str, current: &str) -> Vec<Account> {
        let all = self.of_tool(tool);
        let used_up = self.used_up();
        let at = all.iter().position(|a| a.name == current).unwrap_or(0);
        (1..all.len())
            .map(|step| &all[(at + step) % all.len()])
            .filter(|a| self.logged_in(a) && self.used_up_for(a, &used_up).is_none())
            .cloned()
            .collect()
    }

    /// The account to go on in after `current` of `tool`: of those
    /// `free_after` offers, the one with most of its limit left by
    /// `numbers` (a `usage::report`) — its week first, its five hours
    /// second, see `usage::fullness` — together with the window that
    /// decided, so a message can name it.
    ///
    /// Accounts the numbers say nothing about keep `free_after`'s order
    /// and come after those they do — a number beats a guess, and without
    /// any numbers this is plainly the next free account. So is a tie:
    /// `sort_by` is stable, and the first of two equally free accounts is
    /// the next in the list.
    pub fn freest(
        &self,
        tool: &str,
        current: &str,
        numbers: &[crate::usage::AccountUsage],
    ) -> Option<(Account, Option<crate::usage::Window>)> {
        let free = self.free_after(tool, current);
        let mut rated: Vec<(&Account, (f64, f64), &crate::usage::Window)> = free
            .iter()
            .filter_map(|account| {
                let usage = crate::usage::numbers_for(numbers, tool, &account.name)?;
                Some((
                    account,
                    crate::usage::fullness(usage)?,
                    crate::usage::deciding(usage)?,
                ))
            })
            .collect();
        rated.sort_by(|(_, a, _), (_, b, _)| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        if let Some((account, _, window)) = rated.first() {
            return Some(((*account).clone(), Some((*window).clone())));
        }
        free.into_iter().next().map(|account| (account, None))
    }

    /// Who is logged in to a subscription account, as far as its login
    /// files tell: Claude Code's `oauthAccount` in `.claude.json`, the id
    /// token in Codex' `auth.json`. `None` for providers and accounts not
    /// logged in.
    pub fn identity(&self, account: &Account) -> Option<Identity> {
        if self.login_dead(account) {
            return None;
        }
        self.last_identity(account)
    }

    /// Who `account` is or was logged in as: `identity`, also when the
    /// login is spent and only `.claude.json` still names the person.
    pub fn last_identity(&self, account: &Account) -> Option<Identity> {
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

    /// Whether `account` is left over (#8577): logged out, and the person
    /// it was logged in as has another account that is logged in. Lists
    /// leave it out, so that one person shows once.
    pub fn left_over(&self, account: &Account) -> bool {
        if !self.login_dead(account) {
            return false;
        }
        let Some(was) = self.last_identity(account) else {
            return false;
        };
        self.of_tool(&account.tool).iter().any(|other| {
            other.name != account.name && self.identity(other).is_some_and(|i| i.key == was.key)
        })
    }

    /// The accounts of `tool` as lists show them: `of_tool` without those
    /// left over.
    pub fn listed(&self, tool: &str) -> Vec<Account> {
        self.of_tool(tool)
            .into_iter()
            .filter(|a| !self.left_over(a))
            .collect()
    }

    /// Another named account (not `standard`) of the person `account` is
    /// logged in as, logged in or not: where its login belongs.
    pub fn same_person(&self, account: &Account) -> Option<Account> {
        let me = self.identity(account)?;
        self.of_tool(&account.tool).into_iter().find(|other| {
            other.name != account.name
                && !other.is_standard()
                && self.last_identity(other).is_some_and(|i| i.key == me.key)
        })
    }

    /// A named account of `tool` that is logged out and was the person
    /// called `name` (the part of the address before the @): to log in
    /// again instead of adding a second one.
    pub fn logged_out_as(&self, tool: &str, name: &str) -> Option<Account> {
        self.of_tool(tool).into_iter().find(|a| {
            !a.is_standard()
                && self.login_dead(a)
                && self.last_identity(a).is_some_and(|i| {
                    i.shown
                        .split('@')
                        .next()
                        .is_some_and(|who| who.eq_ignore_ascii_case(name))
                })
        })
    }

    /// Moves the login of `from` into the account `into` of the same
    /// agent: the login file, and for Claude what of `.claude.json` belongs
    /// to the login. A login is never in two places: both would renew it
    /// and lock each other out.
    pub fn move_login(&self, from: &Account, into: &Account) -> Result<()> {
        let (Some(from_dir), Some(into_dir)) = (self.dir_of(from), self.dir_of(into)) else {
            bail!("only between named accounts");
        };
        let file = if from.tool == "codex" {
            "auth.json"
        } else {
            ".credentials.json"
        };
        let target = into_dir.join(file);
        // A link (to what is shared) is replaced, not written through.
        if target.symlink_metadata().is_ok() {
            std::fs::remove_file(&target)?;
        }
        std::fs::rename(from_dir.join(file), &target)
            .with_context(|| format!("could not move the login into {}", into.name))?;
        if from.tool != "codex" {
            let read = |path: &Path| -> serde_json::Map<String, serde_json::Value> {
                std::fs::read_to_string(path)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default()
            };
            let source = read(&from_dir.join(".claude.json"));
            let path = into_dir.join(".claude.json");
            let mut own = read(&path);
            own.retain(|key, _| !account_bound(key));
            for (key, value) in source {
                if account_bound(&key) {
                    own.insert(key, value);
                }
            }
            std::fs::write(&path, serde_json::to_string_pretty(&own)?)?;
        }
        Ok(())
    }

    /// The file holding the Claude login of `account`, `standard`'s too.
    pub fn claude_credentials(&self, account: &Account) -> PathBuf {
        self.dir_of(account)
            .unwrap_or_else(|| self.usual_dir("claude"))
            .join(".credentials.json")
    }

    /// Whether the Claude login of `account` is there but spent: Claude
    /// Code empties it when the server refuses its refresh token, and
    /// `.claude.json` still names who was logged in. Such a login needs
    /// `/login` again; it is no subscription to fall back to or to count
    /// twice. A login file it cannot read is not called dead.
    pub fn login_dead(&self, account: &Account) -> bool {
        if account.url.is_some() || account.tool != "claude" {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(self.claude_credentials(account)) else {
            return false;
        };
        let Ok(login) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        login_spent(&login, now)
    }

    /// Whether the account can be used: a subscription needs its login.
    pub fn logged_in(&self, account: &Account) -> bool {
        if account.url.is_some() {
            return true;
        }
        if self.login_dead(account) {
            return false;
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

/// What a pane's frame says about the account its agent runs in: the
/// provider and model for one by address (`Ollama · qwen3-coder`), else who is logged
/// in, the address before the @ (`henneberg`), else the account's name.
pub fn shown(store: &Store, account: &Account) -> String {
    if let Some(url) = &account.url {
        // A provider's model is the account's choice, not in a transcript.
        return match &account.model {
            Some(model) => format!("{} · {model}", provider_name(url)),
            None => provider_name(url),
        };
    }
    store
        .identity(account)
        .map(|i| i.shown.split('@').next().unwrap_or_default().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| account.name.clone())
}

/// A provider by its address: the known ones by name, others by host.
pub fn provider_name(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?'])
        .next()
        .unwrap_or_default();
    let known = [
        ("openrouter", "OpenRouter"),
        ("11434", "Ollama"),
        ("ollama", "Ollama"),
        ("anthropic", "Anthropic"),
        ("openai", "OpenAI"),
    ];
    known
        .iter()
        .find(|(part, _)| host.contains(part))
        .map(|(_, name)| (*name).to_owned())
        .unwrap_or_else(|| host.to_owned())
}

/// Who an account is logged in as: `shown` in lists, `key` to compare
/// (the address with the organisation or account it belongs to).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub shown: String,
    /// The login and its organisation: alike for one subscription.
    pub key: String,
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

/// Whether a Claude login (`.credentials.json`) can no longer renew
/// itself at `now` (seconds): no refresh token, or one past its end.
pub fn login_spent(login: &serde_json::Value, now: u64) -> bool {
    let oauth = &login["claudeAiOauth"];
    if !oauth.is_object() {
        return false;
    }
    let refresh = oauth["refreshToken"].as_str().unwrap_or_default();
    let ends = oauth["refreshTokenExpiresAt"].as_u64().unwrap_or(0) / 1000;
    refresh.is_empty() || (ends > 0 && ends <= now)
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
        assert_eq!(
            store.freest("claude", STANDARD, &[]).unwrap().0.name,
            "other"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The numbers decide, not the list order (#9102): Bastian's case was
    /// `stelljes` at 88 % of its week and `department` at 12 %, with a
    /// third account standing between them in the list.
    #[test]
    fn the_freest_account_is_picked_by_its_numbers_not_by_the_order() {
        let base = std::env::temp_dir().join(format!("imkr-freest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        claude_login(&store.home, "me@x", "org1");
        std::fs::create_dir_all(store.usual_dir("claude")).unwrap();
        for name in ["middle", "department", "twin"] {
            store
                .add(Account {
                    name: name.into(),
                    tool: "claude".into(),
                    ..Account::default()
                })
                .unwrap();
        }
        let dir = |name: &str| store.root.join("claude").join(name);
        claude_login(&dir("middle"), "middle@x", "org2");
        claude_login(&dir("department"), "dep@x", "org3");
        // `twin` shares the subscription of `standard`, the account left.
        claude_login(&dir("twin"), "me@x", "org1");
        let window = |label: &str, used: f64| crate::usage::Window {
            label: label.into(),
            used,
            resets_at: None,
        };
        let numbers =
            |names: &[&str], windows: Vec<crate::usage::Window>| crate::usage::AccountUsage {
                tool: "claude".into(),
                name: names[0].into(),
                names: names.iter().map(|n| (*n).to_owned()).collect(),
                default: false,
                plan: None,
                windows,
                note: None,
            };
        let all = vec![
            numbers(&[STANDARD, "twin"], vec![window("week", 88.0)]),
            numbers(&["middle"], vec![window("week", 55.0)]),
            // Two thirds of its five hours gone, its week nearly whole:
            // the week decides, so this is the account to go to. A five
            // hour window that runs out is over in five hours — and were
            // it out already, `used_up` would keep it out of the list.
            numbers(
                &["department"],
                vec![window("5h", 61.0), window("week", 12.0)],
            ),
        ];
        // Without numbers the next in the list wins; with them, the freest
        // — and the window that bound is named for the message.
        assert_eq!(
            store.freest("claude", STANDARD, &[]).unwrap().0.name,
            "middle"
        );
        let (picked, bound) = store.freest("claude", STANDARD, &all).unwrap();
        assert_eq!(picked.name, "department");
        let bound = bound.unwrap();
        assert_eq!((bound.label.as_str(), bound.used), ("week", 12.0));
        // `standard` used up takes `twin` with it — one subscription, one
        // limit. From `middle` only `department` is left to go to, and
        // from `department` only `middle`: nobody is sent to a used up
        // account, and nobody to the account they already run in.
        store
            .mark_used_up(
                &store.find("claude", STANDARD).unwrap(),
                crate::persist::now() + 3600,
            )
            .unwrap();
        let names: Vec<String> = ["middle", "department"]
            .iter()
            .map(|from| store.freest("claude", from, &all).unwrap().0.name)
            .collect();
        assert_eq!(names, ["department", "middle"]);
        // An account the numbers say nothing about comes after one they
        // do: a number beats a guess.
        let only_middle = vec![numbers(&["middle"], vec![window("week", 99.0)])];
        assert_eq!(
            store
                .freest("claude", STANDARD, &only_middle)
                .unwrap()
                .0
                .name,
            "middle"
        );
        // Equal weeks: the five hours break the tie, and only then.
        let tied = vec![
            numbers(&["middle"], vec![window("week", 40.0), window("5h", 70.0)]),
            numbers(
                &["department"],
                vec![window("week", 40.0), window("5h", 10.0)],
            ),
        ];
        assert_eq!(
            store.freest("claude", STANDARD, &tied).unwrap().0.name,
            "department"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `~/.claude` logged out by Claude Code (refresh token emptied) while
    /// `.claude.json` still names the address: no second subscription to
    /// `stelljes`, not a default to start in, and not shown by the address
    /// (2026-10-20: `account add` removed the fresh `stelljes` for it).
    #[test]
    fn a_spent_login_is_no_subscription() {
        let base = std::env::temp_dir().join(format!("imkr-spent-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        claude_login(&store.home, "me@x", "org1");
        std::fs::create_dir_all(store.usual_dir("claude")).unwrap();
        let spent = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,
            "refreshTokenExpiresAt":4102444800000}}"#;
        std::fs::write(store.usual_dir("claude").join(".credentials.json"), spent).unwrap();
        store
            .add(Account {
                name: "me".into(),
                tool: "claude".into(),
                ..Account::default()
            })
            .unwrap();
        claude_login(&store.root.join("claude/me"), "me@x", "org1");
        let find = |name: &str| store.find("claude", name).unwrap();

        assert!(store.login_dead(&find(STANDARD)));
        assert!(!store.logged_in(&find(STANDARD)));
        assert!(store.identity(&find(STANDARD)).is_none());
        assert_eq!(shown(&store, &find(STANDARD)), STANDARD);
        assert!(store.same_login(&find("me")).is_none());
        // No default set: the first logged in, not the logged out standard.
        assert_eq!(store.default_of("claude"), "me");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 2026-10-22: stelljes logged in anew as a second account while the
    /// spent `~/.claude` still named stelljes: one person shows once, and
    /// a person with a logged out account is logged in there again.
    #[test]
    fn one_person_is_one_account() {
        let base = std::env::temp_dir().join(format!("imkr-one-person-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        let spent = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#;
        claude_login(&store.home, "me@x", "org1");
        std::fs::create_dir_all(store.usual_dir("claude")).unwrap();
        std::fs::write(store.usual_dir("claude").join(".credentials.json"), spent).unwrap();
        for name in ["me", "old", "fresh"] {
            store
                .add(Account {
                    name: name.into(),
                    tool: "claude".into(),
                    ..Account::default()
                })
                .unwrap();
        }
        let dir = |name: &str| store.root.join(format!("claude/{name}"));
        claude_login(&dir("me"), "me@x", "org1");
        claude_login(&dir("old"), "you@x", "org1");
        std::fs::write(dir("old").join(".credentials.json"), spent).unwrap();
        let find = |name: &str| store.find("claude", name).unwrap();

        // The spent standard was me, who has an account: left out.
        assert!(store.left_over(&find(STANDARD)));
        let listed: Vec<String> = store.listed("claude").into_iter().map(|a| a.name).collect();
        assert_eq!(listed, ["me", "old", "fresh"]);
        // `old` is logged out and has no one else: listed, and `add you`
        // logs it in again.
        assert!(!store.left_over(&find("old")));
        assert_eq!(store.logged_out_as("claude", "You").unwrap().name, "old");
        assert!(store.logged_out_as("claude", "me").is_none());

        // `fresh` logged in as you: its login moves into `old`.
        claude_login(&dir("fresh"), "you@x", "org1");
        std::fs::write(dir("fresh").join(".credentials.json"), "new login").unwrap();
        assert_eq!(store.same_person(&find("fresh")).unwrap().name, "old");
        std::fs::write(
            dir("old").join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"you@x"},"mcpServers":{"brain":{}}}"#,
        )
        .unwrap();
        store.move_login(&find("fresh"), &find("old")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir("old").join(".credentials.json")).unwrap(),
            "new login"
        );
        assert!(!dir("fresh").join(".credentials.json").exists());
        let config: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir("old").join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(config["oauthAccount"]["emailAddress"], "you@x");
        assert!(config["mcpServers"]["brain"].is_object());
        assert!(store.logged_in(&find("old")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn spent_logins_are_told_from_old_keys() {
        let login = |refresh: &str, ends: u64| {
            serde_json::json!({"claudeAiOauth": {
                "accessToken": "a", "refreshToken": refresh, "expiresAt": 0,
                "refreshTokenExpiresAt": ends * 1000}})
        };
        // An old access key alone is no logout: Claude Code renews it.
        assert!(!login_spent(&login("r", 2000), 1000));
        assert!(login_spent(&login("", 2000), 1000));
        assert!(login_spent(&login("r", 1000), 2000));
        // Without an end date the refresh token counts as good.
        assert!(!login_spent(&login("r", 0), 1000));
        assert!(!login_spent(&serde_json::json!("secret"), 1000));
    }

    #[test]
    fn frames_name_providers_and_subscriptions() {
        assert_eq!(provider_name("http://localhost:11434"), "Ollama");
        assert_eq!(provider_name("https://openrouter.ai/api"), "OpenRouter");
        assert_eq!(
            provider_name("https://llm.example.com/v1"),
            "llm.example.com"
        );
        let store = Store {
            root: std::env::temp_dir().join("imkr-shown-none"),
            home: std::env::temp_dir().join("imkr-shown-none"),
        };
        let local = Account {
            name: "local".into(),
            tool: "claude".into(),
            url: Some("http://localhost:11434".into()),
            model: Some("qwen3-coder".into()),
            ..Account::default()
        };
        assert_eq!(shown(&store, &local), "Ollama · qwen3-coder");
        // Not logged in (no login files): the account's name.
        let privat = Account {
            name: "privat".into(),
            tool: "claude".into(),
            ..Account::default()
        };
        assert_eq!(shown(&store, &privat), "privat");
    }

    /// Bastian's case: `standard` is stelljes' login, henneberg is the one
    /// he means to start in.
    #[test]
    fn a_default_account_and_names_by_subscription() {
        let base = std::env::temp_dir().join(format!("imkr-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        claude_login(&store.home, "stelljes@x", "o1");
        std::fs::create_dir_all(store.usual_dir("claude")).unwrap();
        store
            .add(Account {
                name: "work".into(),
                tool: "claude".into(),
                ..Account::default()
            })
            .unwrap();
        claude_login(&store.root.join("claude/work"), "henneberg@x", "o2");

        assert_eq!(store.default_of("claude"), STANDARD);
        assert_eq!(store.named("claude", "stelljes").unwrap().name, STANDARD);
        assert_eq!(store.named("claude", "henneberg").unwrap().name, "work");
        store
            .set_default(&store.find("claude", "work").unwrap())
            .unwrap();
        assert_eq!(store.default_of("claude"), "work");
        assert_eq!(store.default_of("codex"), STANDARD);
        // Gone with its account.
        store.remove("claude", "work").unwrap();
        assert_eq!(store.default_of("claude"), STANDARD);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Another machine's list adds what is missing, logins stay out, and a
    /// default not logged in here does not count yet.
    #[test]
    fn lists_from_another_machine_are_joined() {
        let base = std::env::temp_dir().join(format!("imkr-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("state/accounts"),
            home: base.join("home"),
        };
        std::fs::create_dir_all(store.usual_dir("claude")).unwrap();
        store
            .add(Account {
                name: "mine".into(),
                tool: "claude".into(),
                ..Account::default()
            })
            .unwrap();
        let there = r#"
default = { claude = "work" }

[[account]]
name = "work"
tool = "claude"

[[account]]
name = "mine"
tool = "claude"

[[account]]
name = "local"
tool = "claude"
url = "http://localhost:11434"
model = "qwen3-coder"
"#;
        // `standard` here is logged in as `me@…`: an account `me` from there
        // is that same subscription and stays out.
        claude_login(&store.home, "me@x", "o0");
        let there = format!("{there}\n[[account]]\nname = \"me\"\ntool = \"claude\"\n");
        let added = store.merge_in(&there).unwrap();
        assert_eq!(added, vec!["claude:work", "claude:local"]);
        let names: Vec<String> = store.load().into_iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["mine", "work", "local"]);
        let work = store.find("claude", "work").unwrap();
        assert!(store.dir_of(&work).unwrap().is_dir());
        assert!(!store.logged_in(&work));
        // Not logged in here: new shells stay in standard for now.
        assert_eq!(store.default_of("claude"), STANDARD);
        claude_login(&store.root.join("claude/work"), "w@x", "o");
        assert_eq!(store.default_of("claude"), "work");
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
            store.freest("claude", "standard", &[]).unwrap().0.name,
            "privat"
        );
        store
            .mark_used_up(&account, crate::persist::now() + 3600)
            .unwrap();
        assert_eq!(
            store.freest("claude", "standard", &[]).unwrap().0.name,
            "router"
        );
        assert_eq!(
            store.freest("claude", "router", &[]).unwrap().0.name,
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
