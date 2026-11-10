//! Pairing machines with a code (#605): a new machine comes in with one
//! confirmed pairing, without anyone copying keys into `authorized_keys`.
//!
//! `imkr hosts pair` on a machine already in opens a listener on its VPN
//! address (or 127.0.0.1) for ten minutes and shows a code; `imkr hosts
//! join ADDRESS CODE` on the new one sends the code with its machine key
//! (`~/.ssh/imkr-<id>`, see `key_file`, made if missing) and its register. With the right code
//! both take the other's key and register, and each lets the other in. The
//! keys then travel with the register (`machines::Entry::key`), and every
//! machine that gets one lets that machine in too; `imkr hosts rm` leaves a
//! removal that takes the key out everywhere.
//!
//! Who takes keys at all: only a paired machine (`paired`, it has a key of
//! its own). One reached only by a login set up by hand, such as a server,
//! never stores the keys of others nor touches `authorized_keys`.
//!
//! Who may hand on keys: a register joined on the side that was asked
//! (`machines::tell_hosts`) came from a machine that just logged in here —
//! by hand-set key it has a shell anyway, by a key of ours it was paired.
//! A register joined on the asking side (`machines::learn_from`) loses its
//! keys: answering does not prove the machine may get in here.
//!
//! In `authorized_keys` imkr writes and removes only its own lines, those
//! with `imkr:<machine id>` after the key. Each runs `imkr gate <id>`
//! (`command=`, `restrict,pty`), which lets through only commands in the
//! form imkr itself sends over ssh (`client::remote_command_with`), never
//! a shell, scp or a forwarded port. It does not make a paired machine
//! harmless: `imkr send`, `run` and `new -- CMD` type into panes and start
//! programs, which is what imkr is for.

use crate::machines::{self, Entry};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::BufReader;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Where `imkr hosts pair` listens unless told otherwise.
pub const PORT: u16 = 4658;
/// How long a pairing code is good for, in seconds.
const PAIR_FOR: u64 = 600;
/// Wrong codes before the pairing ends.
const PAIR_TRIES: u32 = 5;
/// The word after the key that marks imkr's own lines.
const MARK: &str = "imkr:";
/// Set by `imkr gate` for the command it lets through: the paired machine
/// that logged in, as its key says, not as it tells.
pub const GATED_ENV: &str = "IMKR_GATED";
/// The file `authorized_keys` is copied to before imkr first changes it.
const BACKUP: &str = "authorized_keys.imkr-backup";

// --- The machine key ---

fn ssh_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".ssh"))
}

/// The machine key imkr pairs with: `~/.ssh/imkr-<id>`, one per machine
/// and made by imkr itself, so its private half was never anywhere else.
/// Not `~/.ssh/imkr`: that one may be the same on several machines (copied
/// by a dotfile manager), and then taking one machine's key out would take
/// none out. `~/.ssh/imkr` is still offered to ssh after this one, for the
/// lines set up by hand.
pub fn key_file() -> Option<PathBuf> {
    let id: String = crate::protocol::machine_id()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_".contains(*c))
        .take(12)
        .collect();
    Some(ssh_dir()?.join(format!("imkr-{id}")))
}

/// The keys ssh is offered for a machine of the user's, own one first;
/// those that exist.
pub fn offered_keys() -> Vec<PathBuf> {
    [key_file(), ssh_dir().map(|d| d.join("imkr"))]
        .into_iter()
        .flatten()
        .filter(|key| key.is_file())
        .collect()
}

/// This machine's key as others accept it, `TYPE BASE64` without the
/// comment; `None` while it has none (`imkr hosts key` or a pairing makes
/// it).
pub fn own_key() -> Option<String> {
    let mut file = key_file()?.into_os_string();
    file.push(".pub");
    public_key(&std::fs::read_to_string(file).ok()?)
}

/// Whether this machine is paired: it has its own key, from `hosts pair`,
/// `join` or `key`. Only then does it keep the keys of other machines and
/// write `authorized_keys`; a machine imkr only reaches by a login set up
/// by hand leaves the file alone, not even a backup.
pub fn paired() -> bool {
    own_key().is_some()
}

