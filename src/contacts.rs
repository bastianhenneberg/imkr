//! Contacts: other people's imkrs that may send messages here. Paired once
//! with a code, both ways at the same time, over the web server
//! (`imkr web`) in a VPN such as NetBird; no ssh between people. A message
//! from a contact waits here until it is accepted, unless the contact is
//! trusted.
//!
//! The other way needs no common network: through AI Brain, which knows the
//! people and who may write to whom. imkr does not sign in there itself, it
//! goes through `ai-brain-harness nachricht` (kontakte, senden, abholen).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::protocol::{self, Request};
use crate::web;

/// How long an invitation code is good for, in seconds.
const INVITE_FOR: u64 = 600;
/// Wrong codes before an invitation is gone.
const INVITE_TRIES: u32 = 5;
/// The longest message taken, in bytes.
pub const MAX_TEXT: usize = 64 * 1024;
/// The longest message AI Brain carries, in bytes.
const MAX_BRAIN_TEXT: usize = 8 * 1024;
/// How many message ids from AI Brain are remembered against doubles: it
/// hands a message out again until it is confirmed.
const BRAIN_SEEN: usize = 500;
/// Messages kept waiting at most, the oldest go first.
const MAX_WAITING: usize = 200;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// What this side calls them, the machine part of an address.
    pub name: String,
    /// Their imkr web server, `http://100.97.1.2:4657/`; empty through AI
    /// Brain.
    #[serde(default)]
    pub url: String,
    /// What we show them when sending.
    #[serde(default)]
    token: String,
    /// What they show us when sending here.
    #[serde(default)]
    key: String,
    /// Their name in AI Brain, when messages go through it instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brain: Option<String>,
    /// Their messages are typed right away, without asking.
    #[serde(default)]
    pub trusted: bool,
    /// Seconds since 1970.
    pub since: u64,
}

/// A message from a contact, waiting to be accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incoming {
    pub id: u64,
    /// The contact's name.
    pub contact: String,
    /// Where to answer, as an address here: `claude@martin:api/0`.
    pub from: String,
    /// `session/pane` or `pane` on this machine.
    pub to: String,
    pub text: String,
    pub at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Invite {
    code: String,
    until: u64,
    tries: u32,
}

fn path(name: &str) -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join(name))
}

fn read<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    path(name)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write<T: Serialize>(name: &str, value: &T) -> Result<()> {
    web::write_private(&path(name)?, &serde_json::to_vec_pretty(value)?)
}

/// Runs `f` while holding the lock on the contact files: the web server
/// answers on many threads, and the CLI changes them too.
fn locked<T>(f: impl FnOnce() -> T) -> Result<T> {
    use std::os::fd::AsRawFd;
    let lock = path("contacts.lock")?;
    if let Some(dir) = lock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)?;
    // SAFETY: flock on a descriptor we own; released when `file` closes.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("cannot lock {}", lock.display());
    }
    Ok(f())
}

pub fn load() -> Vec<Contact> {
    read("contacts.json")
}

fn save(contacts: &[Contact]) -> Result<()> {
    write("contacts.json", &contacts)
}

pub fn find(name: &str) -> Option<Contact> {
    load().into_iter().find(|c| c.name == name)
}

pub fn waiting() -> Vec<Incoming> {
    read("contact-inbox.json")
}

/// Who we are to the other side: `--as`, else the login name.
pub fn own_name() -> String {
    std::env::var("USER")
        .ok()
        .and_then(|u| clean_name(&u))
        .unwrap_or_else(|| "someone".into())
}

