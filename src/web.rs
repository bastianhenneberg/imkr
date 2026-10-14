//! `imkr web`: this machine's sessions in a browser, made for phones. A
//! small HTTP server in the binary itself, nothing to set up next to it:
//! the agents that wait or are done with their last lines and buttons to
//! answer, every session and pane, and each pane as a terminal (xterm.js,
//! embedded) over a WebSocket that taps the pane (`Request::Tap`).
//!
//! It listens where it is told (`--listen`, `web_listen`), else on this
//! machine's VPN address (Tailscale and NetBird use 100.64.0.0/10), else
//! on 127.0.0.1. Only paired devices get in: `imkr web pair` makes a code
//! that is good for ten minutes and five tries, the device keeps a token
//! in a cookie, `imkr web forget` takes it away again.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::client::Conn;
use crate::layout::PaneId;
use crate::protocol::{self, ClientMsg, Request, ServerMsg};

/// "imkr" on a phone's keypad.
pub const PORT: u16 = 4657;
/// How long a pairing code is good for, in seconds.
const PAIR_FOR: u64 = 600;
/// Wrong codes before a pairing code is dropped.
const PAIR_TRIES: u32 = 5;
const COOKIE: &str = "imkr_device";
/// The longest request taken: headers, and a body of text to type.
const MAX_HEAD: usize = 16 * 1024;
pub(crate) const MAX_BODY: usize = 256 * 1024;
/// How often a terminal's socket looks for output while it waits for input.
const POLL: Duration = Duration::from_millis(15);

const INDEX: &str = include_str!("../web/index.html");
const PAIR_PAGE: &str = include_str!("../web/pair.html");
const MANIFEST: &str = include_str!("../web/manifest.json");
const ICON: &str = include_str!("../web/icon.svg");
const XTERM_JS: &str = include_str!("../web/vendor/xterm.js");
const XTERM_CSS: &str = include_str!("../web/vendor/xterm.css");
const FIT_JS: &str = include_str!("../web/vendor/addon-fit.js");
/// Nerd Fonts' symbols, for the icons prompts and programs use: phones have
/// no such font.
const SYMBOLS_FONT: &[u8] = include_bytes!("../web/vendor/symbols-nerd-font-mono.woff2");

pub fn main(args: &[String]) -> Result<()> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => serve(None, false),
        ["--listen", addr] => serve(Some(addr), false),
        ["--background"] => serve(None, true),
        ["pair"] => {
            let addr = listen_addr(None)?;
            let code = new_code()?;
            let url = pair_url(addr, &code);
            print_qr(&url);
            println!("Pairing code: {code} (good for {} minutes)", PAIR_FOR / 60);
            println!("Open on the device: {url}");
            if let Some(hint) = firewall_hint(addr) {
                println!("{hint}");
            }
            Ok(())
        }
        ["devices"] => {
            let devices = load_devices();
            if devices.is_empty() {
                println!("No paired devices; `imkr web pair` pairs one.");
            }
            for device in devices {
                let when = protocol::chrono_like(device.paired).unwrap_or_default();
                println!("{}  (paired {when})", device.name);
            }
            Ok(())
        }
        ["forget", name] => {
            let mut devices = load_devices();
            let before = devices.len();
            devices.retain(|d| *name != "all" && d.name != *name);
            if devices.len() == before {
                bail!("no paired device {name}, see `imkr web devices`");
            }
            save_devices(&devices)?;
            println!("forgot {} device(s)", before - devices.len());
            Ok(())
        }
        _ => bail!(
            "usage: imkr web [--listen ADDR[:PORT]] | imkr web pair | imkr web devices | imkr web forget NAME|all"
        ),
    }
}

/// Where to listen: `asked`, else the setting `web_listen`, else this
/// machine's VPN address, else 127.0.0.1; on `PORT` unless one is given.
pub fn listen_addr(asked: Option<&str>) -> Result<SocketAddr> {
    let configured = crate::config::Config::load().0.web_listen;
    let wanted = asked.map(str::to_owned).or(configured);
    let Some(wanted) = wanted.filter(|w| !w.trim().is_empty()) else {
        let ip = vpn_address().unwrap_or(IpAddr::from([127, 0, 0, 1]));
        return Ok(SocketAddr::new(ip, PORT));
    };
    let wanted = wanted.trim();
    if let Ok(addr) = wanted.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let ip: IpAddr = wanted
        .parse()
        .with_context(|| format!("{wanted} is no address to listen on, e.g. 100.64.1.2:{PORT}"))?;
    Ok(SocketAddr::new(ip, PORT))
}

/// This machine's address in a Tailscale or NetBird network: both hand out
/// addresses from 100.64.0.0/10, which nothing else on a LAN uses.
pub fn vpn_address() -> Option<IpAddr> {
    interfaces()
        .into_iter()
        .map(|(_, ip)| ip)
        .find(|ip| match ip {
            IpAddr::V4(v4) => {
                let [a, b, ..] = v4.octets();
                a == 100 && (64..128).contains(&b)
            }
            IpAddr::V6(_) => false,
        })
}