/// The machine key, made (ed25519, `imkr@NAME`) when there is none yet.
pub fn ensure_key() -> Result<String> {
    if let Some(key) = own_key() {
        return Ok(key);
    }
    let file = key_file().context("HOME is not set")?;
    create_ssh_dir(file.parent().context("no directory")?)?;
    if file.exists() {
        bail!(
            "{} is there but its .pub is not: `ssh-keygen -y -f {0} > {0}.pub` makes it",
            file.display(),
        );
    }
    let comment = format!("imkr@{}", crate::protocol::machine_name());
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
        .arg(&file)
        .stdin(Stdio::null())
        .status()
        .context("could not run ssh-keygen")?;
    if !status.success() {
        bail!("ssh-keygen could not make {}", file.display());
    }
    println!("Made this machine's own key {}.", file.display());
    own_key().context("ssh-keygen made no public key")
}

/// `imkr hosts key`: makes the machine key if missing and puts it into
/// the register, from where it reaches the other machines.
pub fn key_command() -> Result<()> {
    let key = ensure_key()?;
    machines::register_to_send();
    let file = key_file().context("HOME is not set")?;
    println!("{} is this machine's key:", file.display());
    println!("{key}");
    println!(
        "It goes with the register to your other machines; each lets this one in with it \
         once they talk (`imkr hosts` says \"paired\")."
    );
    Ok(())
}

fn create_ssh_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The key types ssh takes in `authorized_keys`.
fn is_key_type(word: &str) -> bool {
    word.starts_with("ssh-")
        || word.starts_with("ecdsa-sha2-")
        || word.starts_with("sk-ssh-")
        || word.starts_with("sk-ecdsa-")
}

/// `TYPE BASE64` from a public key line as `ssh-keygen` writes it, or
/// `None` if it is not one: what other machines send is checked here
/// before it goes near `authorized_keys`.
pub fn public_key(text: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    let kind = words.next()?;
    let blob = words.next()?;
    let base64 = |c: char| c.is_ascii_alphanumeric() || "+/=".contains(c);
    (is_key_type(kind)
        && kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-@.".contains(c))
        && blob.len() >= 16
        && blob.chars().all(base64))
    .then(|| format!("{kind} {blob}"))
}

// --- authorized_keys ---

/// The file imkr changes; `IMKR_AUTHORIZED_KEYS` for tests. Unit tests
/// never get the real one.
fn authorized_keys() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IMKR_AUTHORIZED_KEYS") {
        return Some(PathBuf::from(path));
    }
    if cfg!(test) {
        return None;
    }
    Some(ssh_dir()?.join("authorized_keys"))
}

/// A line of `authorized_keys`, as far as imkr needs it.
struct Line<'a> {
    /// `TYPE BASE64`.
    key: String,
    /// The machine id of imkr's own lines.
    managed: Option<&'a str>,
}

/// Reads one line: the options (which may hold quoted spaces), the key,
/// the comment. `None` for empty lines, comments and what is no key.
fn parse_line(line: &str) -> Option<Line<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Skip the options: up to the first space outside quotes.
    let rest = if line.split_whitespace().next().is_some_and(is_key_type) {
        line
    } else {
        let mut quoted = false;
        let mut end = line.len();
        let mut escaped = false;
        for (i, c) in line.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = !quoted,
                ' ' | '\t' if !quoted => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        line[end..].trim_start()
    };
    let mut words = rest.split_whitespace();
    let kind = words.next().filter(|k| is_key_type(k))?;
    let blob = words.next()?;
    let managed = words
        .find_map(|w| w.strip_prefix(MARK))
        .filter(|id| !id.is_empty());
    Some(Line {
        key: format!("{kind} {blob}"),
        managed,
    })
}

/// The line imkr writes for the machine `id` with `key`.
fn managed_line(gate: &str, id: &str, key: &str, name: &str) -> String {
    let gate = gate.replace('\\', "\\\\").replace('"', "\\\"");
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
        .collect();
    format!("restrict,pty,command=\"{gate} gate {id}\" {key} {MARK}{id} {name}")
        .trim_end()
        .to_owned()
}