/// A name usable in an address: letters, digits, `-`, `_` and `.`.
fn clean_name(name: &str) -> Option<String> {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(40)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// `name`, or `name-2`, … when a contact has it already.
fn free_name(contacts: &[Contact], name: &str) -> String {
    let mut free = name.to_owned();
    let mut n = 2;
    while contacts.iter().any(|c| c.name == free) {
        free = format!("{name}-{n}");
        n += 1;
    }
    free
}

/// This machine's web server, as contacts reach it; it must run, and on an
/// address others can reach.
fn own_url() -> Result<String> {
    let config = crate::config::Config::load().0;
    if !config.web {
        bail!(
            "contacts reach you through imkr's web server: turn on \"Web view for \
             phones\" in the settings or set `web = true` in the config"
        );
    }
    let addr = web::listen_addr(None)?;
    if addr.ip().is_loopback() {
        bail!(
            "imkr's web server only listens on {addr}, others cannot reach it: join a VPN such \
             as NetBird or Tailscale, or set `web_listen`"
        );
    }
    if addr.ip().is_unspecified() {
        bail!("set `web_listen` to the address others reach you at, not {addr}");
    }
    web::keep_running();
    Ok(web::url(addr))
}

/// Opens an invitation: the code the other side joins with.
pub fn invite() -> Result<(String, String)> {
    let url = own_url()?;
    let code = web::random_code()?;
    locked(|| {
        write(
            "contact-invite.json",
            &Invite {
                code: code.clone(),
                until: web::now() + INVITE_FOR,
                tries: 0,
            },
        )
    })??;
    Ok((code, url))
}

#[derive(Debug, Serialize, Deserialize)]
struct Join {
    code: String,
    /// What the joining side is called.
    name: String,
    url: String,
    /// What we show them when sending there.
    token: String,
}

/// Joins the invitation `code` of the imkr at `url`; both sides are
/// contacts of each other afterwards. `alias` names them here.
pub fn join(url: &str, code: &str, own: &str, alias: Option<&str>) -> Result<Contact> {
    let url = normal_url(url)?;
    let key = web::random_token()?;
    let answer = post(
        &url,
        "/contact/join",
        None,
        &serde_json::to_value(Join {
            code: code.to_owned(),
            name: own.to_owned(),
            url: own_url()?,
            token: key.clone(),
        })?,
    )?;
    let (Some(name), Some(token)) = (answer["name"].as_str(), answer["token"].as_str()) else {
        bail!("{url} answered without a name and token");
    };
    let wanted = alias
        .and_then(clean_name)
        .or_else(|| clean_name(name))
        .unwrap_or_else(|| "contact".into());
    locked(|| {
        let mut contacts = load();
        let contact = Contact {
            name: free_name(&contacts, &wanted),
            url,
            token: token.to_owned(),
            key,
            brain: None,
            trusted: false,
            since: web::now(),
        };
        contacts.push(contact.clone());
        save(&contacts).map(|()| contact)
    })?
}

/// `http://host:port/` from what someone typed, `host:port` included.
fn normal_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url.strip_prefix("http://").unwrap_or(url);
    if rest.is_empty() || rest.contains('/') || rest.contains("://") {
        bail!("{url}: say the address as http://HOST:PORT, as `imkr connect invite` printed it");
    }
    let rest = if rest.contains(':') {
        rest.to_owned()
    } else {
        format!("{rest}:{}", web::PORT)
    };
    Ok(format!("http://{rest}/"))
}

/// The web server's side of `join`: a new contact if the code is right.
pub fn accept_join(body: &[u8]) -> Result<serde_json::Value, String> {
    let join: Join = serde_json::from_slice(body).map_err(|e| format!("bad request: {e}"))?;
    let url = normal_url(&join.url).map_err(|e| e.to_string())?;
    if join.token.len() < 32 {
        return Err("bad request: token".into());
    }
    let inner = || -> Result<serde_json::Value, String> {
        let mut invite: Invite = read::<Option<Invite>>("contact-invite.json")
            .filter(|i| i.until > web::now())
            .ok_or("no invitation is open: run `imkr connect invite` there")?;
        let invite_path = path("contact-invite.json").map_err(|e| e.to_string())?;
        if !web::same_code(&invite.code, &join.code) {
            invite.tries += 1;
            if invite.tries >= INVITE_TRIES {
                let _ = std::fs::remove_file(&invite_path);
            } else {
                let _ = write("contact-invite.json", &invite);
            }
            return Err("wrong code".into());
        }
        let _ = std::fs::remove_file(&invite_path);
        let key = web::random_token().map_err(|e| e.to_string())?;
        let mut contacts = load();
        let name = free_name(
            &contacts,
            &clean_name(&join.name).unwrap_or_else(|| "contact".into()),
        );
        contacts.push(Contact {
            name: name.clone(),
            url,
            token: join.token.clone(),
            key: key.clone(),
            brain: None,
            trusted: false,
            since: web::now(),
        });
        save(&contacts).map_err(|e| e.to_string())?;
        notify(&format!(
            "{name} is a contact now: `imkr connect` lists them"
        ));
        Ok(serde_json::json!({ "name": own_name(), "token": key, "as": name }))
    };
    locked(inner).map_err(|e| e.to_string())?
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    /// `session/pane` or `pane` there.
    to: String,
    text: String,
    /// The sender's own address, as `imkr msg` writes it.
    from: String,
}

