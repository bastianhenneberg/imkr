//! Computer use (#9074): an agent sees this machine's screen and drives it.
//!
//! One trait per ability — `Sees`, `Points`, `Types`, `Checks` — so a
//! backend can be written per platform without the commands knowing which
//! one runs. Linux/Hyprland is implemented (grim, the Hyprland socket,
//! ydotool, wtype); macOS and Windows say so instead of pretending.
//!
//! Coordinates are **logical** and global, the way the compositor speaks
//! them: the same numbers `hyprctl cursorpos` prints, not device pixels.
//! A screenshot says how its pixels map onto them (`Shot::hint`), because
//! it is scaled down to something an agent can look at.
//!
//! Nothing here drives a locked session, and nothing types a password:
//! `probe()` refuses while the session is locked, every action asks it
//! first, and every action is logged (`screen.log`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How wide a screenshot is made by default: wide enough to read a
/// terminal on, small enough to look at.
pub const SHOT_WIDTH: u32 = 1280;

/// How long a permission given with `ask` lasts.
pub const GRANT_FOR: Duration = Duration::from_secs(30 * 60);

/// How long `imkr screen` waits for an answer to its request.
pub const ASK_FOR: Duration = Duration::from_secs(90);

/// An action counts as happening now for this long, for the bar.
pub const BUSY_FOR: Duration = Duration::from_secs(5);

/// An input daemon older than this is worth a word: `ydotoold` has been
/// seen to deliver pointer motion but no clicks or keys after weeks.
const DAEMON_OLD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

// ---------------------------------------------------------------------------
// What a backend answers with
// ---------------------------------------------------------------------------

/// One screen, in logical coordinates: `x`/`y` where it sits in the whole
/// layout, `width`/`height` its logical size (device pixels divided by
/// `scale`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    /// Whether the screen is on. A dark one gives black screenshots.
    pub on: bool,
    pub focused: bool,
}

impl Monitor {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x + i32::try_from(self.width).unwrap_or(i32::MAX)
            && y < self.y + i32::try_from(self.height).unwrap_or(i32::MAX)
    }
}

/// A screenshot and how to read coordinates off it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shot {
    /// Where the PNG was written.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Image pixels per logical pixel: 0.25 means the picture is a quarter
    /// the size of what the compositor measures.
    pub scale: f64,
    /// The logical coordinate of the image's top left pixel.
    pub origin: (i32, i32),
    /// Which screens are in it.
    pub monitors: Vec<Monitor>,
}

impl Shot {
    /// How to turn a pixel in this picture into a coordinate for `click`.
    pub fn hint(&self) -> String {
        format!(
            "click at x = {} + image_x / {:.4}, y = {} + image_y / {:.4}",
            self.origin.0, self.scale, self.origin.1, self.scale
        )
    }

    /// The logical coordinate of an image pixel.
    pub fn logical(&self, image_x: f64, image_y: f64) -> (i32, i32) {
        (
            self.origin.0 + (image_x / self.scale).round() as i32,
            self.origin.1 + (image_y / self.scale).round() as i32,
        )
    }

    pub fn json(&self) -> Value {
        json!({
            "path": self.path,
            "image": { "width": self.width, "height": self.height },
            "scale": self.scale,
            "origin": { "x": self.origin.0, "y": self.origin.1 },
            "hint": self.hint(),
            // The box this picture shows, so an agent can tell at once
            // whether what it is looking for is even in it.
            "covers": {
                "from": { "x": self.logical(0.0, 0.0).0, "y": self.logical(0.0, 0.0).1 },
                "to": {
                    "x": self.logical(f64::from(self.width), f64::from(self.height)).0,
                    "y": self.logical(f64::from(self.width), f64::from(self.height)).1,
                },
            },
            "monitors": self.monitors,
        })
    }
}

/// A piece of the desktop in logical coordinates, as `grim -g` takes it.
/// Without one a whole screen is meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// `X,Y,WxH` — the corner and the size, in logical pixels.
    pub fn parse(spec: &str) -> Result<Self> {
        let bad = || {
            anyhow::anyhow!(
                "a region is X,Y,WxH in logical pixels, e.g. 3072,0,1280x800 — not {spec:?}"
            )
        };
        let mut parts = spec.split(',');
        let (Some(x), Some(y), Some(size), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let (w, h) = size.split_once(['x', 'X']).ok_or_else(bad)?;
        let region = Region {
            x: x.trim().parse().map_err(|_| bad())?,
            y: y.trim().parse().map_err(|_| bad())?,
            width: w.trim().parse().map_err(|_| bad())?,
            height: h.trim().parse().map_err(|_| bad())?,
        };
        if region.width == 0 || region.height == 0 {
            return Err(bad());
        }
        Ok(region)
    }

    /// As grim spells it, and as the log notes it.
    pub fn geometry(&self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "left" | "l" | "1" => Button::Left,
            "right" | "r" | "3" => Button::Right,
            "middle" | "m" | "2" => Button::Middle,
            other => bail!("no mouse button {other}: left, right or middle"),
        })
    }
}

/// A key with its modifiers, as `ctrl+c` or `Return`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// Modifier names a backend understands: shift, ctrl, alt, logo, altgr.
    pub mods: Vec<String>,
    /// The key itself, as an X keysym name (`Return`, `c`, `F5`).
    pub keysym: String,
}

impl Key {
    /// `ctrl+c`, `alt+Tab`, `super+1`, `Return`, `esc` … into modifiers and
    /// a keysym libxkbcommon knows.
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            bail!("no key given");
        }
        // The last part is the key, everything before it a modifier. A
        // lone `+` is the key itself, not a separator.
        let parts: Vec<&str> = match spec {
            "+" => vec!["+"],
            _ => spec.split('+').filter(|p| !p.is_empty()).collect(),
        };
        let Some((key, mods)) = parts.split_last() else {
            bail!("no key in {spec:?}");
        };
        let mut wanted = Vec::new();
        for m in mods {
            wanted.push(
                match m.to_ascii_lowercase().as_str() {
                    "ctrl" | "control" | "c" => "ctrl",
                    "alt" | "meta" | "m" => "alt",
                    "shift" | "s" => "shift",
                    "super" | "cmd" | "win" | "logo" => "logo",
                    "altgr" => "altgr",
                    other => bail!("no modifier {other}: ctrl, alt, shift, super or altgr"),
                }
                .to_owned(),
            );
        }
        Ok(Key {
            mods: wanted,
            keysym: keysym(key),
        })
    }
}