/// What `apply` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Machines whose key is in the file by hand already: left alone.
    pub by_hand: Vec<String>,
}

/// One machine to let in: id, name, key.
pub struct Wanted<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub key: &'a str,
}

/// `text` (an `authorized_keys`) with imkr's lines for `wanted` and none
/// for `removed`; every other line stays byte for byte. A key already in
/// a line of someone else's is not added again: ssh takes the first line
/// with a key, so imkr's restricted one would change nothing.
pub fn apply(text: &str, gate: &str, wanted: &[Wanted], removed: &[&str]) -> (String, Changes) {
    let mut changes = Changes::default();
    let mut out: Vec<String> = Vec::new();
    let mut placed: Vec<&str> = Vec::new();
    let others: Vec<String> = text
        .lines()
        .filter_map(parse_line)
        .filter(|l| l.managed.is_none())
        .map(|l| l.key)
        .collect();
    for line in text.split_inclusive('\n') {
        let Some(id) = parse_line(line).and_then(|l| l.managed.map(str::to_owned)) else {
            out.push(line.to_owned());
            continue;
        };
        let want = wanted
            .iter()
            .find(|w| w.id == id)
            .filter(|w| !others.contains(&w.key.to_owned()));
        match want {
            Some(w) if !removed.contains(&w.id) && !placed.contains(&w.id) => {
                placed.push(w.id);
                let new = managed_line(gate, w.id, w.key, w.name);
                if line.trim_end() == new {
                    out.push(line.to_owned());
                } else {
                    changes.added.push(w.name.to_owned());
                    out.push(format!("{new}\n"));
                }
            }
            // Wanted with a key someone wrote by hand, taken out, or a
            // second line of the same machine.
            Some(w) => changes.removed.push(w.name.to_owned()),
            None if removed.contains(&id.as_str()) || wanted.iter().any(|w| w.id == id) => {
                changes.removed.push(id.clone())
            }
            // A machine imkr does not know of now: not imkr's to judge.
            None => out.push(line.to_owned()),
        }
    }
    for w in wanted {
        if placed.contains(&w.id) || removed.contains(&w.id) {
            continue;
        }
        if others.contains(&w.key.to_owned()) {
            changes.by_hand.push(w.name.to_owned());
            continue;
        }
        if out.last().is_some_and(|l| !l.ends_with('\n')) {
            out.push("\n".to_owned());
        }
        out.push(format!("{}\n", managed_line(gate, w.id, w.key, w.name)));
        changes.added.push(w.name.to_owned());
    }
    (out.concat(), changes)
}

/// The command `command=` runs: imkr where the login shell finds it, so
/// it stays right across `imkr update`.
fn gate_program() -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for dir in [".cargo/bin", ".local/bin"] {
        if let Some(path) = home.as_ref().map(|h| h.join(dir).join("imkr"))
            && path.is_file()
        {
            return path.display().to_string();
        }
    }
    std::env::current_exe()
        .map(|p| {
            p.display()
                .to_string()
                .trim_end_matches(" (deleted)")
                .to_owned()
        })
        .unwrap_or_else(|_| "imkr".to_owned())
}

/// Who `authorized_keys` is to let in, and whose lines go: taken out,
/// or with a key that more than one machine says is theirs (a copied
/// key; removing one of them would leave it in for the others).
fn to_let_in<'a>(entries: &'a [Entry], own: &str) -> (Vec<Wanted<'a>>, Vec<&'a str>) {
    let live = || {
        entries
            .iter()
            .filter(|e| !e.removed && !e.id.is_empty() && e.id != own && !e.key.is_empty())
    };
    let shared = |e: &Entry| {
        entries
            .iter()
            .any(|o| o.id != e.id && !o.id.is_empty() && o.key == e.key)
    };
    let wanted = live()
        .filter(|e| !shared(e))
        .map(|e| Wanted {
            id: &e.id,
            name: &e.name,
            key: &e.key,
        })
        .collect();
    let removed = entries
        .iter()
        .filter(|e| !e.id.is_empty() && (e.removed || (!e.key.is_empty() && shared(e))))
        .map(|e| e.id.as_str())
        .collect();
    (wanted, removed)
}