/// This machine's IPv4 addresses with the names of their interfaces.
fn interfaces() -> Vec<(String, IpAddr)> {
    let mut found = Vec::new();
    // SAFETY: getifaddrs fills a list that freeifaddrs frees; it is only
    // read in between.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return found;
        }
        let mut at = list;
        while !at.is_null() {
            let addr = (*at).ifa_addr;
            if !addr.is_null() && i32::from((*addr).sa_family) == libc::AF_INET {
                let v4 = &*(addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr));
                let name = std::ffi::CStr::from_ptr((*at).ifa_name)
                    .to_string_lossy()
                    .into_owned();
                found.push((name, IpAddr::V4(ip)));
            }
            at = (*at).ifa_next;
        }
        libc::freeifaddrs(list);
    }
    found
}

/// What to run when ufw is on and keeps phones from `addr`: nothing would
/// reach `imkr web`, and the browser only says the page is not there.
fn firewall_hint(addr: SocketAddr) -> Option<String> {
    let conf = std::fs::read_to_string("/etc/ufw/ufw.conf").ok()?;
    let rules = std::fs::read_to_string("/etc/ufw/user.rules").ok()?;
    let interface = interfaces()
        .into_iter()
        .find(|(_, ip)| *ip == addr.ip())
        .map(|(name, _)| name);
    ufw_hint(&conf, &rules, addr, interface.as_deref())
}

fn ufw_hint(conf: &str, rules: &str, addr: SocketAddr, interface: Option<&str>) -> Option<String> {
    if addr.ip().is_loopback() || !conf.lines().any(|l| l.trim() == "ENABLED=yes") {
        return None;
    }
    let port = addr.port();
    let dport = format!("--dport {port} ");
    if rules
        .lines()
        .any(|l| l.contains(&dport) && l.contains("-j ACCEPT"))
    {
        return None;
    }
    let on = interface.map_or(String::new(), |name| format!("in on {name} "));
    Some(format!(
        "The firewall (ufw) keeps other devices out. To let them in:\n  \
         sudo ufw allow {on}to any port {port} proto tcp"
    ))
}

/// Shows `text` as a QR code for a phone's camera, if this is a terminal:
/// two rows of modules per line, in black and white of their own so that
/// the terminal's colors do not invert it.
fn print_qr(text: &str) {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }
    for line in qr_lines(text).unwrap_or_default() {
        println!("{line}");
    }
}

fn qr_lines(text: &str) -> Option<Vec<String>> {
    // A quiet zone of two modules all around.
    const QUIET: usize = 2;
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let size = width + 2 * QUIET;
    let dark = |x: usize, y: usize| {
        (QUIET..QUIET + width).contains(&x)
            && (QUIET..QUIET + width).contains(&y)
            && colors[(y - QUIET) * width + (x - QUIET)] == qrcode::Color::Dark
    };
    let mut lines = Vec::new();
    for y in (0..size).step_by(2) {
        let mut line = String::new();
        for x in 0..size {
            let fg = if dark(x, y) { 30 } else { 97 };
            let bg = if dark(x, y + 1) { 40 } else { 107 };
            line.push_str(&format!("\x1b[{fg};{bg}m▀"));
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }
    Some(lines)
}

pub(crate) fn url(addr: SocketAddr) -> String {
    format!("http://{addr}/")
}

fn pair_url(addr: SocketAddr, code: &str) -> String {
    format!("http://{addr}/pair?code={code}")
}

fn serve(asked: Option<&str>, background: bool) -> Result<()> {
    let addr = listen_addr(asked)?;
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        // Already served, by an earlier start.
        Err(e) if background && e.kind() == std::io::ErrorKind::AddrInUse => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("cannot listen on {addr}")),
    };
    restart_when_replaced();
    if background {
        log("-", &format!("serving on {}", url(addr)));
        if let Some(hint) = firewall_hint(addr) {
            log("-", &hint.replace('\n', ""));
        }
    }
    if !background {
        println!("imkr web on {}", url(addr));
        if addr.ip().is_loopback() {
            println!(
                "Only this machine reaches it: on a phone, use a VPN (Tailscale, NetBird) \
                 or `imkr web --listen ADDR`."
            );
        } else if addr.ip().is_unspecified() {
            println!("Warning: every network this machine is on reaches it.");
        }
        if let Some(hint) = firewall_hint(addr) {
            println!("{hint}");
        }
        if load_devices().is_empty() {
            let code = new_code()?;
            let url = pair_url(addr, &code);
            print_qr(&url);
            println!("No device paired yet. Code: {code}, or open {url}");
        } else {
            println!("`imkr web pair` pairs another device.");
        }
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let _ = std::thread::Builder::new()
            .name("web".into())
            .spawn(move || handle(stream));
    }
    Ok(())
}

/// After `imkr update` replaced the binary, runs the new one: open pages
/// connect again by themselves.
fn restart_when_replaced() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("web-update".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let replaced = std::fs::read_link("/proc/self/exe")
                    .is_ok_and(|now| now.to_string_lossy().ends_with(" (deleted)"));
                let path = exe.to_string_lossy();
                let path = PathBuf::from(path.trim_end_matches(" (deleted)"));
                if replaced && path.exists() {
                    use std::os::unix::process::CommandExt;
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let e = std::process::Command::new(&path).args(args).exec();
                    eprintln!("imkr web: could not restart: {e}");
                    return;
                }
            }
        });
}