/// The keysym for a friendly key name; anything unknown is passed on, so
/// every name libxkbcommon knows works even without an entry here.
fn keysym(name: &str) -> String {
    const NAMES: &[(&str, &str)] = &[
        ("enter", "Return"),
        ("return", "Return"),
        ("esc", "Escape"),
        ("escape", "Escape"),
        ("tab", "Tab"),
        ("space", "space"),
        ("backspace", "BackSpace"),
        ("bs", "BackSpace"),
        ("del", "Delete"),
        ("delete", "Delete"),
        ("ins", "Insert"),
        ("up", "Up"),
        ("down", "Down"),
        ("left", "Left"),
        ("right", "Right"),
        ("home", "Home"),
        ("end", "End"),
        ("pgup", "Prior"),
        ("pageup", "Prior"),
        ("pgdn", "Next"),
        ("pagedown", "Next"),
    ];
    let lower = name.to_ascii_lowercase();
    if let Some((_, sym)) = NAMES.iter().find(|(n, _)| *n == lower) {
        return (*sym).to_owned();
    }
    // f1 … f24 are capitalised keysyms.
    if let Some(rest) = lower.strip_prefix('f')
        && let Ok(n) = rest.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return format!("F{n}");
    }
    name.to_owned()
}

/// What the pre-flight check found. `blocked` set means: do nothing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Probe {
    /// Which backend answered, e.g. `hyprland`.
    pub backend: String,
    /// Why nothing may happen right now; empty means it may.
    pub blocked: Vec<String>,
    /// Worth knowing, but no reason to stop.
    pub notes: Vec<String>,
    pub monitors: Vec<Monitor>,
    /// Tools the backend needs and whether they are there.
    pub tools: Vec<(String, bool)>,
}

impl Probe {
    pub fn ok(&self) -> bool {
        self.blocked.is_empty()
    }
}

// ---------------------------------------------------------------------------
// One trait per ability
// ---------------------------------------------------------------------------

/// Seeing the screen.
pub trait Sees {
    fn monitors(&self) -> Result<Vec<Monitor>>;
    /// Writes a PNG to `to` at `scale` image pixels per logical pixel:
    /// of `region` if given, else of `monitor`, else of every screen.
    fn shot(
        &self,
        monitor: Option<&str>,
        region: Option<Region>,
        scale: f64,
        to: &Path,
    ) -> Result<()>;
}

/// Moving and clicking the pointer, in logical coordinates.
pub trait Points {
    fn cursor(&self) -> Result<(i32, i32)>;
    fn move_to(&self, x: i32, y: i32) -> Result<()>;
    fn click(&self, button: Button) -> Result<()>;
}

/// Typing text and pressing keys.
pub trait Types {
    fn text(&self, text: &str) -> Result<()>;
    fn key(&self, key: &Key) -> Result<()>;
}

/// Whether this screen may be driven right now at all.
pub trait Checks {
    fn probe(&self) -> Probe;
}

/// A desktop imkr can see and drive.
pub trait Desktop: Sees + Points + Types + Checks {
    fn name(&self) -> &'static str;
}

/// The backend for this machine, or why there is none. The error names
/// what would be needed, so nobody has to guess.
pub fn desktop() -> Result<Box<dyn Desktop>> {
    #[cfg(target_os = "linux")]
    {
        if let Some(hypr) = Hyprland::find() {
            return Ok(Box::new(hypr));
        }
        bail!(
            "no screen imkr can drive here: it needs Hyprland (its socket in \
             $XDG_RUNTIME_DIR/hypr). Other Wayland compositors and X11 are not \
             supported yet."
        )
    }
    #[cfg(target_os = "macos")]
    bail!(UNSUPPORTED_MACOS);
    #[cfg(target_os = "windows")]
    bail!(UNSUPPORTED_WINDOWS);
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    bail!("imkr screen is not supported on this system yet");
}

/// The systems `imkr screen` does not support yet, each with what it
/// would take. Printed by `imkr screen status` where there is no
/// backend, so the gap is an answer and not a silence.
pub fn gaps() -> Vec<(&'static str, &'static str)> {
    vec![
        ("macos", UNSUPPORTED_MACOS),
        ("windows", UNSUPPORTED_WINDOWS),
    ]
}

/// Said on macOS, so the gap is a sentence and not a surprise.
pub const UNSUPPORTED_MACOS: &str = "imkr screen does not support macOS yet. \
    Screenshots would go through ScreenCaptureKit (the `xcap` crate) and input \
    through CGEvent (the `enigo` crate), both behind the Screen Recording and \
    Accessibility permissions in System Settings.";

/// Said on Windows, likewise.
pub const UNSUPPORTED_WINDOWS: &str = "imkr screen does not support Windows yet. \
    Screenshots would go through the Desktop Duplication API (the `xcap` crate) \
    and input through SendInput (the `enigo` crate).";

// ---------------------------------------------------------------------------
// Linux: Hyprland
// ---------------------------------------------------------------------------

/// Hyprland through its own socket, `grim` for pictures, `ydotool` for the
/// mouse and `wtype` for text.
///
/// The socket rather than `hyprctl`: imkr runs in panes and over ssh,
/// where neither `HYPRLAND_INSTANCE_SIGNATURE` nor a PATH with `hyprctl`
/// is given. Both are found here instead.
#[cfg(target_os = "linux")]
pub struct Hyprland {
    socket: PathBuf,
    /// `WAYLAND_DISPLAY` for grim and wtype, found the same way.
    display: String,
    runtime: PathBuf,
}