/// Makes `authorized_keys` let in the machines of `entries` that have a
/// key and are not taken out, and no longer the ones taken out; called
/// whenever the register changed. Copies the file away before the first
/// change, writes it whole through a file beside it, 0600.
pub fn sync_keys(entries: &[Entry]) -> Changes {
    let (wanted, removed) = to_let_in(entries, &crate::protocol::machine_id());
    let Some(path) = authorized_keys() else {
        return Changes::default();
    };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if wanted.is_empty() && !text.contains(MARK) {
        return Changes::default();
    }
    let (new, changes) = apply(&text, &gate_program(), &wanted, &removed);
    if new != text
        && let Err(e) = write_keys(&path, &text, &new)
    {
        eprintln!("imkr: could not change {}: {e:#}", path.display());
        return Changes::default();
    }
    changes
}

fn write_keys(path: &std::path::Path, old: &str, new: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().context("no directory")?;
    create_ssh_dir(dir)?;
    let backup = dir.join(BACKUP);
    if path.exists() && !backup.exists() {
        crate::web::write_private(&backup, old.as_bytes())?;
    }
    let part = dir.join("authorized_keys.imkr-part");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&part)?;
    file.write_all(new.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(part, path)?;
    Ok(())
}

/// How `key` of the machine `id` is let in here: by imkr (paired), by a
/// line someone wrote, or not.
#[derive(Debug, PartialEq, Eq)]
pub enum Access {
    Paired,
    ByHand,
    None,
}

/// Access of every machine id with an imkr line, and the keys in other
/// lines, read once for `imkr hosts`.
pub fn access() -> impl Fn(&str, &str) -> Access {
    let text = authorized_keys()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let lines: Vec<(String, Option<String>)> = text
        .lines()
        .filter_map(parse_line)
        .map(|l| (l.key, l.managed.map(str::to_owned)))
        .collect();
    move |id: &str, key: &str| {
        if !id.is_empty() && lines.iter().any(|(_, m)| m.as_deref() == Some(id)) {
            Access::Paired
        } else if !key.is_empty() && lines.iter().any(|(k, m)| m.is_none() && k == key) {
            Access::ByHand
        } else {
            Access::None
        }
    }
}

// --- The gate ---

/// Subcommands a paired machine may not run here: who gets in, logins.
fn refused(args: &[String]) -> Option<&'static str> {
    let mut words = args.iter().map(String::as_str);
    let mut first = words.next()?;
    if first == "-s" {
        words.next()?;
        first = words.next()?;
    }
    let second = words.next().unwrap_or("");
    match (first, second) {
        ("gate", _) => Some("the gate"),
        ("hosts", "add") => None,
        ("hosts", _) => Some("who gets in"),
        ("account", _) => Some("logins"),
        ("connect", _) => Some("contacts"),
        ("web", "pair" | "devices" | "forget") => Some("devices"),
        ("screen", "allow" | "refuse") => Some("the screen permission"),
        _ => None,
    }
}

/// Reads one word as `client::sh_quote` writes it: plain characters and
/// `'…'` with `'\''` for a quote. The word and what follows the space
/// after it; `None` for anything else, `$`, `;` or backquotes included.
fn sh_word(text: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let mut rest = text;
    loop {
        if let Some(after) = rest.strip_prefix('\'') {
            let end = after.find('\'')?;
            word.push_str(&after[..end]);
            rest = &after[end + 1..];
        } else if let Some(after) = rest.strip_prefix("\\'") {
            word.push('\'');
            rest = after;
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)))
                .unwrap_or(rest.len());
            if end == 0 {
                break;
            }
            word.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    if rest.is_empty() {
        return Some((word, rest));
    }
    rest.strip_prefix(' ').map(|rest| (word, rest))
}

/// Variables set on an imkr command, by name.
type Vars = Vec<(String, String)>;

