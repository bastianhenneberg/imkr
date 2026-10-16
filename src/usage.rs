//! What each account has used of its plan, for `imkr account usage` and
//! `Ctrl+B s u`: Claude's windows as Anthropic tells them (the numbers of
//! Claude Code's `/usage`), Codex's from its own logs, and the tokens in
//! the transcripts. Accounts of one agent share their transcripts (see
//! `account::Store::link_shared`), so tokens count per agent, not per account.

use crate::account::{Account, Store};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One limit window: how much of it is used and when it starts anew.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window {
    /// `5h`, `week`, or e.g. `week opus`.
    pub label: String,
    /// Percent used, 0 to 100.
    pub used: f64,
    /// When it resets, seconds since 1970.
    pub resets_at: Option<u64>,
}

/// An account with its plan and windows, or why they are unknown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountUsage {
    pub tool: String,
    /// As the frame shows it: who is logged in, or provider and model.
    pub name: String,
    /// The account new shells start in.
    pub default: bool,
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    /// Why there are no windows, or where they come from.
    pub note: Option<String>,
}

/// Tokens an agent used, all its accounts together.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tokens {
    pub tool: String,
    pub last_5h: u64,
    pub last_7d: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub accounts: Vec<AccountUsage>,
    pub tokens: Vec<Tokens>,
}

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

/// Every account of this machine, each subscription once, with its usage.
pub fn report() -> Result<Report> {
    let store = Store::here()?;
    let now = crate::persist::now();
    let mut accounts = Vec::new();
    for tool in crate::account::TOOLS {
        let default = store.default_of(tool);
        let mut seen = Vec::new();
        for account in store.of_tool(tool) {
            // The same subscription twice has one limit: shown once.
            if let Some(identity) = store.identity(&account) {
                if seen.contains(&identity.key) {
                    continue;
                }
                seen.push(identity.key);
            }
            accounts.push(usage_of(&store, &account, account.name == default, now));
        }
    }
    let tokens = vec![
        Tokens {
            tool: "claude".into(),
            last_5h: claude_tokens(&store.usual_dir("claude").join("projects"), now, 5 * HOUR),
            last_7d: claude_tokens(&store.usual_dir("claude").join("projects"), now, 7 * DAY),
        },
        Tokens {
            tool: "codex".into(),
            last_5h: codex_tokens(&store.usual_dir("codex").join("sessions"), now, 5 * HOUR),
            last_7d: codex_tokens(&store.usual_dir("codex").join("sessions"), now, 7 * DAY),
        },
    ];
    Ok(Report { accounts, tokens })
}

fn usage_of(store: &Store, account: &Account, default: bool, now: u64) -> AccountUsage {
    let mut usage = AccountUsage {
        tool: account.tool.clone(),
        name: crate::account::shown(store, account),
        default,
        plan: None,
        windows: Vec::new(),
        note: None,
    };
    if let Some(url) = &account.url {
        usage.note = Some(match crate::account::provider_name(url).as_str() {
            "Ollama" => "local, no limit".into(),
            provider => format!("{provider}: its limits are not read"),
        });
        return usage;
    }
    if !store.logged_in(account) {
        usage.note = Some(format!(
            "not logged in: imkr account login {}:{}",
            account.tool, account.name
        ));
        return usage;
    }
    let dir = store
        .dir_of(account)
        .unwrap_or_else(|| store.usual_dir(&account.tool));
    let found = match account.tool.as_str() {
        "codex" => codex_limits(&store.usual_dir("codex").join("sessions")).map(|(windows, at)| {
            usage.note = Some(format!(
                "as of the last Codex run, {}",
                crate::protocol::chrono_like(at).unwrap_or_default()
            ));
            (None, windows)
        }),
        _ => {
            // The plan is in the login, also when the windows are not known.
            usage.plan = claude_plan(&dir);
            claude_limits(&dir, now).map(|(plan, windows)| (plan.or(usage.plan.clone()), windows))
        }
    };
    match found {
        Ok((plan, windows)) => {
            usage.plan = plan;
            usage.windows = windows;
        }
        Err(e) => usage.note = Some(format!("{e:#}")),
    }
    usage
}

/// The plan of the Claude login in `dir`, e.g. `Max 20x`.
fn claude_plan(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".credentials.json")).ok()?;
    let login: serde_json::Value = serde_json::from_str(&text).ok()?;
    let oauth = &login["claudeAiOauth"];
    let plan = plan_name(
        oauth["subscriptionType"].as_str()?,
        oauth["rateLimitTier"].as_str().unwrap_or_default(),
    );
    Some(plan).filter(|p| !p.is_empty())
}

