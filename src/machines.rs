//! The register of the user's machines (#9648), and how ssh reaches a
//! machine without an entry in `~/.ssh/config`.
//!
//! Machines do not search for each other: every machine keeps a register
//! of the machines it knows of (name, id, address, login, when that was
//! last true), and the register travels on the `imkr ls --json` exchange
//! the servers run anyway, both ways (`HOSTS_ENV`). The registers are
//! joined, the newer entry of one machine id wins (a new name, a new
//! address). `imkr hosts rm` leaves a removal with its time, which travels
//! as well, so a machine taken out does not come back over the others;
//! `imkr hosts add` takes it back. The start is one `imkr hosts add` by
//! hand. A machine from the register becomes a host here once ssh got in
//! and imkr answered: nothing is shown as unreachable for good, and
//! nothing is guessed — no peer of a VPN is tried that no machine of the
//! user's named.
//!
//! Files in the state directory, next to `hosts` (which stays one name a
//! line, as older versions read it):
//! - `host-routes.json`: name → address and user, for ssh;
//! - `machines/register.json`: the register;
//! - `machines/ids.json`: the id behind each host name here;
//! - `machines/tried.json`: when a machine of the register was last tried;
//! - `machines/known_hosts`: host keys of machines reached by address.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How imkr reaches a machine that ssh does not know by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// An IP address or a name that resolves.
    pub address: String,
    /// The login there; ssh's own default without.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
}

/// Set on `imkr ls --json` by a machine that wants this one's name and
/// id and its register along with the sessions; the value is the asking
/// machine's register, as JSON, so both learn from one exchange. Older
/// versions ignore it.
pub const HOSTS_ENV: &str = "IMKR_HOSTS_TOO";
/// Set along with `HOSTS_ENV`: the asking machine's id, so the one asked
/// notes the address that machine came from.
pub const CALLER_ENV: &str = "IMKR_CALLER";

/// A machine of a NetBird network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The first label of its NetBird name.
    pub name: String,
    pub fqdn: String,
    pub address: String,
    /// Connected or idle (lazy connections are idle until used); not when
    /// NetBird is still trying to reach it.
    pub online: bool,
}

/// The other machines of the NetBird network this one is in; empty
/// without NetBird.
pub fn netbird_peers() -> Vec<Peer> {
    let Ok(output) = Command::new("netbird")
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    parse_netbird(&status)
}

fn parse_netbird(status: &serde_json::Value) -> Vec<Peer> {
    let mut peers: Vec<Peer> = status["peers"]["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|peer| {
            let fqdn = peer["fqdn"].as_str()?.trim_end_matches('.').to_lowercase();
            let name = fqdn.split('.').next().filter(|n| !n.is_empty())?.to_owned();
            let address = peer["netbirdIp"].as_str()?.split('/').next()?.to_owned();
            address.parse::<IpAddr>().ok()?;
            let online = matches!(peer["status"].as_str(), Some("Connected" | "Idle"));
            Some(Peer {
                name,
                fqdn,
                address,
                online,
            })
        })
        .collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers
}

/// Whether `address` is in 100.64.0.0/10, where Tailscale and NetBird
/// hand out theirs: a machine of the user's own VPN.
pub fn in_vpn(address: &str) -> bool {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        _ => false,
    }
}

fn dir() -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("machines"))
}