/// What a machine asked to run over ssh, if imkr itself would have sent
/// it: `client::remote_command_with` of known variables and an imkr
/// command that does not change who gets in. Checked by building it again,
/// so nothing can ride along.
pub fn allowed(command: &str) -> Result<(), String> {
    if command == crate::client::INSTALLED_CHECK {
        return Ok(());
    }
    let parse = || -> Option<(Vars, Vec<String>)> {
        let outer = command.strip_prefix("exec \"${SHELL:-sh}\" -lc ")?;
        let (inner, rest) = sh_word(outer)?;
        if !rest.is_empty() {
            return None;
        }
        let mut inner = inner.strip_prefix("PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH ")?;
        let mut env = Vec::new();
        while !inner.starts_with("exec imkr ") {
            let (key, value) = inner.split_once('=')?;
            if ![machines::HOSTS_ENV, machines::CALLER_ENV].contains(&key) {
                return None;
            }
            let (value, rest) = sh_word(value)?;
            env.push((key.to_owned(), value));
            inner = rest;
        }
        let mut rest = inner.strip_prefix("exec imkr ")?;
        let mut args = Vec::new();
        while !rest.is_empty() {
            let (word, after) = sh_word(rest)?;
            args.push(word);
            rest = after;
        }
        Some((env, args))
    };
    let Some((env, args)) = parse() else {
        return Err("this key is only for imkr's own commands".into());
    };
    let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if crate::client::remote_command_with(&vars, &words) != command {
        return Err("this key is only for imkr's own commands".into());
    }
    if let Some(what) = refused(&args) {
        return Err(format!(
            "a paired machine does not change {what} here: `imkr {}` needs a login of your own",
            args.join(" ")
        ));
    }
    Ok(())
}

/// `imkr gate ID`, what imkr's lines in `authorized_keys` run: the command
/// the machine asked for if `allowed`, else a refusal on stderr.
pub fn gate(args: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let [id] = args else {
        bail!("usage: imkr gate MACHINE-ID (for authorized_keys)");
    };
    let Ok(command) = std::env::var("SSH_ORIGINAL_COMMAND") else {
        bail!("this key is only for imkr, not for a shell: use your own login");
    };
    if let Err(why) = allowed(&command) {
        bail!("{why}");
    }
    let error = Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .env(GATED_ENV, id)
        .exec();
    bail!("could not run sh: {error}")
}

// --- Pairing ---

/// What both sides send each other.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Hello {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    code: String,
    name: String,
    id: String,
    #[serde(default)]
    user: String,
    key: String,
    #[serde(default)]
    register: Vec<Entry>,
    /// The ssh host keys, `TYPE BASE64`, so the first ssh does not have
    /// to take one unseen.
    #[serde(default)]
    host_keys: Vec<String>,
}

fn hello(code: &str) -> Result<Hello> {
    Ok(Hello {
        code: code.to_owned(),
        name: crate::protocol::machine_name(),
        id: crate::protocol::machine_id(),
        user: std::env::var("USER").unwrap_or_default(),
        key: ensure_key()?,
        register: machines::register(),
        host_keys: host_keys(),
    })
}

/// This machine's ssh host keys.
fn host_keys() -> Vec<String> {
    let mut keys: Vec<String> = std::fs::read_dir("/etc/ssh")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("ssh_host_") && name.ends_with("_key.pub")
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| public_key(&text))
        .collect();
    keys.sort();
    keys
}

/// Checks what the other side sent before any of it is taken.
fn checked(hello: &Hello) -> Result<String, String> {
    let name: String = hello.name.trim().to_owned();
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if name.is_empty() || name.len() > 64 || !plain(&name) {
        return Err("bad name".into());
    }
    if hello.id.is_empty() || hello.id.len() > 64 || !plain(&hello.id) {
        return Err("bad machine id".into());
    }
    if hello.id == crate::protocol::machine_id() {
        return Err("that is this machine".into());
    }
    if !hello
        .user
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err("bad user".into());
    }
    if public_key(&hello.key).as_deref() != Some(hello.key.as_str()) {
        return Err("bad key".into());
    }
    // Two machines may call themselves the same (two `omarchy`s).
    let taken = machines::host_ids()
        .into_iter()
        .any(|(host, h)| host == name && !h.id.is_empty() && h.id != hello.id);
    if taken {
        return Err(format!(
            "another machine is known as {name} already: give one of them another \
             `name` in imkr's config first"
        ));
    }
    Ok(name)
}

