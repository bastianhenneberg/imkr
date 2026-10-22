//! Push to the phone (#8566): when an agent needs you, stopped on an error
//! or its account is used up, the server tells ntfy and/or a webhook, so it
//! reaches you with no terminal attached and the web page closed.
//!
//! ```toml
//! [push]
//! ntfy = "https://ntfy.sh/a-topic-only-you-know"
//! webhook = "https://example.org/imkr"   # gets the event as JSON
//! on = ["blocked", "failed", "account"]  # also "done"
//! quiet = 60    # nothing while you typed in this session that recently
//! link = "http://100.97.56.232:4657"     # opened by a tap; else imkr web's
//! ```

use std::io::Write;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// The `[push]` table of the config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Push {
    /// An ntfy topic as its whole address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ntfy: Option<String>,
    /// An address that gets each event as JSON (`Event`) by POST.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<String>,
    /// The kinds pushed: `blocked`, `failed`, `account`, `done`.
    pub on: Vec<String>,
    /// Seconds after the last key in this session during which nothing is
    /// pushed: the user is at the keyboard and sees it.
    pub quiet: u64,
    /// What a tap opens; without it imkr web's address when it runs here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Default for Push {
    fn default() -> Self {
        Self {
            ntfy: None,
            webhook: None,
            on: ["blocked", "failed", "account"].map(String::from).to_vec(),
            quiet: 60,
            link: None,
        }
    }
}

impl Push {
    /// Whether anything is set up to push to.
    pub fn enabled(&self) -> bool {
        self.ntfy.is_some() || self.webhook.is_some()
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether events of `kind` are pushed.
    pub fn wants(&self, kind: &str) -> bool {
        self.enabled() && self.on.iter().any(|k| k == kind)
    }
}

/// What happened, as the webhook gets it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Event {
    /// `blocked`, `failed`, `account`, `done` or `test`.
    pub kind: String,
    pub machine: String,
    pub session: String,
    pub pane: u64,
    pub window: u8,
    /// The agent, e.g. `claude`.
    pub agent: String,
    /// What happened, e.g. `needs you`.
    pub text: String,
    /// What the agent says it works on (its terminal title).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub task: String,
    /// The bottom of its screen: its question, or the error.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub screen: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub link: String,
}

impl Event {
    /// The notification's title: `codingmachine · php: claude needs you`.
    pub fn title(&self) -> String {
        format!(
            "{} · {}: {} {}",
            self.machine, self.session, self.agent, self.text
        )
    }

    /// Its text: the task, then the screen's bottom.
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if !self.task.is_empty() {
            parts.push(self.task.clone());
        }
        if !self.screen.is_empty() {
            parts.push(self.screen.clone());
        }
        if parts.is_empty() {
            parts.push(format!("window {}, pane {}", self.window, self.pane));
        }
        let text = parts.join("\n");
        cut(&text, 600)
    }
}

/// The ntfy server and topic of a topic address: `https://ntfy.sh/x` →
/// (`https://ntfy.sh`, `x`).
pub fn ntfy_parts(address: &str) -> Option<(String, String)> {
    let address = address.trim_end_matches('/');
    let (server, topic) = address.rsplit_once('/')?;
    if topic.is_empty() || !server.contains("://") || server.ends_with(':') {
        return None;
    }
    Some((server.to_owned(), topic.to_owned()))
}

/// The JSON ntfy publishes from.
pub fn ntfy_body(topic: &str, event: &Event) -> serde_json::Value {
    let urgent = matches!(event.kind.as_str(), "blocked" | "failed");
    let mut body = serde_json::json!({
        "topic": topic,
        "title": event.title(),
        "message": event.message(),
        "priority": if urgent { 4 } else { 3 },
        "tags": [match event.kind.as_str() {
            "blocked" => "raising_hand",
            "failed" => "warning",
            "account" => "hourglass",
            "done" => "white_check_mark",
            _ => "robot",
        }],
    });
    if !event.link.is_empty() {
        body["click"] = event.link.clone().into();
    }
    body
}

/// Sends `event` to what `push` names, in the background: the loop never
/// waits on the network. Failures go to the server's log.
pub fn send(push: &Push, event: Event) {
    let mut posts: Vec<(String, serde_json::Value)> = Vec::new();
    if let Some(address) = &push.ntfy {
        match ntfy_parts(address) {
            Some((server, topic)) => posts.push((server, ntfy_body(&topic, &event))),
            None => eprintln!("push: not an ntfy topic address: {address}"),
        }
    }
    if let Some(address) = &push.webhook {
        posts.push((
            address.clone(),
            serde_json::to_value(&event).unwrap_or_default(),
        ));
    }
    std::thread::spawn(move || {
        for (address, body) in posts {
            if let Err(e) = post(&address, &body) {
                eprintln!("push to {address}: {e:#}");
            }
        }
    });
}

/// POSTs `body` as JSON to `address` with curl and waits for it.
pub fn post(address: &str, body: &serde_json::Value) -> anyhow::Result<()> {
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "-m",
            "15",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            address,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(body.to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!(
            "curl: {}",
            String::from_utf8_lossy(&out.stderr).trim().to_owned()
        );
    }
    Ok(())
}

/// `text` cut to `max` characters, with … when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntfy_addresses_split_into_server_and_topic() {
        assert_eq!(
            ntfy_parts("https://ntfy.sh/abc/"),
            Some(("https://ntfy.sh".into(), "abc".into()))
        );
        assert_eq!(
            ntfy_parts("http://10.0.0.2:8080/imkr"),
            Some(("http://10.0.0.2:8080".into(), "imkr".into()))
        );
        assert_eq!(ntfy_parts("https://ntfy.sh"), None);
        assert_eq!(ntfy_parts("abc"), None);
    }

    #[test]
    fn events_read_well() {
        let event = Event {
            kind: "blocked".into(),
            machine: "codingmachine".into(),
            session: "php".into(),
            agent: "claude".into(),
            text: "needs you".into(),
            task: "PHP-Sicherheitslücke prüfen".into(),
            screen: "Do you want to proceed?\n❯ 1. Yes".into(),
            link: "http://x".into(),
            ..Event::default()
        };
        assert_eq!(event.title(), "codingmachine · php: claude needs you");
        assert_eq!(
            event.message(),
            "PHP-Sicherheitslücke prüfen\nDo you want to proceed?\n❯ 1. Yes"
        );
        let body = ntfy_body("t", &event);
        assert_eq!(body["priority"], 4);
        assert_eq!(body["click"], "http://x");
        assert!(Push::default().on.contains(&"blocked".to_owned()));
        assert!(!Push::default().wants("blocked"));
    }
}