/// Sends `text` to `to` (`session/pane`) at `contact`; what they answered.
pub fn send(contact: &Contact, to: &str, text: &str, from: &str) -> Result<String> {
    if text.len() > MAX_TEXT {
        bail!("the message is longer than {} KB", MAX_TEXT / 1024);
    }
    if let Some(person) = &contact.brain {
        if text.len() > MAX_BRAIN_TEXT {
            bail!(
                "AI Brain carries at most {} KB, this is {} KB",
                MAX_BRAIN_TEXT / 1024,
                text.len().div_ceil(1024)
            );
        }
        brain_cli(&["senden", person, text, "--von", from, "--ziel", to], "")
            .with_context(|| format!("not sent to {} through AI Brain", contact.name))?;
        return Ok(format!(
            "{} gets it through AI Brain once their imkr asks there, within a minute while \
             it runs; then it waits for them to accept it, unless they trust you",
            contact.name
        ));
    }
    let answer = post(
        &contact.url,
        "/contact/message",
        Some(&contact.token),
        &serde_json::to_value(Message {
            to: to.to_owned(),
            text: text.to_owned(),
            from: from.to_owned(),
        })?,
    )
    .with_context(|| format!("{} not reached at {}", contact.name, contact.url))?;
    Ok(match answer["pane"].as_u64() {
        Some(pane) => format!("delivered to {} pane {pane}", contact.name),
        None => format!(
            "{} has it: it waits until they accept it in their imkr",
            contact.name
        ),
    })
}

/// The web server's side of `send`.
pub fn receive(token: Option<&str>, body: &[u8]) -> Result<serde_json::Value, String> {
    let token = token.ok_or("no token")?;
    let contact = load()
        .into_iter()
        .find(|c| web::same_secret(&c.key, token))
        .ok_or("not a contact")?;
    let message: Message = serde_json::from_slice(body).map_err(|e| format!("bad request: {e}"))?;
    take_in(&contact.name, contact.trusted, message, true)
}

/// A message from `contact` for a pane here: typed in if `trusted`, else
/// waiting for the user. With `check`, a session that does not run is said
/// right away; a message fetched from AI Brain waits anyway, it is not
/// there any more to say it to.
fn take_in(
    contact: &str,
    trusted: bool,
    message: Message,
    check: bool,
) -> Result<serde_json::Value, String> {
    if message.text.trim().is_empty() || message.text.len() > MAX_TEXT {
        return Err("bad request: text".into());
    }
    let to = message.to.trim();
    match to.split_once('/') {
        Some((session, who)) if protocol::valid_session_name(session) && !who.is_empty() => {}
        None if !to.is_empty() && !to.contains(':') => {}
        _ => return Err(format!("bad address {to}: say session/pane")),
    }
    // Said right away, not once someone accepts it.
    let runs = target(to).is_ok_and(|(socket, _)| socket.exists());
    if check && !runs {
        return Err(format!(
            "no session {} runs there",
            to.split_once('/').map_or("default", |(s, _)| s)
        ));
    }
    let incoming = Incoming {
        id: 0,
        contact: contact.to_owned(),
        from: answer_address(contact, &message.from),
        to: to.to_owned(),
        text: message.text,
        at: web::now(),
    };
    if trusted && runs {
        let pane = deliver(&incoming).map_err(|e| format!("{e:#}"))?;
        return Ok(serde_json::json!({ "pane": pane }));
    }
    let id = locked(|| -> Result<u64> {
        let mut list = waiting();
        let id = list.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        list.push(Incoming { id, ..incoming });
        if list.len() > MAX_WAITING {
            list.remove(0);
        }
        write("contact-inbox.json", &list)?;
        Ok(id)
    })
    .and_then(|r| r)
    .map_err(|e| format!("{e:#}"))?;
    notify(&format!(
        "message from {contact} waits for you: Ctrl+B i, or `imkr connect inbox`"
    ));
    Ok(serde_json::json!({ "queued": id }))
}