fn routes_file() -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("host-routes.json"))
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: Result<PathBuf>) -> T {
    path.and_then(|p| Ok(std::fs::read(p)?))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes `value` to `path` whole, through a file beside it, so a reader
/// never sees half of it.
fn write_json<T: Serialize>(path: Result<PathBuf>, value: &T) -> Result<()> {
    let path = path?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = path.with_extension("part");
    std::fs::write(&part, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(part, path)?;
    Ok(())
}

/// The addresses noted for machines, by name.
pub fn routes() -> BTreeMap<String, Route> {
    read_json(routes_file())
}

/// Notes how `name` is reached, if that is new.
pub fn remember_route(name: &str, route: Route) {
    let mut routes = routes();
    if routes.get(name) == Some(&route) || route.address.is_empty() {
        return;
    }
    routes.insert(name.to_owned(), route);
    let _ = write_json(routes_file(), &routes);
}

pub fn forget_route(name: &str) {
    let mut routes = routes();
    if routes.remove(name).is_some() {
        let _ = write_json(routes_file(), &routes);
    }
}

/// What ssh is given for `host` in place of the bare name, ending with
/// `--` and the destination: the name when ssh knows it (an entry in
/// `~/.ssh/config`, or a name that resolves), else the address noted for
/// it. A host key seen first is taken only for an address of the own VPN
/// (`StrictHostKeyChecking=accept-new`), and kept in imkr's own
/// `known_hosts`; any other unknown key still stops ssh.
pub fn ssh_to(host: &str) -> Vec<String> {
    let route = routes().remove(host).filter(|_| !ssh_knows(host));
    match route {
        Some(route) => by_address(&route),
        None => by_name(host),
    }
}

/// ssh's arguments for a name ssh knows: the machine's own key (once it
/// has one) offered along with what the config names, so it still gets in
/// once the lines of a key copied to several machines are gone (#605).
/// `~/.ssh/imkr` is left to the config here, as before.
fn by_name(host: &str) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(key) = crate::pairing::key_file().filter(|key| key.is_file()) {
        args.extend(["-i".to_owned(), key.display().to_string()]);
    }
    args.extend(["--".to_owned(), host.to_owned()]);
    args
}

/// `-i KEY` for each machine key there is (`pairing::offered_keys`).
fn offered_keys() -> Vec<String> {
    crate::pairing::offered_keys()
        .into_iter()
        .flat_map(|key| ["-i".to_owned(), key.display().to_string()])
        .collect()
}

/// ssh's arguments for reaching `route`.
fn by_address(route: &Route) -> Vec<String> {
    let mut args = Vec::new();
    if in_vpn(&route.address) {
        args.extend([
            "-o".to_owned(),
            "StrictHostKeyChecking=accept-new".to_owned(),
        ]);
        if let Ok(dir) = dir() {
            let _ = std::fs::create_dir_all(&dir);
            // New keys go to the first file; the user's own are read too.
            args.extend([
                "-o".to_owned(),
                format!(
                    "UserKnownHostsFile={} ~/.ssh/known_hosts",
                    dir.join("known_hosts").display()
                ),
            ]);
        }
    }
    // No ~/.ssh/config entry names a key for a bare address: offer the
    // machine keys too, next to ssh's own defaults.
    args.extend(offered_keys());
    if !route.user.is_empty() {
        args.extend(["-l".to_owned(), route.user.clone()]);
    }
    args.extend(["--".to_owned(), route.address.clone()]);
    args
}

/// How long what ssh says about a name holds: the server asks for every
/// host every few seconds, and the config rarely changes.
const SSH_KNOWS_FOR: Duration = Duration::from_secs(300);

static SSH_KNOWS: Mutex<BTreeMap<String, (Instant, bool)>> = Mutex::new(BTreeMap::new());

/// Whether ssh reaches `host` by its name: `~/.ssh/config` gives it
/// another host name. A name that only resolves (NetBird's DNS does that
/// for every peer) is not enough: ssh would log in as the user here, with
/// no key for that machine, where the register knows user and address.
fn ssh_knows(host: &str) -> bool {
    if let Some((at, knows)) = SSH_KNOWS.lock().unwrap().get(host)
        && at.elapsed() < SSH_KNOWS_FOR
    {
        return *knows;
    }
    let (hostname, _) = ssh_config(host);
    let knows = hostname
        .as_deref()
        .is_some_and(|h| !h.eq_ignore_ascii_case(host));
    SSH_KNOWS
        .lock()
        .unwrap()
        .insert(host.to_owned(), (Instant::now(), knows));
    knows
}

/// The host name and user `ssh -G` gives for `host`.
fn ssh_config(host: &str) -> (Option<String>, Option<String>) {
    let Ok(output) = Command::new("ssh")
        .args(["-G", "--", host])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let (k, v) = line.split_once(' ')?;
            (k == key).then(|| v.trim().to_owned())
        })
    };
    (value("hostname"), value("user"))
}

/// The address and user this machine uses for `host`: what was noted, or
/// what ssh's config says, an address of the own VPN preferred over a
/// name another machine may not resolve.
fn route_of(host: &str) -> Route {
    if let Some(route) = routes().remove(host) {
        return route;
    }
    let (hostname, user) = ssh_config(host);
    let hostname = hostname.unwrap_or_else(|| host.to_owned());
    let resolved = (hostname.as_str(), 22)
        .to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .map(|a| a.ip().to_string())
        .find(|ip| in_vpn(ip));
    Route {
        address: resolved.unwrap_or(hostname),
        user: user.unwrap_or_default(),
    }
}