/// The address to listen on: asked, the VPN address, or 127.0.0.1; only
/// those, so no one outside the own network sees the code being asked for.
fn listen_addr(asked: Option<&str>) -> Result<SocketAddr> {
    let addr = match asked {
        Some(text) => text
            .parse::<SocketAddr>()
            .or_else(|_| text.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, PORT)))
            .with_context(|| format!("{text} is no address to listen on"))?,
        None => SocketAddr::new(
            crate::web::vpn_address().unwrap_or(IpAddr::from([127, 0, 0, 1])),
            PORT,
        ),
    };
    if !addr.ip().is_loopback() && !machines::in_vpn(&addr.ip().to_string()) {
        bail!(
            "{} is not on your VPN (100.64.0.0/10) nor 127.0.0.1",
            addr.ip()
        );
    }
    Ok(addr)
}

/// `imkr hosts pair [--listen ADDR[:PORT]]`: waits ten minutes for one
/// machine to join with the code shown.
pub fn pair(args: &[String]) -> Result<()> {
    let asked = match args {
        [] => None,
        [flag, addr] if flag == "--listen" => Some(addr.as_str()),
        _ => bail!("usage: imkr hosts pair [--listen ADDR[:PORT]]"),
    };
    let addr = listen_addr(asked)?;
    ensure_key()?;
    let listener = TcpListener::bind(addr).with_context(|| format!("cannot listen on {addr}"))?;
    listener.set_nonblocking(true)?;
    let code = crate::web::random_code()?;
    let until = crate::web::now() + PAIR_FOR;
    let shown = if addr.port() == PORT {
        addr.ip().to_string()
    } else {
        addr.to_string()
    };
    println!("Pairing code {code}, for 10 minutes. On the new machine:");
    println!();
    println!("  imkr hosts join {shown} {code}");
    println!();
    println!("Ctrl+C ends it.");
    let mut tries = 0;
    while crate::web::now() < until {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let peer = stream.peer_addr()?.ip();
        let mut reader = BufReader::new(stream.try_clone()?);
        let Ok(Some(request)) = crate::web::read_request(&mut reader) else {
            continue;
        };
        let mut stream = stream;
        let (response, done) = answer(&request, &code, peer, &mut tries);
        let _ = response.write_to(&mut stream);
        if done {
            return Ok(());
        }
        if tries >= PAIR_TRIES {
            bail!("{PAIR_TRIES} wrong codes: the pairing ended, `imkr hosts pair` opens a new one");
        }
    }
    bail!("no machine joined in 10 minutes")
}

/// One request to the pairing listener, and whether pairing is done.
fn answer(
    request: &crate::web::HttpRequest,
    code: &str,
    peer: IpAddr,
    tries: &mut u32,
) -> (crate::web::Response, bool) {
    use crate::web::Response;
    // Other imkrs only: browsers send `Origin`.
    if request.method != "POST"
        || request.path != "/machine/join"
        || request.header("origin").is_some()
        || request.header("content-type") != Some("application/json")
    {
        return (
            Response::error("404 Not Found", "only for imkr hosts join"),
            false,
        );
    }
    let Ok(hello) = serde_json::from_slice::<Hello>(&request.body) else {
        return (Response::error("400 Bad Request", "bad request"), false);
    };
    if !crate::web::same_code(code, &hello.code) {
        *tries += 1;
        eprintln!("A wrong code came from {peer}.");
        return (Response::error("403 Forbidden", "wrong code"), false);
    }
    let name = match checked(&hello) {
        Ok(name) => name,
        Err(why) => return (Response::error("400 Bad Request", &why), false),
    };
    let mine = match self::hello("") {
        Ok(mine) => mine,
        Err(e) => {
            return (
                Response::error("500 Internal Server Error", &e.to_string()),
                false,
            );
        }
    };
    let changes = take(&name, &hello, &peer.to_string());
    println!("Paired with {name} at {peer}.");
    tell(&changes);
    (
        Response::json(&serde_json::to_value(&mine).unwrap_or_default()),
        true,
    )
}