/// The plan and windows of the Claude login in `dir`, asked of Anthropic
/// as Claude Code does for `/usage`. Never refreshes the login: that would
/// replace the keys Claude Code holds for this account.
fn claude_limits(dir: &Path, now: u64) -> Result<(Option<String>, Vec<Window>)> {
    let text =
        std::fs::read_to_string(dir.join(".credentials.json")).context("no Claude login here")?;
    let login: serde_json::Value = serde_json::from_str(&text)?;
    let oauth = &login["claudeAiOauth"];
    let plan = plan_name(
        oauth["subscriptionType"].as_str().unwrap_or_default(),
        oauth["rateLimitTier"].as_str().unwrap_or_default(),
    );
    let expires = oauth["expiresAt"].as_u64().unwrap_or(0) / 1000;
    if expires <= now {
        // Only the access key of some hours is old; the login lasts weeks
        // and Claude Code fetches a new key with it on its next start here.
        let login = oauth["refreshTokenExpiresAt"].as_u64().unwrap_or(0) / 1000;
        if login > 0 && login <= now {
            bail!("logged out: imkr account login");
        }
        let since = crate::protocol::chrono_like(expires).unwrap_or_default();
        bail!("not used since {since}: numbers once Claude runs in it again");
    }
    let token = oauth["accessToken"].as_str().context("no access token")?;
    // The key goes on stdin, never on the command line where ps shows it.
    let mut curl = Command::new("curl")
        .args(["-sS", "-m", "10", "-H", "@-"])
        .arg("https://api.anthropic.com/api/oauth/usage")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("curl is needed to ask Anthropic")?;
    if let Some(mut stdin) = curl.stdin.take() {
        write!(
            stdin,
            "Authorization: Bearer {token}\nanthropic-beta: oauth-2025-04-20\nUser-Agent: imkr\n"
        )?;
    }
    let out = curl.wait_with_output()?;
    if !out.status.success() {
        bail!("Anthropic did not answer");
    }
    let answer: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("Anthropic answered no usage")?;
    if answer.get("five_hour").is_none() {
        bail!("Anthropic answered no usage");
    }
    Ok((
        Some(plan).filter(|p| !p.is_empty()),
        claude_windows(&answer),
    ))
}

/// The windows in Anthropic's answer that have a value.
fn claude_windows(answer: &serde_json::Value) -> Vec<Window> {
    [
        ("five_hour", "5h"),
        ("seven_day", "week"),
        ("seven_day_opus", "week opus"),
        ("seven_day_sonnet", "week sonnet"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let window = answer.get(key)?;
        Some(Window {
            label: label.into(),
            used: window["utilization"].as_f64()?,
            resets_at: window["resets_at"].as_str().and_then(parse_time),
        })
    })
    .collect()
}

/// `max` with `default_claude_max_20x` is `Max 20x`.
fn plan_name(kind: &str, tier: &str) -> String {
    let mut name = kind.to_owned();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    match tier.rsplit('_').next() {
        Some(times) if times.ends_with('x') && times[..times.len() - 1].parse::<u32>().is_ok() => {
            format!("{name} {times}")
        }
        _ => name,
    }
}

/// `2026-10-16T19:00:00.180696+00:00` as seconds since 1970.
fn parse_time(text: &str) -> Option<u64> {
    let date = text.get(0..19)?;
    let num = |range: std::ops::Range<usize>| date.get(range)?.parse::<i32>().ok();
    // SAFETY: timegm only reads the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = num(0..4)? - 1900;
    tm.tm_mon = num(5..7)? - 1;
    tm.tm_mday = num(8..10)?;
    tm.tm_hour = num(11..13)?;
    tm.tm_min = num(14..16)?;
    tm.tm_sec = num(17..19)?;
    let utc = unsafe { libc::timegm(&mut tm) };
    // The offset after the seconds, `+02:00`; `Z` and `+00:00` are none.
    let rest = &text[19..];
    let offset = rest
        .find(['+', '-'])
        .and_then(|at| {
            let sign = if rest[at..].starts_with('-') { -1 } else { 1 };
            let hours: i64 = rest.get(at + 1..at + 3)?.parse().ok()?;
            let minutes: i64 = rest.get(at + 4..at + 6)?.parse().ok()?;
            Some(sign * (hours * 3600 + minutes * 60))
        })
        .unwrap_or(0);
    u64::try_from(utc - offset).ok()
}