/// Starts `imkr web --background` if it is switched on and nothing answers
/// on its address; called now and then by every server, so it runs after
/// a reboot and comes back if it ended.
pub fn keep_running() {
    let config = crate::config::Config::load().0;
    if !config.web {
        return;
    }
    let Ok(addr) = listen_addr(None) else { return };
    let probe = if addr.ip().is_unspecified() {
        SocketAddr::new(IpAddr::from([127, 0, 0, 1]), addr.port())
    } else {
        addr
    };
    if TcpStream::connect_timeout(&probe, Duration::from_millis(500)).is_ok() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let log = crate::persist::state_dir()
        .ok()
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::File::create(dir.join("web.log")).ok()
        })
        .map_or_else(std::process::Stdio::null, Into::into);
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["web", "--background"])
        .env_remove("IMKR_SOCKET")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log);
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe; the server's end is not its end.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Ok(mut child) = cmd.spawn() {
        // Waited for, so it does not linger as a zombie when it ends.
        let _ = std::thread::Builder::new()
            .name("web-wait".into())
            .spawn(move || child.wait());
    }
}

// --- Devices and pairing ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Device {
    name: String,
    token: String,
    /// Seconds since 1970.
    paired: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Pairing {
    code: String,
    until: u64,
    tries: u32,
}

fn devices_path() -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("web-devices.json"))
}

// --- Who is connected ---

/// A paired device that has the page open, for the sidebar of the
/// sessions here (`Places::web`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub device: String,
    pub address: String,
    /// When it last asked for something.
    pub at: u64,
    /// The pane it shows as a terminal, as `session › pane`, with the
    /// machine in front when it is another one.
    #[serde(default)]
    pub viewing: Option<String>,
}

/// How long a device counts as there after it last asked: the page asks
/// every 3 s while shown, a terminal says it is there every `STILL_THERE`.
const SEEN_FOR: u64 = 15;
const STILL_THERE: Duration = Duration::from_secs(5);

static SEEN: std::sync::Mutex<Vec<Seen>> = std::sync::Mutex::new(Vec::new());

fn seen_path() -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("web-seen.json"))
}

/// Notes that `device` asked from `address`, showing `viewing` (or not,
/// `Some(None)`; `None` keeps what it showed), for `connected`.
fn note_seen(device: &str, address: &str, viewing: Option<Option<String>>) {
    let mut seen = SEEN.lock().unwrap();
    let now = now();
    seen.retain(|s| s.at + SEEN_FOR > now || s.viewing.is_some());
    let at = seen.iter().position(|s| s.device == device);
    let old = at.map(|i| seen[i].clone());
    let entry = match at {
        Some(i) => &mut seen[i],
        None => {
            seen.push(Seen {
                device: device.to_owned(),
                ..Seen::default()
            });
            seen.last_mut().expect("pushed")
        }
    };
    entry.address = address.to_owned();
    entry.at = now;
    if let Some(viewing) = viewing {
        entry.viewing = viewing;
    }
    // Written when something shows differently, else every few seconds.
    let changed = old.is_none_or(|old| {
        old.viewing != entry.viewing || old.address != entry.address || old.at + 4 <= now
    });
    if changed && let (Ok(path), Ok(bytes)) = (seen_path(), serde_json::to_vec(&*seen)) {
        let _ = write_private(&path, &bytes);
    }
}

/// The devices that have the page of `imkr web` here open now.
pub fn connected() -> Vec<Seen> {
    let Some(seen) = seen_path()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<Seen>>(&bytes).ok())
    else {
        return Vec::new();
    };
    let now = now();
    seen.into_iter().filter(|s| s.at + SEEN_FOR > now).collect()
}

fn pairing_path() -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("web-pairing.json"))
}

fn load_devices() -> Vec<Device> {
    devices_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_devices(devices: &[Device]) -> Result<()> {
    write_private(&devices_path()?, &serde_json::to_vec_pretty(devices)?)
}

/// Writes a file only this user reads: tokens let anyone type into panes.
pub(crate) fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// A new pairing code, replacing any earlier one: `ABCD-EFGH`, without
/// letters that look alike.
fn new_code() -> Result<String> {
    let code = random_code()?;
    let pairing = Pairing {
        code: code.clone(),
        until: now() + PAIR_FOR,
        tries: 0,
    };
    write_private(&pairing_path()?, &serde_json::to_vec(&pairing)?)?;
    Ok(code)
}

/// A code to type over: `ABCD-EFGH`, without letters that look alike.
pub(crate) fn random_code() -> Result<String> {
    const LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = random_bytes::<8>()?;
    let mut code: String = bytes
        .iter()
        .map(|b| char::from(LETTERS[usize::from(*b) % LETTERS.len()]))
        .collect();
    code.insert(4, '-');
    Ok(code)
}

/// A secret for a device or a contact, as 64 hex digits.
pub(crate) fn random_token() -> Result<String> {
    Ok(random_bytes::<32>()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Whether two secrets are the same, in a time that does not tell how much
/// of one was right.
pub(crate) fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

pub(crate) fn same_code(a: &str, b: &str) -> bool {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_uppercase())
            .collect()
    };
    clean(a) == clean(b)
}

/// Pairs a device if `code` is the current pairing code; returns its token.
/// Pairs a device with `code`: its token and the name it got.
fn pair(code: &str, name: &str) -> Result<(String, String), &'static str> {
    let path = pairing_path().map_err(|_| "no pairing code")?;
    let pairing: Option<Pairing> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let Some(mut pairing) = pairing.filter(|p| p.until > now()) else {
        return Err("No pairing code is open: run `imkr web pair` on the machine.");
    };
    if !same_code(&pairing.code, code) {
        pairing.tries += 1;
        if pairing.tries >= PAIR_TRIES {
            let _ = std::fs::remove_file(&path);
        } else if let Ok(bytes) = serde_json::to_vec(&pairing) {
            let _ = write_private(&path, &bytes);
        }
        return Err("Wrong code.");
    }
    let _ = std::fs::remove_file(&path);
    let token = random_token().map_err(|_| "no randomness")?;
    let mut devices = load_devices();
    let mut name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim()
        .to_owned();
    if name.is_empty() {
        name = "device".into();
    }
    let base = name.clone();
    let mut n = 2;
    while devices.iter().any(|d| d.name == name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    devices.push(Device {
        name: name.clone(),
        token: token.clone(),
        paired: now(),
    });
    save_devices(&devices).map_err(|_| "could not save the device")?;
    Ok((token, name))
}

// --- HTTP ---

struct HttpRequest {
    /// Who asked, by address.
    peer: String,
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value)
        })
    }

    /// The device this request comes from, if it is a paired one.
    fn device(&self) -> Option<Device> {
        let token = self.cookie(COOKIE)?;
        load_devices().into_iter().find(|d| d.token == token)
    }

    /// Whether a page of this server sent it, not some other site in the
    /// same browser: browsers send `Origin` with every POST and WebSocket.
    fn same_origin(&self) -> bool {
        match (self.header("origin"), self.header("host")) {
            (Some(origin), Some(host)) => origin
                .split_once("://")
                .is_some_and(|(_, rest)| rest.trim_end_matches('/') == host),
            (None, _) => true,
            _ => false,
        }
    }
}