/// One machine of the register.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The name the machine gives itself (`machine_name`), or the one it
    /// was added by.
    pub name: String,
    /// Its `machine_id`; empty while only added by hand and not reached.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// Where ssh reaches it; empty while no machine reached it yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// When this entry was last true, in seconds since 1970: the newer of
    /// two entries of one machine wins.
    pub at: u64,
    /// Taken out with `imkr hosts rm`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    /// Its machine key (`~/.ssh/imkr.pub`, `TYPE BASE64`), which every
    /// machine with this entry lets in (#605). Only taken from a machine
    /// that may hand on keys (see `pairing`), never dropped by a newer
    /// entry without one: older versions do not know the field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
}

impl Entry {
    /// Which machine the entry is about: its id, or its name before the id
    /// is known.
    fn key(&self) -> String {
        if self.id.is_empty() {
            format!("name:{}", self.name)
        } else {
            self.id.clone()
        }
    }

    /// Whether this entry is to replace `other`, of the same machine.
    fn beats(&self, other: &Entry) -> bool {
        self.at > other.at
            || (self.at == other.at && other.address.is_empty() && !self.address.is_empty())
    }
}

/// Joins `theirs` into `mine`: every machine of both, the newer entry of
/// each. An entry known only by name gives way to one with an id under
/// that name, after handing it what it says if it is the newer (a removal
/// by hand before the id was known still removes). True if `mine`
/// changed.
pub fn merge(mine: &mut Vec<Entry>, theirs: &[Entry]) -> bool {
    let before = mine.clone();
    for entry in theirs.iter().filter(|e| !e.name.is_empty()) {
        match mine.iter_mut().find(|m| m.key() == entry.key()) {
            Some(m) if entry.beats(m) => {
                let key = std::mem::take(&mut m.key);
                *m = entry.clone();
                if m.key.is_empty() {
                    m.key = key;
                }
            }
            // Not newer, but a key where there was none: a gap, like an
            // address (the machine's own entry has it from the start).
            Some(m) if m.key.is_empty() => m.key.clone_from(&entry.key),
            Some(_) => {}
            None => mine.push(entry.clone()),
        }
    }
    let by_name: Vec<Entry> = mine.iter().filter(|e| e.id.is_empty()).cloned().collect();
    for named in by_name {
        let Some(with_id) = mine
            .iter_mut()
            .find(|e| !e.id.is_empty() && e.name == named.name)
        else {
            continue;
        };
        if with_id.key.is_empty() {
            with_id.key = named.key.clone();
        }
        if named.at > with_id.at {
            with_id.removed = named.removed;
            with_id.at = named.at;
            if !named.address.is_empty() {
                with_id.address = named.address.clone();
            }
            if !named.user.is_empty() {
                with_id.user = named.user.clone();
            }
        }
        mine.retain(|e| e.key() != named.key());
    }
    mine.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    *mine != before
}

fn register_file() -> Result<PathBuf> {
    Ok(dir()?.join("register.json"))
}

/// The machines of the register, removed ones included.
pub fn register() -> Vec<Entry> {
    read_json(register_file())
}

/// Changes the register under a lock: servers and `imkr ls --json` started
/// by other machines write it at the same time.
fn change_register(change: impl FnOnce(&mut Vec<Entry>)) -> Vec<Entry> {
    change_register_noting(change).0
}

/// `change_register`, with what that did to `authorized_keys`.
fn change_register_noting(
    change: impl FnOnce(&mut Vec<Entry>),
) -> (Vec<Entry>, crate::pairing::Changes) {
    use std::os::fd::AsRawFd;
    let lock = dir().ok().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::File::create(dir.join("register.lock")).ok()
    });
    if let Some(file) = &lock {
        // SAFETY: flock on a descriptor we own; released when it closes.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    }
    let mut entries = register();
    let before = entries.clone();
    change(&mut entries);
    // Only a paired machine keeps keys of others and lets them in; one
    // reached by a login set up by hand (a server) never does (#605).
    let paired = crate::pairing::paired();
    if !paired {
        for entry in &mut entries {
            entry.key.clear();
        }
    }
    let mut changes = crate::pairing::Changes::default();
    if entries != before {
        let _ = write_json(register_file(), &entries);
        // Under the lock still: two writers of authorized_keys at once
        // would lose a line.
        if paired {
            changes = crate::pairing::sync_keys(&entries);
        }
    }
    (entries, changes)
}