/// The newest rate limits Codex wrote into its session logs, and when:
/// it notes them with every answer.
fn codex_limits(sessions: &Path) -> Result<(Vec<Window>, u64)> {
    for file in newest_files(sessions, 8) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines().rev() {
            if !line.contains("\"rate_limits\"") {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let limits = &event["payload"]["rate_limits"];
            let windows: Vec<Window> = ["primary", "secondary"]
                .into_iter()
                .filter_map(|key| {
                    let window = limits.get(key).filter(|w| !w.is_null())?;
                    let minutes = window["window_minutes"].as_u64().unwrap_or(0);
                    Some(Window {
                        label: match minutes {
                            300 => "5h".into(),
                            10080 => "week".into(),
                            m if m % 1440 == 0 && m > 0 => format!("{} days", m / 1440),
                            m => format!("{}h", m / 60),
                        },
                        used: window["used_percent"].as_f64()?,
                        resets_at: window["resets_at"].as_u64(),
                    })
                })
                .collect();
            if windows.is_empty() {
                continue;
            }
            let at = event["timestamp"]
                .as_str()
                .and_then(parse_time)
                .unwrap_or(0);
            return Ok((windows, at));
        }
    }
    bail!("Codex has noted no limits yet")
}

/// Tokens of Claude's transcripts within `span` before `now`: fresh input,
/// cache writes and output, not cache reads (those would dwarf the rest).
/// A message stands once per content block in a transcript: counted once.
fn claude_tokens(projects: &Path, now: u64, span: u64) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    for file in files_since(projects, now.saturating_sub(span)) {
        let Ok(file) = std::fs::File::open(&file) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let at = entry["timestamp"]
                .as_str()
                .and_then(parse_time)
                .unwrap_or(0);
            if at + span < now {
                continue;
            }
            let message = &entry["message"];
            if let Some(id) = message["id"].as_str()
                && !seen.insert(id.to_owned())
            {
                continue;
            }
            let usage = &message["usage"];
            let n = |key: &str| usage[key].as_u64().unwrap_or(0);
            total += n("input_tokens") + n("cache_creation_input_tokens") + n("output_tokens");
        }
    }
    total
}

/// Tokens of Codex's sessions within `span` before `now`: input without
/// the cached part, and output.
fn codex_tokens(sessions: &Path, now: u64, span: u64) -> u64 {
    let mut total = 0;
    for file in files_since(sessions, now.saturating_sub(span)) {
        let Ok(file) = std::fs::File::open(&file) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"token_count\"") {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let at = event["timestamp"]
                .as_str()
                .and_then(parse_time)
                .unwrap_or(0);
            if at + span < now {
                continue;
            }
            let last = &event["payload"]["info"]["last_token_usage"];
            let n = |key: &str| last[key].as_u64().unwrap_or(0);
            total +=
                n("input_tokens").saturating_sub(n("cached_input_tokens")) + n("output_tokens");
        }
    }
    total
}

/// The `.jsonl` files below `dir` changed since `since`.
fn files_since(dir: &Path, since: u64) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") && modified(&meta) >= since {
                found.push(path);
            }
        }
    }
    found
}

/// The `count` `.jsonl` files below `dir` changed last, newest first.
fn newest_files(dir: &Path, count: usize) -> Vec<PathBuf> {
    let mut files: Vec<(u64, PathBuf)> = files_since(dir, 0)
        .into_iter()
        .filter_map(|path| Some((modified(&std::fs::metadata(&path).ok()?), path)))
        .collect();
    files.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    files
        .into_iter()
        .take(count)
        .map(|(_, path)| path)
        .collect()
}

fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// `1234567` as `1.2 M`.
pub fn short_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.0} k", n as f64 / 1e3),
        _ => format!("{:.1} M", n as f64 / 1e6),
    }
}