/// Where to answer a message, as an address here: the sender's machine
/// becomes the contact's name, `claude@omarchy:api/0` → `claude@martin:api/0`.
fn answer_address(contact: &str, from: &str) -> String {
    let safe = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    };
    let Some((head, place)) = from.rsplit_once(':') else {
        return contact.to_owned();
    };
    match head.split_once('@') {
        Some((program, _)) if safe(program) && safe(place) => {
            format!("{program}@{contact}:{place}")
        }
        None if safe(place) => format!("{contact}:{place}"),
        _ => contact.to_owned(),
    }
}

/// The session's socket and the pane in it, for `session/pane` or `pane`.
fn target(to: &str) -> Result<(PathBuf, &str)> {
    Ok(match to.split_once('/') {
        Some((session, who)) => (protocol::session_socket(session)?, who),
        None => (protocol::socket_path()?, to),
    })
}

/// Types a message into its pane here; the pane it went to.
fn deliver(message: &Incoming) -> Result<u64> {
    let (socket, who) = target(&message.to)?;
    let session = message.to.split_once('/').map(|(s, _)| s);
    let reply = crate::client::request_at(
        &socket,
        Request::Message {
            to: who.to_owned(),
            text: message.text.clone(),
            from: message.from.clone(),
        },
    )
    .with_context(|| format!("no session {}", session.unwrap_or("running here")))?;
    Ok(reply["pane"].as_u64().unwrap_or_default())
}

/// Accepts a waiting message: typed into its pane now.
pub fn accept(id: u64) -> Result<String> {
    let message = take(id)?;
    match deliver(&message) {
        Ok(pane) => Ok(format!("delivered to pane {pane}")),
        Err(e) => {
            // Kept, to try again once the session runs.
            locked(|| {
                let mut list = waiting();
                list.push(message);
                list.sort_by_key(|m| m.id);
                write("contact-inbox.json", &list)
            })??;
            Err(e)
        }
    }
}

/// Throws a waiting message away.
pub fn decline(id: u64) -> Result<()> {
    take(id).map(drop)
}

fn take(id: u64) -> Result<Incoming> {
    locked(|| -> Result<Incoming> {
        let mut list = waiting();
        let i = list
            .iter()
            .position(|m| m.id == id)
            .with_context(|| format!("no message {id} waits"))?;
        let message = list.remove(i);
        write("contact-inbox.json", &list)?;
        Ok(message)
    })?
}

pub fn remove(name: &str) -> Result<()> {
    locked(|| {
        let mut contacts = load();
        let before = contacts.len();
        contacts.retain(|c| c.name != name);
        if contacts.len() == before {
            bail!("no contact {name}");
        }
        save(&contacts)
    })?
}

pub fn set_trusted(name: &str, trusted: bool) -> Result<()> {
    locked(|| {
        let mut contacts = load();
        let contact = contacts
            .iter_mut()
            .find(|c| c.name == name)
            .with_context(|| format!("no contact {name}"))?;
        contact.trusted = trusted;
        save(&contacts)
    })?
}

/// The AI Brain command: `ai-brain-harness`, or `ai-brain-cli` as it was
/// called before (October 2026) on machines not updated yet.
fn brain_program() -> &'static str {
    let found = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
    };
    if !found("ai-brain-harness") && found("ai-brain-cli") {
        "ai-brain-cli"
    } else {
        "ai-brain-harness"
    }
}