/// This machine's own entry, made new when its name or login changed.
/// Its removal stays as long as nothing changed: a machine taken out
/// does not put itself back.
fn note_self(entries: &mut Vec<Entry>) {
    let id = crate::protocol::machine_id();
    let name = crate::protocol::machine_name();
    let user = std::env::var("USER").unwrap_or_default();
    let key = crate::pairing::own_key().unwrap_or_default();
    let now = crate::persist::now();
    match entries.iter_mut().find(|e| e.id == id) {
        Some(own) if own.name == name && own.user == user && own.key == key => {}
        Some(own) => {
            own.name = name;
            own.user = user;
            own.key = key;
            own.at = now;
        }
        None => {
            merge(
                entries,
                &[Entry {
                    name,
                    id,
                    user,
                    key,
                    at: now,
                    ..Entry::default()
                }],
            );
        }
    }
}

/// Notes that the machine `id` was reached at `address`: an address of the
/// own VPN is the same from every machine and replaces an old one (a new
/// IP), any other only fills a gap.
fn note_address(entries: &mut [Entry], id: &str, address: &str) {
    let Some(entry) = entries.iter_mut().find(|e| e.id == id && !id.is_empty()) else {
        return;
    };
    if address.is_empty() || entry.address == address {
        return;
    }
    if entry.address.is_empty() || in_vpn(address) {
        entry.address = address.to_owned();
        entry.at = crate::persist::now();
    }
}

/// The register to send along with `imkr ls --json`, with this machine's
/// own entry made current.
pub fn register_to_send() -> String {
    serde_json::to_string(&change_register(note_self)).unwrap_or_default()
}

/// What `imkr hosts add NAME` does to the register: the machine is in
/// again, newer than its removal, at `route` if one is given.
pub fn register_add(name: &str, route: &Route) {
    let id = host_ids()
        .get(name)
        .map(|h| h.id.clone())
        .unwrap_or_default();
    change_register(|entries| {
        let now = crate::persist::now();
        let mut found = false;
        for entry in entries
            .iter_mut()
            .filter(|e| e.name == name || (!id.is_empty() && e.id == id))
        {
            found = true;
            entry.removed = false;
            entry.at = now;
            if !route.address.is_empty() {
                entry.address = route.address.clone();
                entry.user = route.user.clone();
            }
        }
        if !found {
            merge(
                entries,
                &[Entry {
                    name: name.to_owned(),
                    id,
                    address: route.address.clone(),
                    user: route.user.clone(),
                    at: now,
                    removed: false,
                    key: String::new(),
                }],
            );
        }
    });
}

/// What `imkr hosts rm NAME` does to the register: a removal with the
/// time, which travels to the other machines. False if the register did
/// not know `name`.
pub fn register_remove(name: &str) -> (bool, crate::pairing::Changes) {
    let id = host_ids()
        .get(name)
        .map(|h| h.id.clone())
        .unwrap_or_default();
    let mut found = false;
    let (_, changes) = change_register_noting(|entries| {
        let now = crate::persist::now();
        for entry in entries
            .iter_mut()
            .filter(|e| e.name == name || (!id.is_empty() && e.id == id))
        {
            found = true;
            entry.removed = true;
            entry.at = now;
        }
        if !found {
            entries.push(Entry {
                name: name.to_owned(),
                id: id.clone(),
                at: now,
                removed: true,
                ..Entry::default()
            });
        }
    });
    (found, changes)
}

/// What a host name here stands for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostId {
    pub id: String,
    /// Taken from the register (not added by hand or with `--host`): it
    /// follows the machine's new name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_register: bool,
}

fn ids_file() -> Result<PathBuf> {
    Ok(dir()?.join("ids.json"))
}

/// The machine id behind each host name here, from the last answer.
pub fn host_ids() -> BTreeMap<String, HostId> {
    read_json(ids_file())
}

pub fn note_id(host: &str, id: &str, from_register: bool) {
    let mut ids = host_ids();
    let from_register = from_register || ids.get(host).is_some_and(|h| h.from_register);
    let entry = HostId {
        id: id.to_owned(),
        from_register,
    };
    if ids.get(host) != Some(&entry) {
        ids.insert(host.to_owned(), entry);
        let _ = write_json(ids_file(), &ids);
    }
}

fn forget_id(host: &str) {
    let mut ids = host_ids();
    if ids.remove(host).is_some() {
        let _ = write_json(ids_file(), &ids);
    }
}