/// `imkr hosts join ADDRESS[:PORT] CODE` on the new machine.
pub fn join(args: &[String]) -> Result<()> {
    let [address, code] = args else {
        bail!("usage: imkr hosts join ADDRESS[:PORT] CODE");
    };
    let addr = address
        .parse::<SocketAddr>()
        .or_else(|_| {
            address
                .parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, PORT))
        })
        .with_context(|| format!("{address}: say the address `imkr hosts pair` showed"))?;
    if !addr.ip().is_loopback() && !machines::in_vpn(&addr.ip().to_string()) {
        bail!(
            "{} is not on your VPN (100.64.0.0/10): imkr pairs only there",
            addr.ip()
        );
    }
    let mine = hello(code)?;
    let answer = crate::contacts::post(
        &format!("http://{addr}/"),
        "/machine/join",
        None,
        &serde_json::to_value(&mine)?,
    )?;
    let theirs: Hello = serde_json::from_value(answer).context("an answer that is no pairing")?;
    let name = checked(&theirs).map_err(|why| anyhow::anyhow!("{addr} answered: {why}"))?;
    let changes = take(&name, &theirs, &addr.ip().to_string());
    println!("Paired with {name} at {}.", addr.ip());
    tell(&changes);
    println!(
        "The other machines of {name} learn of this one within a minute or two, once they \
         ask {name} for its sessions."
    );
    Ok(())
}

/// Takes the other side of a pairing: its register (it showed the code,
/// so its keys too) and itself, as a host here at `address`.
fn take(name: &str, hello: &Hello, address: &str) -> Changes {
    let entry = Entry {
        name: name.to_owned(),
        id: hello.id.clone(),
        address: address.to_owned(),
        user: hello.user.clone(),
        at: crate::persist::now(),
        removed: false,
        key: hello.key.clone(),
    };
    let host_keys: Vec<String> = hello
        .host_keys
        .iter()
        .filter_map(|k| public_key(k))
        .collect();
    machines::take_paired(entry, &hello.register, &host_keys)
}