/// The request coming in, `None` when the connection closes before it.
fn read_request(reader: &mut BufReader<TcpStream>) -> Result<Option<HttpRequest>> {
    let mut head = Vec::new();
    loop {
        let before = head.len();
        let n = reader.by_ref().take(4096).read_until(b'\n', &mut head)?;
        if n == 0 {
            if head.is_empty() {
                return Ok(None);
            }
            bail!("closed in the middle of a request");
        }
        if head.len() > MAX_HEAD {
            bail!("headers too long");
        }
        if head[before..] == *b"\r\n" || head[before..] == *b"\n" {
            break;
        }
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers: HashMap<String, String> = lines
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        bail!("body too long");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(HttpRequest {
        peer: String::new(),
        method,
        path: path.to_owned(),
        query: parse_query(query),
        headers,
        body,
    }))
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (!key.is_empty()).then(|| (percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Response {
    status: &'static str,
    kind: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    /// What goes into the log: why a request was turned away, or that a
    /// device was paired.
    note: Option<String>,
}

impl Response {
    fn new(status: &'static str, kind: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            kind,
            headers: Vec::new(),
            body: body.into(),
            note: None,
        }
    }

    fn noted(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn json(value: &serde_json::Value) -> Self {
        Self::new("200 OK", "application/json", value.to_string())
    }

    fn error(status: &'static str, message: &str) -> Self {
        Self::new(
            status,
            "application/json",
            serde_json::json!({ "error": message }).to_string(),
        )
        .noted(message)
    }

    fn with(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }

    fn write_to(self, stream: &mut TcpStream) -> std::io::Result<()> {
        let mut head = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
             X-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\n\
             X-Frame-Options: DENY\r\n",
            self.status,
            self.kind,
            self.body.len()
        );
        if !self
            .headers
            .iter()
            .any(|(name, _)| *name == "Cache-Control")
        {
            head.push_str("Cache-Control: no-store\r\n");
        }
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

/// A line for the log (stderr, `<state>/web.log` when in the background),
/// with the time and who asked.
pub(crate) fn log(peer: &str, text: &str) {
    eprintln!("{} {peer} {text}", stamp());
}

/// Now as local date and time, for the log.
fn stamp() -> String {
    let t = now() as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return t.to_string();
    }
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Answers one connection; what goes wrong goes into the log.
fn handle(stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map_or("?".into(), |addr| addr.ip().to_string());
    let result = (|| -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        // Closed without a word: `keep_running` looking whether it runs.
        let Some(mut request) = read_request(&mut reader)? else {
            return Ok(());
        };
        request.peer = peer.clone();
        let mut stream = stream;
        let response = if request.method == "GET" && request.path == "/ws" {
            match request.device() {
                Some(_) if request.same_origin() => return Ok(terminal(stream, &request)?),
                Some(_) => Response::error("403 Forbidden", "wrong origin"),
                None => Response::error("403 Forbidden", "not paired"),
            }
        } else {
            route(&request)
        };
        if let Some(note) = &response.note {
            log(
                &peer,
                &format!(
                    "{} {} → {}: {note}",
                    request.method, request.path, response.status
                ),
            );
        }
        response.write_to(&mut stream)?;
        Ok(())
    })();
    if let Err(e) = &result {
        log(&peer, &format!("{e:#}"));
    }
}

fn route(request: &HttpRequest) -> Response {
    let cached = |kind, body: &[u8]| {
        Response::new("200 OK", kind, body).with("Cache-Control", "public, max-age=86400".into())
    };
    let device = request.device();
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/xterm.js") => cached("text/javascript", XTERM_JS.as_bytes()),
        ("GET", "/xterm.css") => cached("text/css", XTERM_CSS.as_bytes()),
        ("GET", "/addon-fit.js") => cached("text/javascript", FIT_JS.as_bytes()),
        ("GET", "/manifest.json") => cached("application/manifest+json", MANIFEST.as_bytes()),
        ("GET", "/icon.svg") => cached("image/svg+xml", ICON.as_bytes()),
        ("GET", "/symbols.woff2") => cached("font/woff2", SYMBOLS_FONT),
        // Other people's imkrs, not browsers: those send `Origin`, and
        // `Content-Type: application/json` takes a preflight we never allow.
        ("POST", "/contact/join" | "/contact/message")
            if request.header("origin").is_some()
                || request.header("content-type") != Some("application/json") =>
        {
            Response::error("403 Forbidden", "only for imkr")
        }
        ("POST", "/contact/join") => match crate::contacts::accept_join(&request.body) {
            Ok(value) => Response::json(&value).noted(format!(
                "new contact {}",
                value["as"].as_str().unwrap_or("?")
            )),
            Err(e) => Response::error("403 Forbidden", &e),
        },
        ("POST", "/contact/message") => {
            let token = request
                .header("authorization")
                .and_then(|h| h.strip_prefix("Bearer "));
            match crate::contacts::receive(token, &request.body) {
                Ok(value) => Response::json(&value),
                Err(e) => Response::error("403 Forbidden", &e),
            }
        }
        ("GET", "/pair") => pair_page(request.query.get("code").map_or("", String::as_str), ""),
        ("POST", "/pair") => {
            if !request.same_origin() {
                return Response::error("403 Forbidden", "wrong origin");
            }
            let form = parse_query(&String::from_utf8_lossy(&request.body));
            let code = form.get("code").map_or("", String::as_str);
            let name = form.get("name").map_or("", String::as_str);
            match pair(code, name) {
                Ok((token, name)) => Response::new("303 See Other", "text/plain", "paired")
                    .noted(format!("paired device {name}"))
                    .with("Location", "/".into())
                    .with(
                        "Set-Cookie",
                        format!(
                            "{COOKIE}={token}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Strict"
                        ),
                    ),
                Err(message) => pair_page(code, message),
            }
        }
        ("GET", "/") if device.is_none() => pair_page("", ""),
        ("GET", "/") => Response::new(
            "200 OK",
            "text/html; charset=utf-8",
            INDEX
                .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
                .replace("{{MACHINE}}", &html_escape(&protocol::machine_name())),
        ),
        (_, path) if path.starts_with("/api/") && device.is_none() => {
            Response::error("401 Unauthorized", "not paired")
        }
        ("GET", "/api/state") => {
            if let Some(device) = &device {
                note_seen(&device.name, &request.peer, None);
            }
            Response::json(&serde_json::json!({
            "machine": protocol::machine_name(),
            "version": env!("CARGO_PKG_VERSION"),
            "device": device.map(|d| d.name),
            "sessions": crate::client::local_sessions(),
            "hosts": other_machines(),
            }))
        }
        ("POST", "/api/send") if request.same_origin() => send(&request.body),
        _ => Response::error("404 Not Found", "nothing here"),
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn pair_page(code: &str, message: &str) -> Response {
    let page = Response::new(
        if message.is_empty() {
            "200 OK"
        } else {
            "403 Forbidden"
        },
        "text/html; charset=utf-8",
        PAIR_PAGE
            .replace("{{CODE}}", &html_escape(code))
            .replace("{{MESSAGE}}", &html_escape(message))
            .replace("{{MACHINE}}", &html_escape(&protocol::machine_name())),
    );
    if message.is_empty() {
        page
    } else {
        page.noted(message)
    }
}

#[derive(Deserialize)]
struct Send {
    /// Another machine, by the name ssh reaches it by.
    #[serde(default)]
    host: Option<String>,
    session: String,
    pane: PaneId,
    text: String,
    #[serde(default)]
    enter: bool,
}

/// Types into a pane of a session here, see `Request::Send`.
fn send(body: &[u8]) -> Response {
    let Ok(send) = serde_json::from_slice::<Send>(body) else {
        return Response::error("400 Bad Request", "expected session, pane, text");
    };
    if let Some(host) = &send.host {
        let reply = known_machine(host, &send.session).and_then(|()| {
            let pane = send.pane.to_string();
            let mut args = vec!["-s", &send.session, "send", "--stdin", "--pane", &pane];
            if !send.enter {
                args.push("--no-enter");
            }
            crate::client::remote_output(host, &args, &send.text)
        });
        return match reply {
            Ok(_) => Response::json(&serde_json::json!({})),
            Err(e) => Response::error("409 Conflict", &format!("{e:#}")),
        };
    }
    let reply = session_path(&send.session).and_then(|path| {
        crate::client::request_at(
            &path,
            Request::Send {
                pane: send.pane,
                text: send.text,
                enter: send.enter,
            },
        )
    });
    match reply {
        Ok(value) => Response::json(&value),
        Err(e) => Response::error("409 Conflict", &format!("{e:#}")),
    }
}

// --- Other machines ---

/// How old the machines' sessions may get when this process asks them.
const HOSTS_EVERY: Duration = Duration::from_secs(15);

/// The sessions on the other machines. The sessions here ask them every
/// 15 s anyway, through ssh (`Request::Places`); only when none runs does
/// this process ask, in the background, so a page never waits for ssh.
fn other_machines() -> Vec<protocol::HostInfo> {
    let mut found: Vec<protocol::HostInfo> = Vec::new();
    let mut asked = false;
    for session in crate::client::local_sessions() {
        if session.dormant.is_some() {
            continue;
        }
        let Ok(path) = protocol::session_socket(&session.name) else {
            continue;
        };
        let Ok(value) = crate::client::request_at(&path, Request::Places) else {
            continue;
        };
        let Ok(hosts) = serde_json::from_value::<Vec<protocol::HostInfo>>(value) else {
            continue;
        };
        asked = true;
        for host in hosts {
            if !found.iter().any(|h| h.host == host.host) {
                found.push(host);
            }
        }
    }
    if asked { found } else { polled_machines() }
}

#[derive(Default)]
struct Polled {
    at: Option<std::time::Instant>,
    asking: bool,
    hosts: Vec<protocol::HostInfo>,
}

static POLLED: std::sync::Mutex<Polled> = std::sync::Mutex::new(Polled {
    at: None,
    asking: false,
    hosts: Vec::new(),
});

/// The machines' sessions as this process last asked them; asks again in
/// the background when that is longer than `HOSTS_EVERY` ago.
fn polled_machines() -> Vec<protocol::HostInfo> {
    let mut polled = POLLED.lock().unwrap();
    let stale = polled.at.is_none_or(|at| at.elapsed() >= HOSTS_EVERY);
    if stale && !polled.asking {
        polled.asking = true;
        std::thread::spawn(|| {
            let asked: Vec<_> = crate::client::known_hosts()
                .into_iter()
                .map(|host| {
                    std::thread::spawn(move || {
                        let sessions = crate::client::remote_sessions(&host).ok()?;
                        Some(protocol::HostInfo { host, sessions })
                    })
                })
                .collect();
            let hosts = asked
                .into_iter()
                .filter_map(|asking| asking.join().ok().flatten())
                .collect();
            let mut polled = POLLED.lock().unwrap();
            *polled = Polled {
                at: Some(std::time::Instant::now()),
                asking: false,
                hosts,
            };
        });
    }
    polled.hosts.clone()
}

/// Whether `session` runs on `host`, one of the machines the page shows:
/// nothing else is handed to ssh.
fn known_machine(host: &str, session: &str) -> Result<()> {
    if !protocol::valid_session_name(session) {
        bail!("no session {session}");
    }
    let machines = other_machines();
    let Some(machine) = machines.iter().find(|m| m.host == host) else {
        bail!("{host} is not one of the machines here");
    };
    if !machine.sessions.iter().any(|s| s.name == session) {
        bail!("no session {session} on {host}");
    }
    Ok(())
}

fn session_path(name: &str) -> Result<PathBuf> {
    if !protocol::valid_session_name(name) {
        bail!("no session {name}");
    }
    protocol::session_socket(name)
}

// --- Terminal ---

/// Shows a pane over a WebSocket: output as binary messages, a first text
/// message with its size, `{"exited":true}` when it ends; binary messages
/// coming back are typed into it. With `fit` the pane takes the size in
/// `rows` and `cols` while shown; the page connects again for another.
/// Without, it only watches: the pane keeps its size, and the server cuts
/// the tap off when the size changes, so the page connects again.
fn terminal(mut stream: TcpStream, request: &HttpRequest) -> std::io::Result<()> {
    use tungstenite::{Message, protocol::Role};
    let Some(key) = request.header("sec-websocket-key") else {
        return Response::error("400 Bad Request", "not a WebSocket").write_to(&mut stream);
    };
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    let mut ws = tungstenite::WebSocket::from_raw_socket(stream.try_clone()?, Role::Server, None);
    if request.query.get("attach").is_some_and(|a| a == "1") {
        return attach(ws, stream, request);
    }
    let text = |value: serde_json::Value| Message::text(value.to_string());
    let session = request.query.get("session").cloned().unwrap_or_default();
    let pane: Option<PaneId> = request.query.get("pane").and_then(|p| p.parse().ok());
    let fit = request.query.get("fit").is_some_and(|f| f == "1");
    let host = request.query.get("host").filter(|h| !h.is_empty());
    // The socket here, to end the tap with: the reading thread holds it too.
    let mut socket = None;
    let tapped = (|| -> Result<(Conn, serde_json::Value)> {
        let pane = pane.context("no pane")?;
        let mut conn = match host {
            // Through ssh to `imkr proxy` there, as `imkr --host` does.
            Some(host) => {
                known_machine(host, &session)?;
                Conn::remote(host, Some(&session), false)?
            }
            None => {
                let unix = UnixStream::connect(session_path(&session)?)
                    .with_context(|| format!("session {session} does not run"))?;
                socket = Some(unix.try_clone()?);
                Conn::local(unix)?
            }
        };
        let size = request
            .query
            .get("rows")
            .zip(request.query.get("cols"))
            .and_then(|(rows, cols)| Some((rows.parse::<u16>().ok()?, cols.parse::<u16>().ok()?)))
            .map(|(rows, cols)| (rows.clamp(3, 500), cols.clamp(10, 1000)));
        let tap = Request::Tap {
            pane,
            watch: !fit,
            size,
        };
        ClientMsg::Request(tap).write_to(&mut conn.writer)?;
        conn.writer.flush()?;
        match ServerMsg::read_from(&mut conn.reader)? {
            Some(ServerMsg::Reply(Ok(value))) => Ok((conn, value)),
            Some(ServerMsg::Reply(Err(message))) => bail!(message),
            _ => bail!("imkr there is too old to show panes, run `imkr update`"),
        }
    })();
    let (mut conn, reply) = match tapped {
        Ok(tapped) => tapped,
        Err(e) => {
            let _ = ws.send(text(serde_json::json!({ "error": format!("{e:#}") })));
            let _ = ws.close(None);
            let _ = ws.flush();
            return Ok(());
        }
    };
    if ws
        .send(text(serde_json::json!({
            "rows": reply["rows"], "cols": reply["cols"], "fit": fit,
        })))
        .is_err()
    {
        return Ok(());
    }
    let device = request.device().map(|d| d.name).unwrap_or_default();
    let shown = match host {
        Some(host) => format!("{host}:{session} #{}", pane.unwrap_or_default()),
        None => format!("{session} #{}", pane.unwrap_or_default()),
    };
    note_seen(&device, &request.peer, Some(Some(shown)));
    let mut told = std::time::Instant::now();
    // Output comes in on its own thread; this one waits for input, briefly,
    // and passes on what came meanwhile.
    let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
    let mut output = std::mem::replace(&mut conn.reader, Box::new(std::io::empty()));
    std::thread::Builder::new()
        .name("web-tap".into())
        .spawn(move || {
            loop {
                match ServerMsg::read_from(&mut output) {
                    Ok(Some(ServerMsg::Output(bytes))) => {
                        if tx.send(Some(bytes)).is_err() {
                            break;
                        }
                    }
                    Ok(Some(ServerMsg::Exited)) => {
                        let _ = tx.send(None);
                        break;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        })?;
    stream.set_read_timeout(Some(POLL))?;
    let ended = 'session: loop {
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if ClientMsg::Input(bytes.to_vec())
                    .write_to(&mut conn.writer)
                    .and_then(|()| conn.writer.flush())
                    .is_err()
                {
                    break false;
                }
            }
            Ok(Message::Close(_)) => break false,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break false,
        }
        let mut pending = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Some(bytes)) => pending.extend_from_slice(&bytes),
                Ok(None) => {
                    if !pending.is_empty() {
                        let _ = ws.send(Message::binary(pending));
                    }
                    break 'session true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // Cut off: the pane changed its size, or the server was
                // updated. The page connects again.
                Err(mpsc::TryRecvError::Disconnected) => break 'session false,
            }
        }
        if !pending.is_empty() && ws.send(Message::binary(pending)).is_err() {
            break false;
        }
        if told.elapsed() >= STILL_THERE {
            told = std::time::Instant::now();
            note_seen(&device, &request.peer, None);
        }
    };
    note_seen(&device, &request.peer, Some(None));
    if ended {
        let _ = ws.send(text(serde_json::json!({ "exited": true })));
    }
    let _ = ws.close(None);
    let _ = ws.flush();
    // Ends the ssh process for another machine; here, the socket.
    drop(conn);
    if let Some(socket) = socket {
        let _ = socket.shutdown(std::net::Shutdown::Both);
    }
    Ok(())
}

/// A whole session in the browser: `imkr` itself, as a client in a pty of
/// this process, so that the prefix, the sidebar and every window work as
/// in a terminal. Output and input as with a pane; a text message
/// `{"cols":C,"rows":R}` resizes it.
fn attach(
    mut ws: tungstenite::WebSocket<TcpStream>,
    stream: TcpStream,
    request: &HttpRequest,
) -> std::io::Result<()> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use tungstenite::Message;
    let text = |value: serde_json::Value| Message::text(value.to_string());
    let session = request.query.get("session").cloned().unwrap_or_default();
    let host = request.query.get("host").filter(|h| !h.is_empty());
    let number = |key: &str, default: u16| {
        request
            .query
            .get(key)
            .and_then(|n| n.parse::<u16>().ok())
            .unwrap_or(default)
    };
    let size = PtySize {
        rows: number("rows", 24).clamp(5, 500),
        cols: number("cols", 80).clamp(20, 1000),
        pixel_width: 0,
        pixel_height: 0,
    };
    let opened = (|| -> Result<_> {
        match host {
            Some(host) => known_machine(host, &session)?,
            None if !protocol::valid_session_name(&session) => bail!("no session {session}"),
            None => {}
        }
        let pair = native_pty_system().openpty(size)?;
        let mut cmd = CommandBuilder::new(std::env::current_exe()?);
        if let Some(host) = host {
            cmd.args(["--host", host]);
        }
        cmd.args(["-s", &session]);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // The session asked for, not the one this process was started from.
        cmd.env_remove("IMKR_SOCKET");
        cmd.env_remove("IMKR_SESSION");
        if let Some(home) = std::env::var_os("HOME") {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader()?;
        // A copy of the master to type with: portable-pty's own writer types
        // Ctrl+D when dropped, which imkr would pass on to a pane.
        let fd = pair.master.as_raw_fd().context("no pty")?;
        // SAFETY: the master stays open while it is borrowed here.
        let writer = std::fs::File::from(
            unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?,
        );
        Ok((pair.master, child, reader, writer))
    })();
    let (master, mut child, mut reader, mut writer) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            let _ = ws.send(text(serde_json::json!({ "error": format!("{e:#}") })));
            let _ = ws.close(None);
            let _ = ws.flush();
            return Ok(());
        }
    };
    if ws
        .send(text(serde_json::json!({
            "rows": size.rows, "cols": size.cols, "fit": true,
        })))
        .is_err()
    {
        let _ = child.kill();
        return Ok(());
    }
    let device = request.device().map(|d| d.name).unwrap_or_default();
    let shown = match host {
        Some(host) => format!("{host}:{session}"),
        None => session.clone(),
    };
    note_seen(&device, &request.peer, Some(Some(shown)));
    let mut told = std::time::Instant::now();
    let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
    std::thread::Builder::new()
        .name("web-attach".into())
        .spawn(move || {
            let mut buf = [0u8; 16384];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(None);
                        break;
                    }
                    Ok(n) => {
                        if tx.send(Some(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;
    stream.set_read_timeout(Some(POLL))?;
    let ended = 'session: loop {
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if writer.write_all(&bytes).is_err() {
                    break false;
                }
            }
            Ok(Message::Text(message)) => {
                let size: serde_json::Value = serde_json::from_str(&message).unwrap_or_default();
                if let (Some(cols), Some(rows)) = (size["cols"].as_u64(), size["rows"].as_u64()) {
                    let _ = master.resize(PtySize {
                        rows: (rows as u16).clamp(5, 500),
                        cols: (cols as u16).clamp(20, 1000),
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
            Ok(Message::Close(_)) => break false,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break false,
        }
        let mut pending = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Some(bytes)) => pending.extend_from_slice(&bytes),
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                    if !pending.is_empty() {
                        let _ = ws.send(Message::binary(pending));
                    }
                    break 'session true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if !pending.is_empty() && ws.send(Message::binary(pending)).is_err() {
            break false;
        }
        if told.elapsed() >= STILL_THERE {
            told = std::time::Instant::now();
            note_seen(&device, &request.peer, None);
        }
    };
    note_seen(&device, &request.peer, Some(None));
    // The client goes, the session stays: as when a terminal is closed.
    let _ = child.kill();
    let _ = child.wait();
    if ended {
        let _ = ws.send(text(serde_json::json!({ "exited": true })));
    }
    let _ = ws.close(None);
    let _ = ws.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_firewall_is_pointed_out() {
        let addr: SocketAddr = "100.97.1.2:4657".parse().unwrap();
        let on = "ENABLED=yes\n";
        let other = "-A ufw-user-input -p tcp --dport 22 -j ACCEPT\n";
        let open = "-A ufw-user-input -i wt0 -p tcp --dport 4657 -j ACCEPT\n";
        let hint = ufw_hint(on, other, addr, Some("wt0")).unwrap();
        assert!(hint.contains("sudo ufw allow in on wt0 to any port 4657 proto tcp"));
        assert_eq!(ufw_hint(on, open, addr, Some("wt0")), None);
        assert_eq!(ufw_hint("ENABLED=no\n", other, addr, None), None);
        let local: SocketAddr = "127.0.0.1:4657".parse().unwrap();
        assert_eq!(ufw_hint(on, other, local, None), None);
    }

    #[test]
    fn queries_are_decoded() {
        let q = parse_query("session=my%20work&pane=3&code=ABCD-EFGH&x=a+b&bad=%zz&end=%4");
        assert_eq!(q["session"], "my work");
        assert_eq!(q["pane"], "3");
        assert_eq!(q["code"], "ABCD-EFGH");
        assert_eq!(q["x"], "a b");
        assert_eq!(q["bad"], "%zz");
        assert_eq!(q["end"], "%4");
    }

    #[test]
    fn qr_codes_are_square_and_framed() {
        let lines = qr_lines("http://100.97.56.232:4657/pair?code=ABCD-EFGH").unwrap();
        let cells = |line: &str| line.matches('▀').count();
        let width = cells(&lines[0]);
        // Two rows per line, and a light frame around the code.
        assert_eq!(lines.len(), width.div_ceil(2));
        assert!(lines.iter().all(|l| cells(l) == width));
        assert!(lines[0].starts_with("\x1b[97;107m▀"));
    }

    #[test]
    fn codes_match_loosely() {
        assert!(same_code("ABCD-EFGH", "abcdefgh"));
        assert!(same_code("ABCD-EFGH", " abcd efgh "));
        assert!(!same_code("ABCD-EFGH", "ABCD-EFGJ"));
    }

    #[test]
    fn listen_addresses_parse() {
        let addr = listen_addr(Some("100.97.1.2")).unwrap();
        assert_eq!(addr, SocketAddr::new(IpAddr::from([100, 97, 1, 2]), PORT));
        let addr = listen_addr(Some("0.0.0.0:8080")).unwrap();
        assert_eq!(addr.port(), 8080);
        assert!(listen_addr(Some("phone")).is_err());
    }

    #[test]
    fn only_own_pages_count() {
        let request = |origin: Option<&str>| HttpRequest {
            peer: String::new(),
            method: "POST".into(),
            path: "/api/send".into(),
            query: HashMap::new(),
            headers: [("host", Some("100.97.1.2:4657")), ("origin", origin)]
                .into_iter()
                .filter_map(|(k, v)| Some((k.to_owned(), v?.to_owned())))
                .collect(),
            body: Vec::new(),
        };
        assert!(request(Some("http://100.97.1.2:4657")).same_origin());
        assert!(request(Some("https://100.97.1.2:4657/")).same_origin());
        assert!(!request(Some("https://evil.example")).same_origin());
        assert!(request(None).same_origin());
    }
}