/// For `imkr ls --json` asked with `HOSTS_ENV` by another machine: joins
/// its register into this one's, notes the address it came from, and
/// hands back this machine's name and id and the joined register on the
/// first entry — on a stand-in when no session runs, as the asking side
/// makes one up for an empty list anyway.
pub fn tell_hosts(sessions: &mut Vec<crate::protocol::SessionInfo>) {
    let Some(theirs) = std::env::var_os(HOSTS_ENV) else {
        return;
    };
    let mut theirs: Vec<Entry> =
        serde_json::from_str(&theirs.to_string_lossy()).unwrap_or_default();
    // Keys stay: the caller logged in here. By a key of imkr's, `imkr
    // gate` says which machine it is, and that one does not take back
    // its own removal; by a key set by hand it has a shell anyway.
    let gated = std::env::var(crate::pairing::GATED_ENV).ok();
    let caller = gated
        .clone()
        .or_else(|| std::env::var(CALLER_ENV).ok())
        .unwrap_or_default();
    if let Some(gated) = &gated {
        let removed: Vec<String> = register()
            .into_iter()
            .filter(|e| e.removed && e.id == *gated)
            .map(|e| e.id)
            .collect();
        theirs.retain(|e| !removed.contains(&e.id));
    }
    let came_from = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|c| c.split_whitespace().next().map(str::to_owned))
        .filter(|a| a.parse::<IpAddr>().is_ok())
        .unwrap_or_default();
    let entries = change_register(|entries| {
        note_self(entries);
        merge(entries, &theirs);
        note_address(entries, &caller, &came_from);
    });
    if sessions.is_empty() {
        sessions.push(crate::protocol::SessionInfo {
            name: crate::protocol::DEFAULT_SESSION.to_owned(),
            ..Default::default()
        });
    }
    let first = &mut sessions[0];
    first.machine = crate::protocol::machine_name();
    first.machine_id = crate::protocol::machine_id();
    first.machines = entries;
}

/// From the sessions `host` listed with `HOSTS_ENV`: its id, its register
/// joined into this one's, and the address it was reached at.
pub fn learn_from(host: &str, sessions: &[crate::protocol::SessionInfo]) {
    let Some(first) = sessions.first().filter(|s| !s.machine_id.is_empty()) else {
        return;
    };
    note_id(host, &first.machine_id, false);
    let address = route_of(host).address;
    // Without keys: that it answered does not prove it may get in here.
    let theirs: Vec<Entry> = first
        .machines
        .iter()
        .map(|e| Entry {
            key: String::new(),
            ..e.clone()
        })
        .collect();
    change_register(|entries| {
        merge(entries, &theirs);
        note_address(entries, &first.machine_id, &address);
    });
}

fn tried_file() -> Result<PathBuf> {
    Ok(dir()?.join("tried.json"))
}

/// How long a machine of the register that ssh did not reach is left
/// before it is tried again: it may be off, or not let this one in.
const TRY_AGAIN: u64 = 10 * 60;
/// After a pairing, how long the machines of the register are tried every
/// `TRY_AGAIN_PAIRED` instead: the other side's key reaches them only with
/// the next exchanges, so the first try often comes too early (#605).
const PAIRED_FRESH: u64 = 15 * 60;
const TRY_AGAIN_PAIRED: u64 = 60;

fn paired_file() -> Result<PathBuf> {
    Ok(dir()?.join("paired-at"))
}

/// How long to wait before trying a machine of the register again.
fn try_again() -> u64 {
    let paired: u64 = paired_file()
        .and_then(|p| Ok(std::fs::read_to_string(p)?))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0);
    if crate::persist::now().saturating_sub(paired) < PAIRED_FRESH {
        TRY_AGAIN_PAIRED
    } else {
        TRY_AGAIN
    }
}

