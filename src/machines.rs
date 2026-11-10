//! How the machines find each other (#9648): the peers of a NetBird
//! network (`netbird status --json`), the machines the known ones know,
//! and the way back to the one that called. And how ssh reaches a machine
//! without an entry in `~/.ssh/config`: by the address imkr noted for it.
//!
//! Nothing is shown as a machine before imkr answered there: a NetBird
//! network holds colleagues' laptops and phones, and a machine that one
//! host can reach need not be reachable from here. Every candidate is
//! tried once with ssh in batch mode (`probe`) and what came out is kept
//! (`checked.json`), so a machine without imkr is not asked again for a
//! day.
//!
//! Files in the state directory, next to `hosts` (which stays one name a
//! line, as older versions read it):
//! - `host-routes.json`: name → address and user, for ssh;
//! - `machines/checked.json`: what trying an address gave, and when;
//! - `machines/candidates.json`: machines heard of, not tried yet;
//! - `machines/forgotten`: names `imkr hosts rm` took out, which finding
//!   them again must not bring back;
//! - `machines/known_hosts`: host keys of machines reached by address.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
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

/// A machine another one knows, as `imkr ls --json` passes it on when
/// asked with `HOSTS_ENV`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: String,
    #[serde(default)]
    pub address: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
}

/// Set on `imkr ls --json` by a machine that wants this one's name and
/// the machines it knows along with the sessions.
pub const HOSTS_ENV: &str = "IMKR_HOSTS_TOO";
/// Set on `imkr ls --json` by the machine asking: its name and login,
/// `NAME USER`, so the one asked can find its way back.
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
        None => vec!["--".to_owned(), host.to_owned()],
    }
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
/// another host name, or the name resolves.
fn ssh_knows(host: &str) -> bool {
    if let Some((at, knows)) = SSH_KNOWS.lock().unwrap().get(host)
        && at.elapsed() < SSH_KNOWS_FOR
    {
        return *knows;
    }
    let (hostname, _) = ssh_config(host);
    let knows = hostname
        .as_deref()
        .is_some_and(|h| !h.eq_ignore_ascii_case(host))
        || (host, 22)
            .to_socket_addrs()
            .is_ok_and(|mut a| a.next().is_some());
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

/// The machines this one knows, for another that asked with `HOSTS_ENV`.
pub fn host_infos() -> Vec<HostInfo> {
    crate::client::known_hosts()
        .into_iter()
        .map(|name| {
            let route = route_of(&name);
            HostInfo {
                name,
                address: route.address,
                user: route.user,
            }
        })
        .collect()
}

/// A machine heard of, to be tried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// What it is called where it was heard of; replaced by the name imkr
    /// there gives itself.
    pub name: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
}

fn candidates_file() -> Result<PathBuf> {
    Ok(dir()?.join("candidates.json"))
}

/// Notes machines to try, for the next round of `discover`.
pub fn note_candidates(found: impl IntoIterator<Item = Candidate>) {
    let mut all: Vec<Candidate> = read_json(candidates_file());
    let before = all.len();
    for candidate in found {
        if candidate.address.is_empty() || all.iter().any(|c| c.address == candidate.address) {
            continue;
        }
        all.push(candidate);
    }
    if all.len() != before {
        let _ = write_json(candidates_file(), &all);
    }
}

/// For `imkr ls --json` started by another machine through ssh with
/// `CALLER_ENV`: that machine as a candidate, at the address ssh says it
/// came from. The way back: whoever reaches this machine is shown here
/// too, once this one reached it.
pub fn note_caller() {
    let Ok(caller) = std::env::var(CALLER_ENV) else {
        return;
    };
    let mut words = caller.split_whitespace();
    let (Some(name), user) = (words.next(), words.next().unwrap_or_default()) else {
        return;
    };
    let Some(address) = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|c| c.split_whitespace().next().map(str::to_owned))
        .filter(|a| a.parse::<IpAddr>().is_ok())
    else {
        return;
    };
    if crate::client::known_hosts().iter().any(|h| h == name)
        || forgotten().contains(&name.to_owned())
    {
        return;
    }
    note_candidates([Candidate {
        name: name.to_owned(),
        address,
        user: user.to_owned(),
    }]);
}

/// The value for `CALLER_ENV`: this machine's name and login.
pub fn caller() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    format!("{} {user}", crate::protocol::machine_name())
}

/// From the sessions `host` listed with `HOSTS_ENV`: the machines it
/// knows, as candidates. Itself, and the stand-in for a machine without a
/// session, are no machines to try.
pub fn learn_from(host: &str, sessions: &[crate::protocol::SessionInfo]) {
    let Some(first) = sessions.first() else {
        return;
    };
    let known = crate::client::known_hosts();
    let here = crate::protocol::machine_name();
    note_candidates(
        first
            .hosts
            .iter()
            .filter(|h| h.name != here && h.name != host && !known.contains(&h.name))
            .map(|h| Candidate {
                name: h.name.clone(),
                address: h.address.clone(),
                user: h.user.clone(),
            }),
    );
}