#[cfg(target_os = "linux")]
impl Hyprland {
    /// The running instance, or `None` where there is none.
    pub fn find() -> Option<Self> {
        let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
        let socket = match std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") {
            Some(sig) => {
                let path = runtime.join("hypr").join(sig).join(".socket.sock");
                path.exists().then_some(path)
            }
            None => None,
        }
        // Started from a pane, a cron job or over ssh the variable is
        // missing: take the newest instance whose socket is there.
        .or_else(|| {
            let mut found: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(runtime.join("hypr"))
                .ok()?
                .flatten()
                .map(|e| e.path().join(".socket.sock"))
                .filter(|p| p.exists())
                .filter_map(|p| {
                    let when = p.metadata().and_then(|m| m.modified()).ok()?;
                    Some((when, p))
                })
                .collect();
            found.sort();
            found.pop().map(|(_, p)| p)
        })?;
        let display = std::env::var("WAYLAND_DISPLAY").ok().or_else(|| {
            // The compositor's own socket sits next to Hyprland's.
            let mut names: Vec<String> = std::fs::read_dir(&runtime)
                .ok()?
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
                .collect();
            names.sort();
            names.pop()
        })?;
        Some(Hyprland {
            socket,
            display,
            runtime,
        })
    }

    /// A backend on a socket given straight, rather than found in the
    /// environment: that is how the tests serve canned answers.
    #[cfg(test)]
    fn at(socket: PathBuf) -> Self {
        Hyprland {
            socket,
            display: "wayland-test".to_owned(),
            runtime: PathBuf::from("/nonexistent"),
        }
    }

    /// One request on Hyprland's socket, as `hyprctl` makes it.
    fn ask(&self, request: &str) -> Result<String> {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("cannot reach Hyprland at {}", self.socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer)?;
        Ok(answer)
    }

    fn ask_json(&self, request: &str) -> Result<Value> {
        let text = self.ask(request)?;
        serde_json::from_str(&text)
            .with_context(|| format!("Hyprland answered {request} with {text:?}"))
    }

    /// A tool of ours, with the session's Wayland environment: started
    /// from a pane neither variable is set, and then grim finds no screen.
    fn tool(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            // `wtype` decodes the text it is given through the locale.
            // In an agent's shell `LANG` is often unset and `LC_CTYPE`
            // is `POSIX`, and then: the argv form refuses loudly
            // ("Failed to deencode input argv"), and the stdin form —
            // the one used here, because typed text does not belong in
            // a command line — **silently drops everything from the
            // first non-ASCII byte** and exits 0. "Umlaute: äöü" became
            // "Umlaute: " with a report of success.
            .env("LC_ALL", "C.UTF-8");
        if let Some(sig) = self
            .socket
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            command.env("HYPRLAND_INSTANCE_SIGNATURE", sig);
        }
        command
    }

    /// Whether the locale handed to the helper tools really decodes
    /// UTF-8 — asked before typing anything beyond ASCII.
    ///
    /// `locale charmap` answers with exactly what `wtype` will see: the
    /// character set its own `setlocale` resolves to. `UTF-8` means the
    /// text arrives whole; `ANSI_X3.4-1968` is glibc falling back to
    /// ASCII, and then wtype's stdin form drops the rest of the text
    /// from the first such character, without a word and with exit 0.
    ///
    /// Probing wtype itself was tried and is wrong: every form of it
    /// that decodes text also types it, and the one flag that would not
    /// (`-s 0`) is refused as an invalid sleep time — a guard that is
    /// red whatever the locale says is no guard.
    fn check_utf8(&self) -> Result<()> {
        let charmap = self
            .tool("locale")
            .arg("charmap")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .unwrap_or_default();
        // No `locale` command at all: let the text through rather than
        // refuse on every system that does not ship one.
        if charmap.is_empty() || charmap.eq_ignore_ascii_case("UTF-8") {
            return Ok(());
        }
        bail!(
            "nothing typed: this machine cannot type anything beyond ASCII. Its locale \
             resolves to {charmap}, not UTF-8, and wtype would drop the text from the \
             first such character without saying so. A UTF-8 locale (C.UTF-8) fixes it."
        )
    }

    /// Runs a tool, with its complaint as the error.
    fn run(&self, program: &str, args: &[&str], input: Option<&str>) -> Result<()> {
        let mut child = self
            .tool(program)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run {program}"))?;
        if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
            stdin.write_all(input.as_bytes())?;
            drop(stdin);
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr);
            let why = why.trim();
            bail!(
                "{program} failed{}",
                if why.is_empty() {
                    String::new()
                } else {
                    format!(": {why}")
                }
            );
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Sees for Hyprland {
    fn monitors(&self) -> Result<Vec<Monitor>> {
        let list = self.ask_json("j/monitors")?;
        let Some(list) = list.as_array() else {
            bail!("Hyprland listed no monitors");
        };
        let mut monitors = Vec::new();
        for m in list {
            // The trap from the tray work: `x`/`y` are logical, `width`
            // and `height` are DEVICE pixels. The logical size is their
            // quotient with the scale — without it a 3840 wide monitor at
            // 1.25 looks 3840 logical pixels wide, and every coordinate on
            // the screen beside it lands on this one.
            let scale = m["scale"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
            monitors.push(Monitor {
                name: m["name"].as_str().unwrap_or_default().to_owned(),
                x: m["x"].as_i64().unwrap_or_default() as i32,
                y: m["y"].as_i64().unwrap_or_default() as i32,
                width: (m["width"].as_f64().unwrap_or_default() / scale).round() as u32,
                height: (m["height"].as_f64().unwrap_or_default() / scale).round() as u32,
                scale,
                on: m["dpmsStatus"].as_bool().unwrap_or(true),
                focused: m["focused"].as_bool().unwrap_or(false),
            });
        }
        Ok(monitors)
    }

    fn shot(
        &self,
        monitor: Option<&str>,
        region: Option<Region>,
        scale: f64,
        to: &Path,
    ) -> Result<()> {
        // grim's -s counts in LOGICAL pixels: `-s 0.25` of a 3072x2048
        // logical screen gives 768x512, whatever the device resolution is.
        // That is what makes the mapping back a single division.
        let scale = format!("{scale:.6}");
        let out = to.to_string_lossy().into_owned();
        let geometry = region.map(|r| r.geometry());
        let mut args = vec!["-s", &scale];
        match (&geometry, monitor) {
            // `-g` is already an absolute box: naming a screen as well
            // would only narrow it, and a region that crosses screens is
            // a thing an agent may well want.
            (Some(geometry), _) => args.extend(["-g", geometry]),
            (None, Some(name)) => args.extend(["-o", name]),
            (None, None) => {}
        }
        args.push(&out);
        self.run("grim", &args, None)
    }
}

#[cfg(target_os = "linux")]
impl Points for Hyprland {
    fn cursor(&self) -> Result<(i32, i32)> {
        let answer = self.ask("cursorpos")?;
        let (x, y) = answer
            .trim()
            .split_once(',')
            .with_context(|| format!("Hyprland gave the cursor as {answer:?}"))?;
        Ok((x.trim().parse()?, y.trim().parse()?))
    }

    fn move_to(&self, x: i32, y: i32) -> Result<()> {
        // Hyprland 0.56 reads the rest of a dispatch as Lua, so the old
        // `movecursor X Y` spelling fails with a parser error. This form
        // is exact and absolute — unlike `ydotool mousemove --absolute`,
        // which saturates at the right edge and needs its factor measured.
        let answer = self.ask(&format!(
            "dispatch hl.dsp.cursor.move{{ x = {x}, y = {y} }}"
        ))?;
        if answer.trim() != "ok" {
            bail!("Hyprland would not move the cursor: {}", answer.trim());
        }
        Ok(())
    }

    fn click(&self, button: Button) -> Result<()> {
        // ydotool's buttons are hex with 0x40 down and 0x80 up, so 0xC0 is
        // a whole left click. There is no compositor dispatcher for a
        // button press — this one has to go through uinput.
        let code = match button {
            Button::Left => "0xC0",
            Button::Right => "0xC1",
            Button::Middle => "0xC2",
        };
        self.run("ydotool", &["click", code], None)
    }
}

#[cfg(target_os = "linux")]
impl Types for Hyprland {
    fn text(&self, text: &str) -> Result<()> {
        // wtype brings its own keymap over the virtual-keyboard protocol,
        // so umlauts and anything else beyond ASCII arrive whatever the
        // active layout is — ydotool types kernel keycodes and loses them
        // on a US layout. The text goes in on stdin, never in argv.
        //
        // Before typing, make sure it *can* be typed whole: with a
        // broken locale wtype drops the tail without a word (see
        // `tool`). Half a message typed into somebody's editor, reported
        // as done, is worse than nothing happening.
        if !text.is_ascii() {
            self.check_utf8()?;
        }
        self.run("wtype", &["-"], Some(text))
    }

    fn key(&self, key: &Key) -> Result<()> {
        let mut args: Vec<String> = Vec::new();
        for m in &key.mods {
            args.extend(["-M".to_owned(), m.clone()]);
        }
        args.extend(["-k".to_owned(), key.keysym.clone()]);
        // The modifiers release themselves when wtype ends, but saying so
        // keeps the order plain for anyone reading the log.
        for m in key.mods.iter().rev() {
            args.extend(["-m".to_owned(), m.clone()]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run("wtype", &args, None)
    }
}

#[cfg(target_os = "linux")]
impl Checks for Hyprland {
    fn probe(&self) -> Probe {
        let mut probe = Probe {
            backend: "hyprland".to_owned(),
            ..Probe::default()
        };
        for tool in ["grim", "ydotool", "wtype"] {
            probe.tools.push((tool.to_owned(), which(tool)));
        }
        // A locked session is the hard stop: imkr never drives a lock
        // screen and never types a password. Its pointer and keys reach
        // nothing reliable there anyway, and a screenshot is the lock
        // screen with whatever the user typed into it.
        match self.ask_json("j/monitors") {
            Ok(list) => {
                if session_locked(&list) {
                    probe
                        .blocked
                        .push("the session is locked — imkr does not unlock it".to_owned());
                }
            }
            Err(e) => probe.blocked.push(format!("Hyprland does not answer: {e}")),
        }
        match self.monitors() {
            Ok(monitors) => {
                let dark: Vec<&str> = monitors
                    .iter()
                    .filter(|m| !m.on)
                    .map(|m| m.name.as_str())
                    .collect();
                if !dark.is_empty() && monitors.iter().all(|m| !m.on) {
                    probe.blocked.push(format!(
                        "every screen is off ({}) — grim gives black pictures",
                        dark.join(", ")
                    ));
                } else if !dark.is_empty() {
                    probe
                        .notes
                        .push(format!("screen off: {} (dpms)", dark.join(", ")));
                }
                probe.monitors = monitors;
            }
            Err(e) => probe.blocked.push(format!("cannot list the screens: {e}")),
        }
        for (tool, there) in probe.tools.clone() {
            if !there {
                probe.blocked.push(match tool.as_str() {
                    "grim" => "grim is missing: no screenshots".to_owned(),
                    "ydotool" => "ydotool is missing: no clicks".to_owned(),
                    _ => format!("{tool} is missing: no typing"),
                });
            }
        }
        // A locker that is running is a second reason to keep out, even
        // when the compositor has not blocked a monitor yet.
        for locker in ["hyprlock", "swaylock", "gtklock", "waylock"] {
            if pid_of(locker).is_some() {
                probe
                    .blocked
                    .push(format!("{locker} runs: the screen is locked"));
            }
        }
        // ydotoold has been seen to deliver pointer motion but neither
        // clicks nor keys after weeks of uptime, with nothing to see from
        // outside. Say its age rather than guess.
        if let Some(pid) = pid_of("ydotoold") {
            match running_for(pid) {
                Some(age) if age > DAEMON_OLD => probe.notes.push(format!(
                    "ydotoold has been running {} days — if clicks do not arrive: \
                     systemctl --user restart ydotool",
                    age.as_secs() / 86_400
                )),
                Some(age) => probe
                    .notes
                    .push(format!("ydotoold running for {}h", age.as_secs() / 3600)),
                None => {}
            }
        } else {
            probe
                .blocked
                .push("ydotoold does not run: systemctl --user start ydotool".to_owned());
        }
        probe
    }
}

#[cfg(target_os = "linux")]
impl Desktop for Hyprland {
    fn name(&self) -> &'static str {
        "hyprland"
    }
}

/// Whether the session is locked, out of Hyprland's `j/monitors`.
///
/// `LOCK` in `solitaryBlockedBy` is how Omarchy's own
/// `omarchy-hyprland-session-locked` decides it, and the reason **stays
/// standing when the lock client dies** — a session that is locked with
/// nobody to unlock it. Pointer and keys reach nothing reliable there,
/// and a screenshot would be the lock screen with whatever was typed
/// into it. So: locked means nothing happens.
fn session_locked(monitors: &Value) -> bool {
    monitors
        .as_array()
        .map(|ms| {
            ms.iter().any(|m| {
                m["solitaryBlockedBy"]
                    .as_array()
                    .is_some_and(|why| why.iter().any(|w| w == "LOCK"))
            })
        })
        .unwrap_or(false)
}

/// Whether `program` is in the PATH.
fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let file = dir.join(program);
                file.is_file() && std::fs::metadata(&file).is_ok()
            })
        })
        .unwrap_or(false)
}