/// Runs `ai-brain-harness nachricht ARGS` with `input` on stdin; its output.
/// The harness holds the sign-in to AI Brain, imkr none.
fn brain_cli(args: &[&str], input: &str) -> Result<String> {
    let program = brain_program();
    let mut child = std::process::Command::new("timeout")
        .args(["30", program, "nachricht"])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        match out.status.code() {
            Some(127) => bail!("ai-brain-harness is not installed here"),
            Some(124) => bail!("{program} did not answer within 30 s"),
            _ => bail!("{}", why.trim()),
        }
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Makes `person` from AI Brain a contact, called `alias` here: only those
/// AI Brain lets this user write to.
pub fn add_brain(person: &str, alias: Option<&str>) -> Result<Contact> {
    let answer: serde_json::Value = serde_json::from_str(&brain_cli(&["kontakte", "--json"], "")?)
        .context("ai-brain-harness answered no list of contacts")?;
    let known = answer["kontakte"].as_array().cloned().unwrap_or_default();
    // By name or e-mail, exactly; kept under the name AI Brain says they
    // send as (`von_person`).
    let Some(found) = known
        .iter()
        .find(|k| k["person"].as_str() == Some(person) || k["email"].as_str() == Some(person))
    else {
        let names: Vec<&str> = known.iter().filter_map(|k| k["person"].as_str()).collect();
        bail!(
            "AI Brain knows no {person} you may write to; these: {}",
            names.join(", ")
        );
    };
    let person = found["person"].as_str().unwrap_or(person);
    let wanted = alias
        .and_then(clean_name)
        .or_else(|| clean_name(&person.to_lowercase()))
        .context("no usable name")?;
    locked(|| {
        let mut contacts = load();
        if let Some(c) = contacts.iter().find(|c| c.brain.as_deref() == Some(person)) {
            bail!("{person} is a contact already, as {}", c.name);
        }
        let contact = Contact {
            name: free_name(&contacts, &wanted),
            url: String::new(),
            token: String::new(),
            key: String::new(),
            brain: Some(person.to_owned()),
            trusted: false,
            since: web::now(),
        };
        contacts.push(contact.clone());
        save(&contacts).map(|()| contact)
    })?
}

/// What `ai-brain-harness nachricht abholen --json` answers.
#[derive(Debug, Deserialize)]
struct FetchedAll {
    #[serde(default)]
    nachrichten: Vec<Fetched>,
}

/// One message as `ai-brain-harness nachricht abholen` hands it over: given to
/// this machine at the first fetch, again until it is confirmed.
#[derive(Debug, Deserialize)]
struct Fetched {
    id: u64,
    /// Who sent it, as AI Brain verified; `von` is what their imkr said.
    von_person: String,
    #[serde(default)]
    von: Option<String>,
    #[serde(default)]
    ziel: Option<String>,
    text: String,
}

/// Takes in what waits in AI Brain for this user on this machine: as from
/// a contact if the sender is one, else under the sender's AI Brain name,
/// always asking first then. Only with a contact through AI Brain, so
/// machines without one never run the CLI. How many came.
pub fn fetch_brain() -> Result<usize> {
    use std::os::fd::AsRawFd;
    let contacts = load();
    if !contacts.iter().any(|c| c.brain.is_some()) {
        return Ok(0);
    }
    // One fetch per machine at a time: every session's server asks, and two
    // at once both got the same unconfirmed message and both took it in.
    // Its own lock, not `locked`: `take_in` takes that one inside.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path("contact-brain.lock")?)?;
    // SAFETY: flock on a descriptor we own; released when `lock` closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(0);
    }
    let out = brain_cli(&["abholen", "--json"], "")?;
    let fetched = serde_json::from_str::<FetchedAll>(&out)
        .context("ai-brain-harness answered no list of messages")?
        .nachrichten;
    let count = fetched.len();
    // At least once, not exactly once: what was taken but not confirmed
    // comes again, then only the confirmation is missing.
    let mut seen: Vec<u64> = read("contact-brain-seen.json");
    for m in fetched {
        let id = m.id;
        if seen.contains(&id) {
            let _ = brain_cli(&["bestaetigen", &id.to_string()], "");
            continue;
        }
        let (name, trusted) = contacts
            .iter()
            .find(|c| c.brain.as_deref() == Some(m.von_person.as_str()))
            .map(|c| (c.name.clone(), c.trusted))
            .unwrap_or_else(|| {
                let name = clean_name(&m.von_person.to_lowercase());
                (name.unwrap_or("someone".into()), false)
            });
        // Without a pane to go to it cannot be taken; said in the log.
        let message = Message {
            to: m.ziel.unwrap_or_default(),
            text: m.text,
            from: m.von.unwrap_or_default(),
        };
        // A message that cannot be taken (no such pane address) is
        // confirmed too: it would come again forever.
        if let Err(e) = take_in(&name, trusted, message, false) {
            eprintln!("message from {name} through AI Brain not taken: {e}");
        }
        seen.push(id);
        if seen.len() > BRAIN_SEEN {
            seen.remove(0);
        }
        let _ = write("contact-brain-seen.json", &seen);
        let _ = brain_cli(&["bestaetigen", &id.to_string()], "");
    }
    Ok(count)
}

/// Tells every session here, as a flash.
fn notify(text: &str) {
    for session in crate::client::local_sessions() {
        if session.dormant.is_some() {
            continue;
        }
        if let Ok(socket) = protocol::session_socket(&session.name) {
            let _ = crate::client::request_at(
                &socket,
                Request::Flash {
                    text: text.to_owned(),
                },
            );
        }
    }
}