fn forgotten_file() -> Result<PathBuf> {
    Ok(dir()?.join("forgotten"))
}

/// The names `imkr hosts rm` took out.
pub fn forgotten() -> Vec<String> {
    forgotten_file()
        .and_then(|p| Ok(std::fs::read_to_string(p)?))
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Notes that `name` is not to come back by being found (`forget`), or
/// may again (`!forget`, from `imkr hosts add`).
pub fn set_forgotten(name: &str, forget: bool) {
    let mut names = forgotten();
    let had = names.iter().any(|n| n == name);
    if had == forget {
        return;
    }
    names.retain(|n| n != name);
    if forget {
        names.push(name.to_owned());
    }
    if let Ok(path) = forgotten_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = names.iter().map(|n| format!("{n}\n")).collect();
        let _ = std::fs::write(path, text);
    }
}

/// What trying an address gave.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    /// Seconds since 1970.
    pub at: u64,
    /// The name it was heard of by.
    pub name: String,
    /// ssh got in.
    pub ssh: bool,
    /// The name imkr there gave itself; empty without imkr.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub imkr: String,
    /// It is this machine.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
}

fn checked_file() -> Result<PathBuf> {
    Ok(dir()?.join("checked.json"))
}

/// What trying each address gave, by address.
pub fn checked() -> BTreeMap<String, Checked> {
    read_json(checked_file())
}

/// How long a machine without imkr (or without a way in) is left alone.
const CHECK_AGAIN: u64 = 24 * 3600;
/// How many machines one round tries, at once.
const PROBES_AT_ONCE: usize = 6;

/// Whether `address` is to be tried (again) now.
fn due(checked: &BTreeMap<String, Checked>, address: &str, now: u64) -> bool {
    checked
        .get(address)
        .is_none_or(|c| !c.own && c.imkr.is_empty() && now.saturating_sub(c.at) >= CHECK_AGAIN)
}

/// One round of finding machines, from the server's places thread every
/// minute: the NetBird peers that are online and the candidates heard of,
/// those not known and due tried at once (a handful per round), the ones
/// with imkr remembered as hosts with their route. Returns the machines
/// ssh reached that have no imkr yet, as suggestions: name and network.
///
/// Only one server of this machine does it at a time: two sessions
/// trying the same addresses doubles the ssh connections for nothing.
pub fn discover() -> Vec<(String, &'static str)> {
    let Some(_lock) = lock() else {
        return suggestions();
    };
    let now = crate::persist::now();
    let known = crate::client::known_hosts();
    let forgotten = forgotten();
    let checked = checked();
    let peers = netbird_peers();
    // Addresses of machines known already: no reason to try them again.
    let routes = routes();
    let known_addresses: Vec<String> = known
        .iter()
        .filter_map(|h| routes.get(h).map(|r| r.address.clone()))
        .collect();
    let mut todo: Vec<Candidate> = Vec::new();
    let heard: Vec<Candidate> = read_json(candidates_file());
    for candidate in heard
        .iter()
        .cloned()
        .chain(peers.iter().filter(|p| p.online).map(|p| Candidate {
            name: p.name.clone(),
            address: p.address.clone(),
            user: String::new(),
        }))
    {
        let skip = known.contains(&candidate.name)
            || forgotten.contains(&candidate.name)
            || known_addresses.contains(&candidate.address)
            || !due(&checked, &candidate.address, now)
            || todo.iter().any(|t| t.address == candidate.address);
        if !skip {
            todo.push(candidate);
        }
        if todo.len() == PROBES_AT_ONCE {
            break;
        }
    }
    let threads: Vec<_> = todo
        .into_iter()
        .map(|candidate| std::thread::spawn(move || (probe(&candidate), candidate)))
        .collect();
    let mut checked = checked;
    for thread in threads {
        let Ok((result, candidate)) = thread.join() else {
            continue;
        };
        if !result.imkr.is_empty() && !result.own {
            take(&result.imkr, &candidate, &forgotten);
        }
        checked.insert(candidate.address.clone(), result);
    }
    let _ = write_json(checked_file(), &checked);
    // Tried ones leave the candidates, whatever came out.
    let left: Vec<Candidate> = heard
        .into_iter()
        .filter(|c| !checked.contains_key(&c.address))
        .collect();
    let _ = write_json(candidates_file(), &left);
    suggestions()
}

/// Makes `name`, found at `candidate`'s address, a host of this machine,
/// with the address noted where ssh does not know the name.
fn take(name: &str, candidate: &Candidate, forgotten: &[String]) {
    if forgotten.iter().any(|n| n == name) || name == crate::protocol::machine_name() {
        return;
    }
    if !ssh_knows(name) {
        remember_route(
            name,
            Route {
                address: candidate.address.clone(),
                user: candidate.user.clone(),
            },
        );
    }
    if !crate::client::known_hosts().iter().any(|h| h == name) {
        crate::client::remember_host(name);
    }
}