/// The pid of a process named exactly `name`, as `pgrep -x` finds it, read
/// straight from /proc so no tool has to be there.
fn pid_of(name: &str) -> Option<u32> {
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if comm.trim() == name {
            return Some(pid);
        }
    }
    None
}

/// How long process `pid` has been running.
fn running_for(pid: u32) -> Option<Duration> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22 (1-based) is the start time in clock ticks since boot; the
    // command in field 2 may hold spaces, so count from its closing brace.
    let after = stat.rsplit_once(')')?.1;
    let ticks: u64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let hz = 100; // CLOCK_TCK is 100 on every Linux imkr runs on.
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let up: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let started = ticks as f64 / hz as f64;
    Some(Duration::from_secs_f64((up - started).max(0.0)))
}

// ---------------------------------------------------------------------------
// Doing something
// ---------------------------------------------------------------------------

/// The screens there are right now, by name, for the settings menu. An
/// empty list where no backend answers — a headless screen made for
/// agents comes and goes, and so does a real one that is switched off.
pub fn monitor_names() -> Vec<String> {
    desktop()
        .and_then(|desk| desk.monitors())
        .map(|ms| ms.into_iter().map(|m| m.name).collect())
        .unwrap_or_default()
}

/// Which screen a command means: `--monitor NAME` for that one, `all` for
/// every screen at once, else the one the settings name (a headless
/// screen for agents, so the real one may be off), else the focused one.
///
/// Resolved against the screens that are there **now**, never from
/// memory: a headless screen goes when the session restarts, and a real
/// one goes when it is unplugged.
pub fn pick_monitor(
    monitors: &[Monitor],
    asked: Option<&str>,
    settled: Option<&str>,
) -> Result<Option<String>> {
    let names = || {
        monitors
            .iter()
            .map(|m| m.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(name) = asked {
        if name == "all" {
            return Ok(None);
        }
        if !monitors.iter().any(|m| m.name == name) {
            bail!("no screen named {name} here: {}", names());
        }
        return Ok(Some(name.to_owned()));
    }
    // The settings' screen, but only while it is there: silently taking
    // another one would put a click on the wrong desktop.
    if let Some(name) = settled.filter(|n| !n.is_empty()) {
        if monitors.iter().any(|m| m.name == name) {
            return Ok(Some(name.to_owned()));
        }
        bail!(
            "the screen for agents, {name}, is not there: {}.              `hyprctl output create headless {name}` makes it, or pick another one              in the settings (\"Screen agents see\").",
            names()
        );
    }
    Ok(monitors
        .iter()
        .find(|m| m.focused)
        .or_else(|| monitors.iter().find(|m| m.on))
        .or_else(|| monitors.first())
        .map(|m| m.name.clone()))
}

/// A screenshot of `monitor` (every screen with `None`), scaled down to
/// about `width` pixels across, written below `dir`.
pub fn shot(
    desk: &dyn Desktop,
    monitor: Option<&str>,
    region: Option<Region>,
    width: u32,
    to: Option<PathBuf>,
) -> Result<Shot> {
    let monitors = desk.monitors()?;
    if monitors.is_empty() {
        bail!("no screen to look at");
    }
    let shown: Vec<Monitor> = match monitor {
        Some(name) => monitors
            .iter()
            .filter(|m| m.name == name)
            .cloned()
            .collect(),
        None => monitors.clone(),
    };
    if shown.is_empty() {
        bail!(
            "no screen named {}: {}",
            monitor.unwrap_or_default(),
            monitors
                .iter()
                .map(|m| m.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // The picture in logical pixels: the region asked for, one screen,
    // or the box around all of them.
    let (left, top, right, bottom) = match region {
        Some(r) => (r.x, r.y, r.x + r.width as i32, r.y + r.height as i32),
        None => (
            shown.iter().map(|m| m.x).min().unwrap_or(0),
            shown.iter().map(|m| m.y).min().unwrap_or(0),
            shown
                .iter()
                .map(|m| m.x + m.width as i32)
                .max()
                .unwrap_or(0),
            shown
                .iter()
                .map(|m| m.y + m.height as i32)
                .max()
                .unwrap_or(0),
        ),
    };
    let whole = (right - left).max(1) as f64;
    let scale = (f64::from(width.max(16)) / whole).min(1.0);
    let path = match to {
        Some(path) => path,
        None => {
            let dir = shot_dir()?;
            std::fs::create_dir_all(&dir)?;
            prune(&dir);
            dir.join(format!("shot-{}.png", now()))
        }
    };
    desk.shot(monitor, region, scale, &path)?;
    let (image_width, image_height) = png_size(&path).unwrap_or((
        (whole * scale).round() as u32,
        ((bottom - top).max(1) as f64 * scale).round() as u32,
    ));
    Ok(Shot {
        path,
        width: image_width,
        height: image_height,
        // What grim really did, measured off the file: with one screen the
        // asked-for scale and the picture agree to the pixel, but a
        // rounded size would shift a click by a pixel or two at the far
        // edge of a wide layout.
        scale: f64::from(image_width) / whole,
        origin: (left, top),
        monitors: shown,
    })
}

/// Where a coordinate pair really points: global as the compositor
/// counts, or, with a screen named, relative to that screen's top left
/// corner — which is what an agent reads off a screenshot of one screen.
pub fn place(monitors: &[Monitor], on: Option<&str>, x: i32, y: i32) -> Result<(i32, i32)> {
    let Some(name) = on else {
        return Ok((x, y));
    };
    let monitor = monitors
        .iter()
        .find(|m| m.name == name)
        .with_context(|| format!("no screen named {name} here"))?;
    Ok((monitor.x + x, monitor.y + y))
}

/// Moves the pointer there and reads back where it ended up: a move that
/// silently did nothing would otherwise make the next click land wherever
/// the pointer happened to be.
pub fn move_to(desk: &dyn Desktop, x: i32, y: i32) -> Result<(i32, i32)> {
    let monitors = desk.monitors()?;
    if !monitors.is_empty() && !monitors.iter().any(|m| m.contains(x, y)) {
        bail!(
            "{x},{y} is on no screen: {}",
            monitors
                .iter()
                .map(|m| format!("{} {},{} {}x{}", m.name, m.x, m.y, m.width, m.height))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    desk.move_to(x, y)?;
    let got = desk.cursor()?;
    if got != (x, y) {
        bail!("the pointer went to {},{} instead of {x},{y}", got.0, got.1);
    }
    Ok(got)
}

// ---------------------------------------------------------------------------
// May an agent do this at all
// ---------------------------------------------------------------------------

/// What `computer_use` in the config allows on this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComputerUse {
    /// Nobody sees or drives this screen. The default.
    #[default]
    Off,
    /// imkr asks here, visibly, before the first action, and the answer
    /// holds for half an hour.
    Ask,
    /// Always allowed, for a machine nobody sits at.
    On,
}

impl ComputerUse {
    pub fn word(self) -> &'static str {
        match self {
            ComputerUse::Off => "off",
            ComputerUse::Ask => "ask",
            ComputerUse::On => "on",
        }
    }
}

/// The permission as it stands on this machine: who asked, until when it
/// holds, and when something last happened — written by the command that
/// asks, read by the server for the bar.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Grant {
    /// Until when the permission holds, seconds since 1970.
    #[serde(default)]
    pub until: u64,
    /// Who it was given to, as an address.
    #[serde(default)]
    pub who: String,
    /// A request waiting for an answer here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking: Option<Asking>,
    /// When an action last happened, so the bar can say it is happening.
    #[serde(default)]
    pub acted: u64,
    /// What that action was.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
}

/// An agent waiting for the user to allow it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asking {
    pub who: String,
    /// `see` or `drive`: looking is less than typing.
    pub what: String,
    /// When it asked.
    pub since: u64,
    /// `None` while nobody has answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<bool>,
}

impl Grant {
    /// Whether the permission holds now.
    pub fn allowed(&self) -> bool {
        self.until > now()
    }

    /// How much longer it holds.
    pub fn left(&self) -> Option<Duration> {
        self.until
            .checked_sub(now())
            .filter(|s| *s > 0)
            .map(Duration::from_secs)
    }

    /// Whether something is happening right now (for the bar).
    pub fn busy(&self) -> bool {
        now().saturating_sub(self.acted) < BUSY_FOR.as_secs()
    }
}

fn state(name: &str) -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join(name))
}

fn shot_dir() -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("screenshots"))
}

