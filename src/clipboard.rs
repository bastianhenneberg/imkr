//! Copies text to the system clipboard of the client's terminal with OSC 52.
//! This works over SSH too, and without any clipboard tool on the server.

/// The escape sequence that asks the terminal to put `text` into the
/// clipboard.
pub fn osc52(text: &str) -> Vec<u8> {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes())).into_bytes()
}

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 back to bytes; `None` for anything else.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut bits = 0u32;
    let mut count = 0;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        bits = bits << 6 | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

/// An image in this machine's clipboard, with its file ending (`png` …),
/// read with `wl-paste` under Wayland, else `xclip`. `None` when the
/// clipboard holds no image or no tool answers within two seconds.
pub fn local_image() -> Option<(Vec<u8>, &'static str)> {
    const KINDS: [(&str, &str); 4] = [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/webp", "webp"),
        ("image/gif", "gif"),
    ];
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    let types = if wayland {
        run_for(&["wl-paste", "--list-types"])?
    } else if x11 {
        run_for(&["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"])?
    } else {
        return None;
    };
    let types = String::from_utf8_lossy(&types);
    let (kind, ending) = KINDS
        .iter()
        .find(|(kind, _)| types.lines().any(|l| l.trim() == *kind))?;
    let bytes = if wayland {
        run_for(&["wl-paste", "--no-newline", "--type", kind])?
    } else {
        run_for(&["xclip", "-selection", "clipboard", "-t", kind, "-o"])?
    };
    (!bytes.is_empty()).then_some((bytes, *ending))
}

/// Runs `argv` and returns what it printed, if it succeeds within two
/// seconds: a clipboard tool must not hold up the keyboard.
fn run_for(argv: &[&str]) -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        let _ = tx.send(out);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(out) => {
            let status = child.wait().ok()?;
            status.success().then_some(out)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Picks the OSC 52 sequences, "put this into the clipboard", out of a
/// program's output, so they reach the client's terminal: a pane shows the
/// program through imkr, which would swallow them otherwise. That is what
/// copying in an imkr on another machine, reached by ssh in a pane, or in
/// nvim relies on. Sequences may span reads.
#[derive(Default)]
pub struct Osc52Scanner {
    /// The sequence being read, `ESC ] 52 ;` included.
    current: Option<Vec<u8>>,
    /// The end of the last read, if it could be the start of one.
    tail: Vec<u8>,
}

const START: &[u8] = b"\x1b]52;";
/// Large copies are fine, endless ones are not.
const MAX_SEQUENCE: usize = 8 * 1024 * 1024;

impl Osc52Scanner {
    /// The complete sequences in `bytes` and what came before, each ended
    /// with BEL. Requests to read the clipboard (`?`) are left out: a
    /// program in a pane does not get to see what the user copied.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut data = std::mem::take(&mut self.tail);
        data.extend_from_slice(bytes);
        let mut found = Vec::new();
        let mut i = 0;
        while i < data.len() {
            if let Some(current) = &mut self.current {
                let rest = &data[i..];
                // Ended by BEL or by ST, `ESC \`.
                let end = rest.iter().enumerate().find_map(|(j, &b)| match b {
                    0x07 => Some((j, 1)),
                    0x1b if rest.get(j + 1) == Some(&b'\\') => Some((j, 2)),
                    _ => None,
                });
                match end {
                    Some((j, len)) => {
                        current.extend_from_slice(&rest[..j]);
                        let mut sequence = self.current.take().expect("inside one");
                        if keep(&sequence) {
                            sequence.push(0x07);
                            found.push(sequence);
                        }
                        i += j + len;
                    }
                    None => {
                        // A lone ESC at the end may start ST.
                        let cut = if rest.ends_with(b"\x1b") {
                            rest.len() - 1
                        } else {
                            rest.len()
                        };
                        current.extend_from_slice(&rest[..cut]);
                        self.tail = rest[cut..].to_vec();
                        if current.len() > MAX_SEQUENCE {
                            self.current = None;
                        }
                        break;
                    }
                }
                continue;
            }
            match find(&data[i..], START) {
                Some(at) => {
                    self.current = Some(START.to_vec());
                    i += at + START.len();
                }
                None => {
                    // Hold back an end that may be the start of one.
                    let rest = &data[i..];
                    let keep = (1..START.len())
                        .rev()
                        .find(|&n| rest.ends_with(&START[..n]))
                        .unwrap_or(0);
                    self.tail = rest[rest.len() - keep..].to_vec();
                    break;
                }
            }
        }
        found
    }
}

/// Whether a sequence sets the clipboard, rather than asking for it.
fn keep(sequence: &[u8]) -> bool {
    let body = &sequence[START.len()..];
    let data = body
        .iter()
        .position(|&b| b == b';')
        .map_or(body, |at| &body[at + 1..]);
    data != b"?"
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Base64 for other modules' tests.
#[cfg(test)]
pub fn base64_for_tests(bytes: &[u8]) -> String {
    base64(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_from_programs_get_through_whole() {
        let mut scanner = Osc52Scanner::default();
        let found = scanner.feed(b"text\x1b]52;c;aGFsbG8=\x07more");
        assert_eq!(found, vec![b"\x1b]52;c;aGFsbG8=\x07".to_vec()]);
        // Ended by ST, and split across reads, the start included.
        assert!(scanner.feed(b"x\x1b]5").is_empty());
        assert!(scanner.feed(b"2;c;d2Vs").is_empty());
        assert!(scanner.feed(b"dA==\x1b").is_empty());
        assert_eq!(
            scanner.feed(b"\\ rest"),
            vec![b"\x1b]52;c;d2VsdA==\x07".to_vec()]
        );
    }

    #[test]
    fn asking_for_the_clipboard_is_not_passed_on() {
        let mut scanner = Osc52Scanner::default();
        assert!(scanner.feed(b"\x1b]52;c;?\x07").is_empty());
        assert!(scanner.feed(b"\x1b]52;?\x07").is_empty());
    }

    #[test]
    fn base64_matches_the_rfc_examples() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain}");
        }
    }

    #[test]
    fn osc52_wraps_the_text() {
        assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn base64_goes_both_ways() {
        for bytes in [&b""[..], b"f", b"fo", b"foo", b"\x89PNG\r\n\x1a\n\x00\xff"] {
            assert_eq!(unbase64(&base64(bytes)).unwrap(), bytes);
        }
        assert!(unbase64("a$b").is_none());
    }
}