fn tell(changes: &Changes) {
    if !changes.added.is_empty() {
        println!("Lets in now: {}.", changes.added.join(", "));
    }
    if !changes.by_hand.is_empty() {
        println!(
            "Already let in by a line of authorized_keys someone wrote, left as is: {}.",
            changes.by_hand.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn want<'a>(id: &'a str, name: &'a str, key: &'a str) -> Wanted<'a> {
        Wanted { id, name, key }
    }

    #[test]
    fn lines_of_others_stay_byte_for_byte() {
        let text = "# mine\r\nssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAB bastian@desk\n\n\
                    from=\"10.0.0.1\",command=\"echo a b\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5XYZ x";
        let (out, changes) = apply(text, "/bin/imkr", &[want("aa11", "laptop", KEY_A)], &[]);
        assert!(out.starts_with(text), "{out:?}");
        assert_eq!(
            &out[text.len()..],
            format!("\nrestrict,pty,command=\"/bin/imkr gate aa11\" {KEY_A} imkr:aa11 laptop\n")
        );
        assert_eq!(changes.added, ["laptop"]);
        // Again: nothing changes.
        let (again, changes) = apply(&out, "/bin/imkr", &[want("aa11", "laptop", KEY_A)], &[]);
        assert_eq!(again, out);
        assert_eq!(changes, Changes::default());
    }

    #[test]
    fn a_removal_takes_only_imkrs_line() {
        let hand = format!("{KEY_A} imkr@laptop\n");
        let text =
            format!("{hand}restrict,pty,command=\"/bin/imkr gate bb22\" {KEY_B} imkr:bb22 pi\n");
        let (out, changes) = apply(&text, "/bin/imkr", &[], &["bb22", "aa11"]);
        assert_eq!(out, hand);
        assert_eq!(changes.removed, ["bb22"]);
    }

    #[test]
    fn a_key_written_by_hand_is_not_added_again() {
        let text = format!("{KEY_A} imkr@laptop\n");
        let (out, changes) = apply(&text, "/bin/imkr", &[want("aa11", "laptop", KEY_A)], &[]);
        assert_eq!(out, text);
        assert_eq!(changes.by_hand, ["laptop"]);
    }

    #[test]
    fn a_new_key_of_a_machine_replaces_its_line() {
        let text =
            format!("restrict,pty,command=\"/bin/imkr gate aa11\" {KEY_A} imkr:aa11 laptop\n");
        let (out, _) = apply(&text, "/bin/imkr", &[want("aa11", "laptop", KEY_B)], &[]);
        assert_eq!(
            out,
            format!("restrict,pty,command=\"/bin/imkr gate aa11\" {KEY_B} imkr:aa11 laptop\n")
        );
    }

    #[test]
    fn only_key_lines_are_taken_from_others() {
        assert_eq!(
            public_key(&format!("{KEY_A} imkr@x\n")).as_deref(),
            Some(KEY_A)
        );
        assert!(public_key("ssh-ed25519 AAAA;rm -rf ~").is_none());
        assert!(public_key("command=\"sh\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA").is_none());
        assert!(public_key("ssh-ed25519\nssh-rsa").is_none());
    }

    #[test]
    fn the_gate_lets_through_what_imkr_sends() {
        use crate::client::{remote_command, remote_command_with};
        for args in [
            vec!["ls", "--json"],
            vec!["proxy", "--start"],
            vec!["-s", "api", "attach-pane", "3"],
            vec!["msg", "--from", "the user at x", "it's"],
            vec!["hosts", "add", "laptop"],
            vec!["upgrade"],
        ] {
            assert_eq!(allowed(&remote_command(&args)), Ok(()), "{args:?}");
        }
        let register = r#"[{"name":"a b","at":1,"key":"ssh-ed25519 AAAA"}]"#;
        let line = remote_command_with(
            &[
                (machines::HOSTS_ENV, register),
                (machines::CALLER_ENV, "abc"),
            ],
            &["ls", "--json"],
        );
        assert_eq!(allowed(&line), Ok(()));
        assert_eq!(allowed(crate::client::INSTALLED_CHECK), Ok(()));
    }

    #[test]
    fn the_gate_stops_everything_else() {
        use crate::client::remote_command;
        let ls = remote_command(&["ls"]);
        for command in [
            "sh".to_owned(),
            "bash -i".to_owned(),
            "scp -t /tmp".to_owned(),
            "internal-sftp".to_owned(),
            format!("{ls}; rm -rf ~"),
            format!("{ls} && id"),
            ls.replace("exec imkr ls", "exec imkr ls $(id)"),
            ls.replace("exec imkr", "exec sh -c id; imkr"),
            "exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH \
             FOO=1 exec imkr ls'"
                .to_owned(),
            "exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH \
             exec imkr ls `id`'"
                .to_owned(),
        ] {
            assert!(allowed(&command).is_err(), "{command}");
        }
        for args in [
            vec!["gate", "x"],
            vec!["hosts", "rm", "laptop"],
            vec!["hosts", "pair"],
            vec!["-s", "x", "account", "login", "claude:a"],
            vec!["web", "pair"],
            vec!["screen", "allow"],
        ] {
            assert!(allowed(&remote_command(&args)).is_err(), "{args:?}");
        }
    }

    #[test]
    fn a_key_two_machines_say_is_theirs_lets_in_neither() {
        let entry = |id: &str, key: &str| Entry {
            name: id.into(),
            id: id.into(),
            key: key.into(),
            at: 1,
            ..Entry::default()
        };
        let entries = vec![
            entry("desktop", KEY_A),
            entry("laptop", KEY_A),
            entry("pi", KEY_B),
            entry(
                "me",
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAImeeeeeeeeeeeeeee",
            ),
        ];
        let (wanted, removed) = to_let_in(&entries, "me");
        let ids: Vec<&str> = wanted.iter().map(|w| w.id).collect();
        assert_eq!(ids, ["pi"]);
        assert_eq!(removed, ["desktop", "laptop"]);
    }

    #[test]
    fn unit_tests_never_touch_the_real_authorized_keys() {
        if std::env::var_os("IMKR_AUTHORIZED_KEYS").is_none() {
            assert_eq!(authorized_keys(), None);
        }
    }
}