/// POSTs `body` as JSON to `path` of the imkr at `url`; its JSON answer.
fn post(
    url: &str,
    path: &str,
    token: Option<&str>,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let host = url
        .strip_prefix("http://")
        .unwrap_or(url)
        .trim_end_matches('/');
    let addr = host
        .to_socket_addrs()?
        .next()
        .with_context(|| format!("no address for {host}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let body = body.to_string();
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    let mut answer = Vec::new();
    stream
        .take(web::MAX_BODY as u64 + 16 * 1024)
        .read_to_end(&mut answer)?;
    let answer = String::from_utf8_lossy(&answer);
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .context("no HTTP answer: is it imkr's web server?")?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .context("no HTTP status")?;
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    if status == 404 {
        bail!("imkr there is too old for contacts, it needs `imkr update`");
    }
    if !(200..300).contains(&status) {
        let reason = value["error"].as_str().unwrap_or(body.trim());
        bail!("{reason}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_go_to_the_contact() {
        assert_eq!(
            answer_address("martin", "claude@omarchy:api/0"),
            "claude@martin:api/0"
        );
        assert_eq!(answer_address("martin", "omarchy:api/3"), "martin:api/3");
        assert_eq!(answer_address("martin", "the user at omarchy"), "martin");
        // Nothing they send ends up as an address of something else.
        assert_eq!(answer_address("martin", "x@y:a b;rm"), "martin");
    }

    #[test]
    fn what_ai_brain_hands_over() {
        // As `ai-brain-harness nachricht abholen --json` answers (#495).
        let out = r#"{"nachrichten":[{"id":123,"von_person":"Bastian",
            "von":"claude@omarchy:imkr/0","ziel":"api/claude","text":"hi",
            "angelegt":"2026-10-14T13:00:00Z","abholungen":2},
            {"id":124,"von_person":"Martin","von":null,"ziel":null,"text":"x"}]}"#;
        let fetched = serde_json::from_str::<FetchedAll>(out).unwrap().nachrichten;
        assert_eq!(fetched[0].von_person, "Bastian");
        assert_eq!(fetched[0].ziel.as_deref(), Some("api/claude"));
        assert_eq!(
            answer_address("bastian", fetched[0].von.as_deref().unwrap()),
            "claude@bastian:imkr/0"
        );
        assert!(fetched[1].ziel.is_none());
        // Nothing waiting: an empty list, or the field left out.
        assert!(
            serde_json::from_str::<FetchedAll>("{}")
                .unwrap()
                .nachrichten
                .is_empty()
        );
    }

    #[test]
    fn contacts_of_both_ways_load() {
        // Saved before contacts through AI Brain, and one through it.
        let old = r#"[{"name":"martin","url":"http://100.97.1.2:4657/","token":"t",
            "key":"k","trusted":false,"since":1}]"#;
        let brain = r#"[{"name":"martin","brain":"martin","since":1}]"#;
        let old: Vec<Contact> = serde_json::from_str(old).unwrap();
        let brain: Vec<Contact> = serde_json::from_str(brain).unwrap();
        assert!(old[0].brain.is_none());
        assert_eq!(brain[0].brain.as_deref(), Some("martin"));
        assert!(brain[0].url.is_empty());
    }

    #[test]
    fn urls_as_typed() {
        assert_eq!(
            normal_url("100.97.1.2:4657").unwrap(),
            "http://100.97.1.2:4657/"
        );
        assert_eq!(
            normal_url("http://100.97.1.2:4657/").unwrap(),
            "http://100.97.1.2:4657/"
        );
        assert_eq!(
            normal_url("100.97.1.2").unwrap(),
            format!("http://100.97.1.2:{}/", web::PORT)
        );
        assert!(normal_url("https://x/y").is_err());
    }

    #[test]
    fn names_stay_apart() {
        assert_eq!(clean_name(" Mar tin!"), Some("Martin".into()));
        assert_eq!(clean_name("!!"), None);
        let taken = Contact {
            name: "martin".into(),
            url: String::new(),
            token: String::new(),
            key: String::new(),
            brain: None,
            trusted: false,
            since: 0,
        };
        assert_eq!(free_name(&[taken], "martin"), "martin-2");
    }
}