/// One round of acting on the register, from the server's places thread
/// every minute: removed machines leave the hosts, renamed ones follow
/// their new name, and the machines not shown yet are tried (ssh in batch
/// mode, `imkr ls --json`) and become hosts once imkr answers there.
/// Returns the NetBird peers to suggest.
pub fn settle() -> Vec<Suggestion> {
    let entries = register();
    let own_id = crate::protocol::machine_id();
    let here = crate::protocol::machine_name();
    let ids = host_ids();
    let remembered = crate::client::remembered_hosts();
    let mut tried: BTreeMap<String, u64> = read_json(tried_file());
    let now = crate::persist::now();
    let mut to_try = Vec::new();
    let wait = try_again();
    for entry in entries
        .iter()
        .filter(|e| e.id != own_id && e.name != here && !e.name.is_empty())
    {
        // The names this machine is known by here: its own, and the ones
        // the same id answered under.
        let same =
            |n: &String| !entry.id.is_empty() && ids.get(n).is_some_and(|h| h.id == entry.id);
        if entry.removed {
            for name in remembered.iter().filter(|n| **n == entry.name || same(n)) {
                drop_host(name);
            }
            continue;
        }
        // Renamed: the same machine under its new name, no need to try.
        // Only names taken from the register; one added by hand stays.
        for old in remembered.iter().filter(|n| {
            **n != entry.name && same(n) && ids.get(*n).is_some_and(|h| h.from_register)
        }) {
            drop_host(old);
            take(entry, true);
        }
        let known = crate::client::known_hosts();
        if known.contains(&entry.name) {
            // The register's address, where ssh does not know the name.
            if !entry.address.is_empty() && !ssh_knows(&entry.name) {
                remember_route(
                    &entry.name,
                    Route {
                        address: entry.address.clone(),
                        user: entry.user.clone(),
                    },
                );
            }
            continue;
        }
        // Shown already under another name (added by hand).
        if known.iter().any(same) {
            continue;
        }
        let reachable = !entry.address.is_empty() || ssh_knows(&entry.name);
        let due = tried
            .get(&entry.key())
            .is_none_or(|at| now.saturating_sub(*at) >= wait);
        if reachable && due {
            tried.insert(entry.key(), now);
            to_try.push(entry.clone());
        }
    }
    if !to_try.is_empty() {
        let _ = write_json(tried_file(), &tried);
    }
    let threads: Vec<_> = to_try
        .into_iter()
        .map(|entry| std::thread::spawn(move || (reach(&entry), entry)))
        .collect();
    for thread in threads {
        if let Ok((Some(sessions), entry)) = thread.join() {
            take(&entry, true);
            tried.remove(&entry.key());
            learn_from(&entry.name, &sessions);
        }
    }
    let _ = write_json(tried_file(), &tried);
    suggestions()
}

/// Makes `entry` a host here, with its address where ssh does not know
/// the name.
fn take(entry: &Entry, from_register: bool) {
    if !ssh_knows(&entry.name) && !entry.address.is_empty() {
        remember_route(
            &entry.name,
            Route {
                address: entry.address.clone(),
                user: entry.user.clone(),
            },
        );
    }
    crate::client::remember_host(&entry.name);
    if !entry.id.is_empty() {
        note_id(&entry.name, &entry.id, from_register);
    }
}

/// What `imkr hosts pair` and `join` do with the other side once the code
/// was right: its register joined in with its keys, itself in again at
/// the address it was reached at, its host keys in imkr's `known_hosts`,
/// and a host here. What `authorized_keys` changed.
pub fn take_paired(
    entry: Entry,
    register: &[Entry],
    host_keys: &[String],
) -> crate::pairing::Changes {
    let name = entry.name.clone();
    let own = crate::protocol::machine_id();
    // Its word about this machine does not count: this one knows itself.
    let theirs: Vec<Entry> = register
        .iter()
        .filter(|e| e.id != own && e.id != entry.id)
        .cloned()
        .collect();
    let (_, changes) = change_register_noting(|entries| {
        merge(entries, &theirs);
        // Paired on purpose: in again, newer than any removal.
        entries.retain(|e| e.id != entry.id && !(e.id.is_empty() && e.name == name));
        entries.push(entry.clone());
        entries.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    });
    if in_vpn(&entry.address) || is_loopback(&entry.address) {
        note_host_keys(&entry.address, host_keys);
    }
    take(&entry, false);
    crate::client::remember_host(&name);
    if let Ok(file) = paired_file() {
        let _ = std::fs::write(file, crate::persist::now().to_string());
    }
    changes
}

fn is_loopback(address: &str) -> bool {
    address.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Adds the host keys of `address` to imkr's own `known_hosts`, where ssh
/// by address looks (`by_address`).
fn note_host_keys(address: &str, keys: &[String]) {
    let Ok(dir) = dir() else {
        return;
    };
    let file = dir.join("known_hosts");
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    let before = text.len();
    for key in keys {
        let line = format!("{address} {key}");
        if !text.lines().any(|l| l.trim() == line) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            text.push('\n');
        }
    }
    if text.len() != before {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&file, text);
    }
}

/// Takes `name` out of the hosts here, with its address, without a new
/// removal in the register (it came from there).
fn drop_host(name: &str) {
    crate::client::forget_host(name);
    forget_id(name);
}