/// Screenshots older than a day go: they are pictures of the user's
/// screen, and nothing reads them twice.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| m.elapsed().unwrap_or_default() > Duration::from_secs(24 * 3600))
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Everything that touches the permission file holds this lock: the
/// command that asks and the server that answers are two processes.
fn locked<T>(f: impl FnOnce() -> T) -> Result<T> {
    use std::os::fd::AsRawFd;
    let lock = state("computer-use.lock")?;
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

fn read_grant() -> Grant {
    let Ok(path) = state("computer-use.json") else {
        return Grant::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_grant(grant: &Grant) -> Result<()> {
    let path = state("computer-use.json")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(grant)?)?;
    // It says who may drive this machine's screen: nobody else's business.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// The permission as it stands, without changing anything. For the bar and
/// for `status`.
pub fn grant() -> Grant {
    read_grant()
}

/// Gives the permission for `GRANT_FOR`, answering whoever asked.
pub fn allow(who: &str) -> Result<Grant> {
    locked(|| {
        let mut grant = read_grant();
        grant.until = now() + GRANT_FOR.as_secs();
        grant.who = who.to_owned();
        if let Some(asking) = &mut grant.asking {
            asking.answer = Some(true);
        }
        write_grant(&grant)?;
        log(
            "allowed",
            who,
            &format!("{} minutes", GRANT_FOR.as_secs() / 60),
        );
        Ok(grant)
    })?
}

/// Refuses whoever asked, and ends any permission that holds.
pub fn refuse(who: &str) -> Result<()> {
    locked(|| {
        let mut grant = read_grant();
        grant.until = 0;
        if let Some(asking) = &mut grant.asking {
            asking.answer = Some(false);
        }
        write_grant(&grant)?;
        log("refused", who, "");
        Ok(())
    })?
}

/// Writes down that an agent wants to see or drive the screen, for the
/// server to ask about. Replaces an older request.
pub fn ask(who: &str, what: &str) -> Result<()> {
    locked(|| {
        let mut grant = read_grant();
        grant.asking = Some(Asking {
            who: who.to_owned(),
            what: what.to_owned(),
            since: now(),
            answer: None,
        });
        write_grant(&grant)?;
        log("asked", who, what);
        Ok(())
    })?
}

/// Notes that an action happened, for the bar and the log.
pub fn acted(who: &str, action: &str, detail: &str) {
    let _ = locked(|| {
        let mut grant = read_grant();
        grant.acted = now();
        grant.action = action.to_owned();
        let _ = write_grant(&grant);
    });
    log(action, who, detail);
}

/// Every action, in `<state>/screen.log`: when, who, what. Appended, never
/// rewritten, so nothing a session did can be taken back out.
pub fn log(action: &str, who: &str, detail: &str) {
    let Ok(path) = state("screen.log") else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let line = format!(
        "{} {} {}{}\n",
        stamp(),
        if who.is_empty() { "-" } else { who },
        action,
        if detail.is_empty() {
            String::new()
        } else {
            format!(" {detail}")
        }
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

pub fn log_path() -> Result<PathBuf> {
    state("screen.log")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The time as `2026-10-29 15:45:02`, in the reader's own time zone.
///
/// Local, not UTC: this log is read by the person whose screen it is,
/// next to "what was I doing at a quarter to four" — and two hours off
/// in summer makes every line look like somebody else's.
fn stamp() -> String {
    let t = now() as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return now().to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// A PNG's size, out of its own header: 8 bytes of signature, then an
/// IHDR chunk whose first two fields are width and height.
fn png_size(path: &Path) -> Option<(u32, u32)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((read(16), read(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_modifiers_and_keysyms() {
        let key = Key::parse("ctrl+c").unwrap();
        assert_eq!(key.mods, ["ctrl"]);
        assert_eq!(key.keysym, "c");
        assert_eq!(Key::parse("enter").unwrap().keysym, "Return");
        assert_eq!(Key::parse("ESC").unwrap().keysym, "Escape");
        assert_eq!(Key::parse("f5").unwrap().keysym, "F5");
        assert_eq!(Key::parse("pgdn").unwrap().keysym, "Next");
        let combo = Key::parse("super+shift+Left").unwrap();
        assert_eq!(combo.mods, ["logo", "shift"]);
        assert_eq!(combo.keysym, "Left");
        // A name libxkbcommon knows and we do not is passed on unchanged.
        assert_eq!(Key::parse("Menu").unwrap().keysym, "Menu");
        // A lone plus is the key, not a separator.
        assert_eq!(Key::parse("+").unwrap().keysym, "+");
        assert!(Key::parse("hyper+a").is_err());
        assert!(Key::parse("  ").is_err());
    }

    #[test]
    fn a_shot_says_how_its_pixels_map_back() {
        let shot = Shot {
            path: PathBuf::from("/tmp/x.png"),
            width: 768,
            height: 512,
            scale: 0.25,
            origin: (0, 0),
            monitors: Vec::new(),
        };
        // The middle of a 768x512 picture of a 3072x2048 logical screen.
        assert_eq!(shot.logical(384.0, 256.0), (1536, 1024));
        assert!(shot.hint().contains("0.2500"));
        // A second screen to the right keeps its offset.
        let beside = Shot {
            origin: (3072, 0),
            ..shot
        };
        assert_eq!(beside.logical(0.0, 0.0), (3072, 0));
        assert_eq!(beside.logical(40.0, 20.0), (3232, 80));
    }

    fn two_screens() -> Vec<Monitor> {
        vec![
            Monitor {
                name: "HDMI-A-1".into(),
                x: 0,
                y: 0,
                width: 3072,
                height: 2048,
                scale: 1.25,
                on: true,
                focused: true,
            },
            // A headless screen for agents, beside the real one.
            Monitor {
                name: "AGENT".into(),
                x: 3072,
                y: 0,
                width: 1536,
                height: 864,
                scale: 1.25,
                on: true,
                focused: false,
            },
        ]
    }

    #[test]
    fn the_screen_is_picked_from_the_ones_that_are_there() {
        let all = two_screens();
        // Asked for by name, and `all` for the whole layout.
        assert_eq!(
            pick_monitor(&all, Some("AGENT"), None).unwrap().as_deref(),
            Some("AGENT")
        );
        assert_eq!(pick_monitor(&all, Some("all"), None).unwrap(), None);
        // The settings' screen wins over the focused one …
        assert_eq!(
            pick_monitor(&all, None, Some("AGENT")).unwrap().as_deref(),
            Some("AGENT")
        );
        // … but only while it is there: a headless screen goes with the
        // session, and clicking on the real one instead would be wrong.
        let real_only = vec![all[0].clone()];
        let gone = pick_monitor(&real_only, None, Some("AGENT")).unwrap_err();
        assert!(format!("{gone}").contains("hyprctl output create headless AGENT"));
        // Nothing settled: the focused screen, and the real one can go too.
        assert_eq!(
            pick_monitor(&all, None, None).unwrap().as_deref(),
            Some("HDMI-A-1")
        );
        let agent_only = vec![all[1].clone()];
        assert_eq!(
            pick_monitor(&agent_only, None, None).unwrap().as_deref(),
            Some("AGENT")
        );
        assert!(pick_monitor(&all, Some("VGA-9"), None).is_err());
        assert_eq!(pick_monitor(&[], None, None).unwrap(), None);
    }

    #[test]
    fn coordinates_on_a_screen_get_its_corner_added() {
        let all = two_screens();
        assert_eq!(place(&all, None, 40, 20).unwrap(), (40, 20));
        assert_eq!(place(&all, Some("HDMI-A-1"), 40, 20).unwrap(), (40, 20));
        // The same pixel read off a shot of the second screen.
        assert_eq!(place(&all, Some("AGENT"), 40, 20).unwrap(), (3112, 20));
        assert!(place(&all, Some("VGA-9"), 0, 0).is_err());
    }

    /// `j/monitors` as Hyprland really answers it, with the shape a
    /// locked session leaves behind (measured 28./29.10.2026).
    const MONITORS: &str = r#"[
        {"name":"HDMI-A-1","x":0,"y":0,"width":3840,"height":2560,"scale":1.25,
         "dpmsStatus":true,"focused":true,
         "solitaryBlockedBy":["WINDOWED","CANDIDATE"]},
        {"name":"AGENT","x":3072,"y":0,"width":2560,"height":1440,"scale":1.0,
         "dpmsStatus":true,"focused":false,
         "solitaryBlockedBy":["WINDOWED","CANDIDATE"]}
    ]"#;

    /// A lock client that died leaves `LOCK` standing with nobody to
    /// unlock: that is the state to refuse, and it looks like this.
    const LOCKED: &str = r#"[
        {"name":"HDMI-A-1","x":0,"y":0,"width":3840,"height":2560,"scale":1.25,
         "dpmsStatus":false,"focused":true,
         "solitaryBlockedBy":["LOCK","WINDOWED","CANDIDATE"]}
    ]"#;

    #[test]
    fn a_locked_session_is_read_off_the_compositors_own_answer() {
        let open: Value = serde_json::from_str(MONITORS).unwrap();
        let shut: Value = serde_json::from_str(LOCKED).unwrap();
        assert!(!session_locked(&open));
        assert!(session_locked(&shut));
        // Nothing to go on is not the same as locked — that case is
        // blocked by the unanswered request, with its own reason.
        assert!(!session_locked(&serde_json::json!([])));
        assert!(!session_locked(&serde_json::json!(null)));
    }

    /// Serves canned answers on a socket of its own, so the whole
    /// Hyprland path can be measured without a compositor — and without
    /// locking anybody's screen to see the refusal.
    #[cfg(target_os = "linux")]
    fn fake_hyprland(monitors: &'static str) -> (Hyprland, std::path::PathBuf) {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!(
            "imkr-screen-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hypr.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let Ok(mut stream) = stream else { continue };
                let mut asked = [0u8; 256];
                let read = stream.read(&mut asked).unwrap_or(0);
                let asked = String::from_utf8_lossy(&asked[..read]).into_owned();
                let answer = match asked.trim() {
                    "j/monitors" => monitors.to_owned(),
                    "cursorpos" => "1500, 900".to_owned(),
                    request if request.starts_with("dispatch") => "ok".to_owned(),
                    other => format!("unknown request {other}"),
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        (Hyprland::at(path), dir)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_hyprland_backend_reads_logical_sizes_and_moves_the_cursor() {
        let (hypr, dir) = fake_hyprland(MONITORS);
        let monitors = hypr.monitors().unwrap();
        // The trap: width/height come in DEVICE pixels, x/y are logical.
        // 3840 / 1.25 = 3072, and the screen beside it starts right there.
        assert_eq!(
            (monitors[0].width, monitors[0].height),
            (3072, 2048),
            "device pixels were not divided by the scale"
        );
        assert_eq!(monitors[1].x, 3072);
        assert_eq!((monitors[1].width, monitors[1].height), (2560, 1440));
        assert!(monitors[0].focused && monitors[0].on);
        assert_eq!(hypr.cursor().unwrap(), (1500, 900));
        assert!(hypr.move_to(400, 300).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The point of the whole pre-flight check: a locked machine is
    /// turned down, and the reason says so in words.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_locked_machine_is_turned_down() {
        let (hypr, dir) = fake_hyprland(LOCKED);
        let probe = hypr.probe();
        assert!(!probe.ok(), "a locked session was let through");
        assert!(
            probe.blocked.iter().any(|why| why.contains("locked")),
            "{:?}",
            probe.blocked
        );
        // And the dark screen is its own reason, not folded into the lock.
        assert!(
            probe.blocked.iter().any(|why| why.contains("screen")),
            "{:?}",
            probe.blocked
        );
        let (open, dir2) = fake_hyprland(MONITORS);
        let probe = open.probe();
        assert!(
            !probe.blocked.iter().any(|why| why.contains("locked")),
            "{:?}",
            probe.blocked
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[test]
    fn a_region_is_a_corner_and_a_size() {
        let r = Region::parse("3072,0,1280x800").unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (3072, 0, 1280, 800));
        assert_eq!(r.geometry(), "3072,0 1280x800");
        assert_eq!(Region::parse("-20,-10,4x4").unwrap().x, -20);
        for bad in ["3072,0", "3072,0,1280", "a,0,1x1", "0,0,0x10", "1,2,3x4,5"] {
            assert!(Region::parse(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn a_grant_runs_out() {
        let fresh = Grant {
            until: now() + 60,
            who: "a".into(),
            ..Grant::default()
        };
        assert!(fresh.allowed() && fresh.left().is_some());
        let stale = Grant {
            until: now() - 1,
            ..Grant::default()
        };
        assert!(!stale.allowed() && stale.left().is_none());
        assert!(!stale.busy());
        let busy = Grant {
            acted: now(),
            ..Grant::default()
        };
        assert!(busy.busy());
    }

    #[test]
    fn a_monitor_holds_only_its_own_coordinates() {
        let m = Monitor {
            name: "HDMI-A-1".into(),
            x: 0,
            y: 0,
            width: 3072,
            height: 2048,
            scale: 1.25,
            on: true,
            focused: true,
        };
        assert!(m.contains(0, 0) && m.contains(3071, 2047));
        assert!(!m.contains(3072, 0) && !m.contains(-1, 5));
    }

    /// The word shown in the settings menu and the word in the config
    /// file are the same word — two spellings would make the menu lie
    /// about what is set.
    #[test]
    fn the_settings_word_is_the_one_in_the_config() {
        for use_it in [ComputerUse::Off, ComputerUse::Ask, ComputerUse::On] {
            let written = toml::to_string(&serde_json::json!({ "computer_use": use_it })).unwrap();
            assert!(
                written.contains(&format!("\"{}\"", use_it.word())),
                "{} is written as {written}",
                use_it.word()
            );
        }
        // And the default is the one that does nothing.
        assert_eq!(ComputerUse::default(), ComputerUse::Off);
    }

    /// The gap on macOS and Windows is a sentence, not a surprise — and
    /// it names the way in, so nobody has to research it again.
    #[test]
    fn the_unsupported_systems_say_what_would_be_needed() {
        for said in [UNSUPPORTED_MACOS, UNSUPPORTED_WINDOWS] {
            assert!(said.contains("xcap") && said.contains("enigo"), "{said}");
            assert!(said.contains("does not support"), "{said}");
        }
        assert!(UNSUPPORTED_MACOS.contains("Screen Recording"));
    }

    #[test]
    fn a_stamp_is_a_readable_local_time() {
        let said = stamp();
        assert_eq!(said.len(), 19, "{said}");
        assert!(said.starts_with("20"), "{said}");
        // Local, not UTC: the hour must match what the C library says
        // for this machine, whatever its zone.
        let t = now() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        assert!(!unsafe { libc::localtime_r(&t, &mut tm) }.is_null());
        assert_eq!(&said[11..13], &format!("{:02}", tm.tm_hour), "{said}");
    }
}