/// Machines ssh reached that have no imkr, with the network they are in.
pub fn suggestions() -> Vec<(String, &'static str)> {
    let known = crate::client::known_hosts();
    let mut found: Vec<(String, &'static str)> = checked()
        .into_values()
        .filter(|c| c.ssh && c.imkr.is_empty() && !c.own && !known.contains(&c.name))
        .map(|c| (c.name, "NetBird"))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The lock of the machine's finding round; `None` while another server
/// holds it.
fn lock() -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    let path = dir().ok()?.join("discover.lock");
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = std::fs::File::create(path).ok()?;
    // SAFETY: flock on a descriptor we own; released when `file` closes.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

/// Tries `candidate`: is ssh's port open, does ssh get in without asking,
/// and does imkr answer there, with its name. The port first, so a phone
/// or a laptop without sshd costs one refused connection and nothing in
/// anyone's logs.
fn probe(candidate: &Candidate) -> Checked {
    let mut result = Checked {
        at: crate::persist::now(),
        name: candidate.name.clone(),
        user: candidate.user.clone(),
        ..Checked::default()
    };
    let port_open = candidate
        .address
        .parse::<IpAddr>()
        .map(|ip| vec![SocketAddr::new(ip, 22)])
        .or_else(|_| {
            (candidate.address.as_str(), 22)
                .to_socket_addrs()
                .map(|a| a.collect())
        })
        .unwrap_or_default()
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok());
    if !port_open {
        return result;
    }
    let route = Route {
        address: candidate.address.clone(),
        user: candidate.user.clone(),
    };
    let output = Command::new("ssh")
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .args(by_address(&route))
        .arg(crate::client::remote_command_with(
            &[(HOSTS_ENV, "1"), (CALLER_ENV, &caller())],
            &["ls", "--json"],
        ))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return result;
    };
    match output.status.code() {
        Some(0) => {
            result.ssh = true;
            let sessions: Vec<crate::protocol::SessionInfo> =
                serde_json::from_slice(&output.stdout).unwrap_or_default();
            let first = sessions.first();
            result.own = first.is_some_and(|s| s.machine_id == crate::protocol::machine_id());
            result.imkr = first
                .map(|s| s.machine.clone())
                .filter(|m| !m.is_empty())
                // An imkr from before #9648 does not say its name.
                .unwrap_or_else(|| candidate.name.clone());
        }
        Some(255) | None => {}
        // In, but the shell found no imkr (127) or it failed.
        Some(_) => result.ssh = true,
    }
    result
}

/// For `imkr ls --json` asked with `HOSTS_ENV`: this machine's name and id
/// and the machines it knows, on the first entry — on a stand-in when no
/// session runs, as the asking side makes one up for an empty list anyway.
pub fn tell_hosts(sessions: &mut Vec<crate::protocol::SessionInfo>) {
    note_caller();
    if std::env::var_os(HOSTS_ENV).is_none() {
        return;
    }
    if sessions.is_empty() {
        sessions.push(crate::protocol::SessionInfo {
            name: crate::protocol::DEFAULT_SESSION.to_owned(),
            ..Default::default()
        });
    }
    let first = &mut sessions[0];
    first.machine = crate::protocol::machine_name();
    first.machine_id = crate::protocol::machine_id();
    first.hosts = host_infos();
}

/// `imkr hosts vpn`: the machines of the Tailscale and NetBird networks,
/// and what imkr knows of each.
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
    let mut out = std::io::stdout().lock();
    for peer in tailnet {
        let what = match (known.contains(&peer.name), peer.online) {
            (true, _) => "used with imkr",
            (false, true) => "online · `imkr hosts add` or `imkr --host` uses it",
            (false, false) => "offline",
        };
        writeln!(out, "{:<24} tailscale {:<8} {what}", peer.name, peer.os)?;
    }
    let checked = checked();
    for peer in netbird {
        let what = match checked.get(&peer.address) {
            Some(c) if c.own => "this machine".to_owned(),
            Some(c) if !c.imkr.is_empty() && known.contains(&c.imkr) => {
                format!("used with imkr as {}", c.imkr)
            }
            Some(c) if !c.imkr.is_empty() => format!("imkr as {}, taken out", c.imkr),
            Some(c) if c.ssh => "ssh works, no imkr · `imkr hosts sync` after adding".into(),
            Some(_) => "no way in by ssh (key, or no sshd)".into(),
            None if peer.online => "not tried yet".into(),
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
        assert_eq!(lan, ["--", "192.168.1.23"]);
    }

    #[test]
    fn a_machine_without_imkr_waits_a_day_one_with_it_is_never_tried_again() {
        let mut checked = BTreeMap::new();
        assert!(due(&checked, "100.97.1.1", 1000));
        checked.insert(
            "100.97.1.1".to_owned(),
            Checked {
                at: 1000,
                ssh: true,
                ..Checked::default()
            },
        );
        assert!(!due(&checked, "100.97.1.1", 1000 + CHECK_AGAIN - 1));
        assert!(due(&checked, "100.97.1.1", 1000 + CHECK_AGAIN));
        checked.get_mut("100.97.1.1").unwrap().imkr = "pi".into();
        assert!(!due(&checked, "100.97.1.1", 1000 + 10 * CHECK_AGAIN));
    }
}