/// Asks the machine of `entry` for its sessions, along with the registers;
/// `None` if ssh did not get in or imkr did not answer.
fn reach(entry: &Entry) -> Option<Vec<crate::protocol::SessionInfo>> {
    let target = if ssh_knows(&entry.name) {
        by_name(&entry.name)
    } else {
        by_address(&Route {
            address: entry.address.clone(),
            user: entry.user.clone(),
        })
    };
    let output = Command::new("ssh")
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .args(target)
        .arg(crate::client::remote_command_with(
            &[
                (HOSTS_ENV, &register_to_send()),
                (CALLER_ENV, &crate::protocol::machine_id()),
            ],
            &["ls", "--json"],
        ))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sessions: Vec<crate::protocol::SessionInfo> =
        serde_json::from_slice(&output.stdout).ok()?;
    // An imkr from before the register does not say who it is.
    sessions
        .first()
        .is_some_and(|s| entry.id.is_empty() || s.machine_id.is_empty() || s.machine_id == entry.id)
        .then_some(sessions)
}

/// A NetBird peer to offer when a session starts elsewhere, with the
/// address to reach it at once the user picks it. imkr never tries it by
/// itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub name: String,
    pub address: String,
}

/// The NetBird peers that are online and neither a host here nor in the
/// register (where they are tried, or were taken out).
pub fn suggestions() -> Vec<Suggestion> {
    let known = crate::client::known_hosts();
    let entries = register();
    netbird_peers()
        .into_iter()
        .filter(|p| p.online && !known.contains(&p.name))
        .filter(|p| {
            !entries
                .iter()
                .any(|e| e.name == p.name || e.address == p.address)
        })
        .map(|p| Suggestion {
            name: p.name,
            address: p.address,
        })
        .collect()
}

/// The user picked `suggestion` to start a session on: ssh is to reach it
/// at its NetBird address unless it knows the name.
pub fn reach_suggested(suggestion: &Suggestion) {
    if !ssh_knows(&suggestion.name) && !routes().contains_key(&suggestion.name) {
        remember_route(
            &suggestion.name,
            Route {
                address: suggestion.address.clone(),
                user: String::new(),
            },
        );
    }
}