/// The report as `imkr account usage` prints it, with colours when
/// `colour`: a bar per window, yellow from 80 %, red from 95 %.
pub fn print(report: &Report, colour: bool) -> String {
    let paint = |code: &str, text: &str| {
        if colour {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    };
    let width = report
        .accounts
        .iter()
        .map(|a| a.name.chars().count() + if a.default { 11 } else { 0 })
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for tool in crate::account::TOOLS {
        let accounts: Vec<&AccountUsage> =
            report.accounts.iter().filter(|a| a.tool == *tool).collect();
        if accounts.is_empty() {
            continue;
        }
        out.push_str(&paint(
            "1",
            if *tool == "codex" { "Codex" } else { "Claude" },
        ));
        out.push('\n');
        for account in accounts {
            let name = if account.default {
                format!("{} (standard)", account.name)
            } else {
                account.name.clone()
            };
            let plan = account.plan.clone().unwrap_or_default();
            let lead = format!("  {name:<width$}  {plan:<8}");
            if account.windows.is_empty() {
                let note = account.note.as_deref().unwrap_or("nothing known");
                out.push_str(&format!("{lead}  {}\n", paint("2", note)));
                continue;
            }
            for (i, window) in account.windows.iter().enumerate() {
                let lead = if i == 0 {
                    lead.clone()
                } else {
                    " ".repeat(lead.chars().count())
                };
                let filled = ((window.used / 10.0).round() as usize).min(10);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled));
                let code = match window.used {
                    u if u >= 95.0 => "31",
                    u if u >= 80.0 => "33",
                    _ => "32",
                };
                let resets = window
                    .resets_at
                    .and_then(crate::protocol::chrono_like)
                    .map(|at| format!("  resets {at}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{lead}  {:<11} {} {:>3.0} %{}\n",
                    window.label,
                    paint(code, &bar),
                    window.used,
                    paint("2", &resets)
                ));
            }
            if let Some(note) = &account.note {
                let pad = " ".repeat(lead.chars().count() + 2);
                out.push_str(&format!("{pad}{}\n", paint("2", note)));
            }
        }
    }
    out.push_str(&paint("1", "Tokens"));
    out.push_str(&paint(
        "2",
        " (all accounts of an agent share their transcripts; without cache reads)",
    ));
    out.push('\n');
    for tokens in &report.tokens {
        let tool = if tokens.tool == "codex" {
            "Codex"
        } else {
            "Claude"
        };
        out.push_str(&format!(
            "  {tool:<8} 5h {:>8}   7 days {:>8}\n",
            short_count(tokens.last_5h),
            short_count(tokens.last_7d)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_and_times_are_read() {
        assert_eq!(plan_name("max", "default_claude_max_20x"), "Max 20x");
        assert_eq!(plan_name("pro", "default_claude_pro"), "Pro");
        assert_eq!(
            parse_time("2026-10-16T19:00:00.180696+00:00"),
            Some(1_792_177_200)
        );
        assert_eq!(parse_time("2026-10-16T21:00:00+02:00"), Some(1_792_177_200));
        assert_eq!(parse_time("2026-10-16T19:00:00Z"), Some(1_792_177_200));
        assert_eq!(short_count(950), "950");
        assert_eq!(short_count(31_200), "31 k");
        assert_eq!(short_count(18_400_000), "18.4 M");
    }

    #[test]
    fn claude_windows_are_those_with_a_value() {
        let answer = serde_json::json!({
            "five_hour": {"utilization": 12.0, "resets_at": "2026-10-16T19:00:00+00:00"},
            "seven_day": {"utilization": 39.0, "resets_at": null},
            "seven_day_opus": null,
        });
        let windows = claude_windows(&answer);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "5h");
        assert_eq!(windows[0].resets_at, Some(1_792_177_200));
        assert_eq!(windows[1].used, 39.0);
    }

    #[test]
    fn codex_limits_and_tokens_come_from_its_logs() {
        let dir = std::env::temp_dir().join(format!("imkr-codex-{}", std::process::id()));
        let day = dir.join("2026/10/16");
        std::fs::create_dir_all(&day).unwrap();
        let now = 1_792_177_200;
        let line = |at: &str, input: u64, cached: u64, output: u64| {
            serde_json::json!({
                "timestamp": at,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {"last_token_usage": {
                        "input_tokens": input, "cached_input_tokens": cached,
                        "output_tokens": output
                    }},
                    "rate_limits": {
                        "primary": {"used_percent": 7.0, "window_minutes": 300, "resets_at": now + 100},
                        "secondary": {"used_percent": 41.0, "window_minutes": 10080, "resets_at": now + 900},
                    }
                }
            })
            .to_string()
        };
        let text = [
            line("2026-10-16T18:00:00Z", 1000, 400, 50),
            line("2026-10-10T18:00:00Z", 5000, 0, 0),
        ]
        .join("\n");
        std::fs::write(day.join("rollout-a.jsonl"), text).unwrap();
        let (windows, at) = codex_limits(&dir).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!((windows[0].label.as_str(), windows[0].used), ("5h", 7.0));
        assert_eq!((windows[1].label.as_str(), windows[1].used), ("week", 41.0));
        assert_eq!(
            at,
            parse_time("2026-10-10T18:00:00Z").unwrap(),
            "the last line"
        );
        // 1000 - 400 cached + 50 output within 5 hours; the older line only in 7 days.
        assert_eq!(codex_tokens(&dir, now, 5 * HOUR), 650);
        assert_eq!(codex_tokens(&dir, now, 7 * DAY), 5650);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