/// `imkr hosts vpn`: the machines of the Tailscale and NetBird networks,
/// and what imkr knows of each. None of them is tried.
pub fn print_vpn() -> Result<()> {
    let tailnet = crate::client::tailnet_peers();
    let netbird = netbird_peers();
    if tailnet.is_empty() && netbird.is_empty() {
        anyhow::bail!(
            "no machines found: is Tailscale or NetBird installed and up \
             (`tailscale status`, `netbird status`)?"
        );
    }
    let known = crate::client::known_hosts();
    let entries = register();
    let mut out = std::io::stdout().lock();
    for peer in tailnet {
        let what = match (known.contains(&peer.name), peer.online) {
            (true, _) => "used with imkr",
            (false, true) => "online · `imkr hosts add` or `imkr --host` uses it",
            (false, false) => "offline",
        };
        writeln!(out, "{:<24} tailscale {:<8} {what}", peer.name, peer.os)?;
    }
    for peer in netbird {
        let entry = entries
            .iter()
            .find(|e| e.address == peer.address || e.name == peer.name);
        let what = match entry {
            Some(e) if e.removed => format!("taken out as {}", e.name),
            Some(e) if known.contains(&e.name) => format!("used with imkr as {}", e.name),
            Some(e) => format!("in the register as {}, not reached yet", e.name),
            None if known.contains(&peer.name) => "used with imkr".into(),
            None if peer.online => "online · `imkr hosts add NAME ADDRESS` uses it".into(),
            None => "offline".into(),
        };
        writeln!(
            out,
            "{:<24} netbird   {:<15} {what}",
            peer.name, peer.address
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netbird_peers_by_the_first_label_with_their_address() {
        let status = serde_json::json!({
            "fqdn": "laptop.netbird.selfhosted",
            "peers": {"total": 3, "connected": 1, "details": [
                {"fqdn": "pimarchy-on-the-road.netbird.selfhosted",
                 "netbirdIp": "100.97.139.120", "status": "Connected"},
                {"fqdn": "iphone-x.netbird.selfhosted",
                 "netbirdIp": "100.97.37.222/32", "status": "Idle"},
                {"fqdn": "pe-l-2207.netbird.selfhosted",
                 "netbirdIp": "100.97.21.213", "status": "Connecting"},
                {"fqdn": "", "netbirdIp": "100.97.1.1", "status": "Connected"}
            ]}
        });
        let peers = parse_netbird(&status);
        assert_eq!(
            peers
                .iter()
                .map(|p| (p.name.as_str(), p.address.as_str(), p.online))
                .collect::<Vec<_>>(),
            [
                ("iphone-x", "100.97.37.222", true),
                ("pe-l-2207", "100.97.21.213", false),
                ("pimarchy-on-the-road", "100.97.139.120", true),
            ]
        );
    }

    #[test]
    fn only_vpn_addresses_take_a_new_host_key() {
        assert!(in_vpn("100.97.139.120"));
        assert!(in_vpn("100.64.0.1"));
        assert!(!in_vpn("100.128.0.1"));
        assert!(!in_vpn("192.168.1.23"));
        assert!(!in_vpn("desktop"));
        let vpn = by_address(&Route {
            address: "100.97.1.2".into(),
            user: "bastian".into(),
        });
        assert!(vpn.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(vpn.ends_with(&[
            "-l".into(),
            "bastian".into(),
            "--".into(),
            "100.97.1.2".into()
        ]));
        let lan = by_address(&Route {
            address: "192.168.1.23".into(),
            user: String::new(),
        });
        assert!(lan.ends_with(&["--".into(), "192.168.1.23".into()]));
        assert!(!lan.iter().any(|a| a.starts_with("StrictHostKeyChecking")));
    }

    fn entry(name: &str, id: &str, at: u64) -> Entry {
        Entry {
            name: name.into(),
            id: id.into(),
            at,
            ..Entry::default()
        }
    }

    #[test]
    fn a_key_is_never_lost_to_an_entry_without_one() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIkkkkkkkkkkkkkk";
        let mut mine = vec![entry("beta", "b1", 10)];
        let mut with_key = entry("beta", "b1", 10);
        with_key.key = key.into();
        // As old: the key fills the gap.
        assert!(merge(&mut mine, &[with_key]));
        assert_eq!(mine[0].key, key);
        // Newer, from a version without keys: the key stays.
        let mut moved = entry("beta", "b1", 20);
        moved.address = "100.97.0.9".into();
        assert!(merge(&mut mine, &[moved]));
        assert_eq!((mine[0].key.as_str(), mine[0].at), (key, 20));
    }

    #[test]
    fn the_newer_entry_of_a_machine_id_wins_a_new_name_or_address() {
        let mut mine = vec![entry("gamma", "g1", 10), entry("beta", "b1", 10)];
        let mut renamed = entry("gamma2", "g1", 20);
        renamed.address = "100.97.0.3".into();
        assert!(merge(&mut mine, &[renamed.clone(), entry("beta", "b1", 5)]));
        assert_eq!(mine, [entry("beta", "b1", 10), renamed.clone()]);
        // An older word on it changes nothing, and joining twice neither.
        assert!(!merge(&mut mine, &[entry("gamma", "g1", 15)]));
        assert!(!merge(&mut mine, &[renamed]));
    }

    #[test]
    fn a_removal_travels_until_the_machine_is_added_again() {
        let mut here = vec![entry("gamma", "g1", 10)];
        let mut there = here.clone();
        let mut removed = entry("gamma", "g1", 20);
        removed.removed = true;
        merge(&mut here, std::slice::from_ref(&removed));
        // Their older entry does not bring it back, the removal goes there.
        assert!(!merge(&mut here, &there));
        assert!(here[0].removed);
        merge(&mut there, &here);
        assert!(there[0].removed);
        // Added again by hand: newer than the removal.
        let again = entry("gamma", "g1", 30);
        merge(&mut there, std::slice::from_ref(&again));
        merge(&mut here, &there);
        assert_eq!(here, [again]);
    }

    #[test]
    fn an_entry_by_name_gives_way_to_the_machine_with_an_id() {
        // Added by hand, before anyone reached it.
        let mut added = entry("beta", "", 10);
        added.address = "100.97.0.2".into();
        let mut mine = vec![added];
        merge(&mut mine, &[entry("beta", "b1", 20)]);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, "b1");
        // A removal by name newer than what the machine said removes it.
        let mut gone = entry("beta", "", 30);
        gone.removed = true;
        merge(&mut mine, &[gone]);
        assert_eq!(mine.len(), 1);
        assert!(mine[0].removed && mine[0].id == "b1" && mine[0].at == 30);
    }

    #[test]
    fn an_address_of_the_vpn_replaces_others_only_fill_a_gap() {
        let mut entries = vec![entry("beta", "b1", 10)];
        note_address(&mut entries, "b1", "192.168.1.5");
        assert_eq!(entries[0].address, "192.168.1.5");
        note_address(&mut entries, "b1", "10.0.0.5");
        assert_eq!(entries[0].address, "192.168.1.5");
        note_address(&mut entries, "b1", "100.97.0.2");
        assert_eq!(entries[0].address, "100.97.0.2");
        assert!(entries[0].at > 10);
        note_address(&mut entries, "nobody", "100.97.0.9");
        assert_eq!(entries.len(), 1);
    }
}
