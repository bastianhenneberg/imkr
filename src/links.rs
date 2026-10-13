//! Links in pane output: hyperlinks programs mark with OSC 8, and URLs
//! that are plain text. `Ctrl`+click opens one on the user's machine.

/// What `LinkScanner` splits pane output into.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    /// Output for the terminal, without the sequences taken out.
    Text(Vec<u8>),
    /// A hyperlink to this URL starts at the cursor.
    Start(String),
    /// The hyperlink ends at the cursor.
    End,
    /// A shell integration mark (OSC 133) at the cursor: `A` prompt, `B`
    /// command typed, `C` its output starts, `D` it ended, with its exit
    /// code if the shell said.
    Mark(char, Option<i32>),
    /// The agent in a pane shown from another machine and its state, from
    /// imkr there (`ESC ] 7377 ; agent=NAME;state=STATE`), empty when none.
    Remote { agent: String, state: String },
}

/// Finds hyperlinks, `ESC ] 8 ; params ; URI`, and shell marks,
/// `ESC ] 133 ; X [; exit]`, ended by BEL or `ESC \`, in pane output, also
/// across reads.
#[derive(Default)]
pub struct LinkScanner {
    /// The start of a sequence that the last read cut off.
    pending: Vec<u8>,
}

const OSC: &[u8] = b"\x1b]";
/// What follows `OSC` in the sequences taken out.
const KINDS: [&[u8]; 3] = [b"8;", b"133;", REMOTE];
/// imkr's own: what runs in a pane shown elsewhere, see `remote_state`.
const REMOTE: &[u8] = b"7377;";
/// Longer is no link but garbage, passed on as text.
const MAX_SEQUENCE: usize = 8 * 1024;

impl LinkScanner {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Piece> {
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(bytes);
        let mut pieces = Vec::new();
        let mut text = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let Some(at) = find(&data[i..], OSC) else {
                // A sequence may begin with the last byte.
                let rest = &data[i..];
                let keep = usize::from(rest.ends_with(b"\x1b"));
                text.extend_from_slice(&rest[..rest.len() - keep]);
                self.pending = rest[rest.len() - keep..].to_vec();
                break;
            };
            let start = i + at;
            let after = &data[start + OSC.len()..];
            let kind = KINDS.iter().find(|k| after.starts_with(k));
            // Cut off before it can be told what it is: wait for more.
            if kind.is_none() && KINDS.iter().any(|k| k.starts_with(after)) {
                text.extend_from_slice(&data[i..start]);
                self.pending = data[start..].to_vec();
                break;
            }
            let Some(kind) = kind else {
                // Another OSC, e.g. a title: the terminal's.
                text.extend_from_slice(&data[i..start + OSC.len()]);
                i = start + OSC.len();
                continue;
            };
            text.extend_from_slice(&data[i..start]);
            let body_start = start + OSC.len() + kind.len();
            let end = data[body_start..]
                .iter()
                .enumerate()
                .find_map(|(j, &b)| match b {
                    0x07 => Some((j, 1)),
                    0x1b if data.get(body_start + j + 1) == Some(&b'\\') => Some((j, 2)),
                    _ => None,
                });
            let Some((len, terminator)) = end else {
                if data.len() - start > MAX_SEQUENCE {
                    text.extend_from_slice(&data[start..]);
                } else {
                    self.pending = data[start..].to_vec();
                }
                break;
            };
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            let body = String::from_utf8_lossy(&data[body_start..body_start + len]);
            pieces.push(if *kind == REMOTE {
                let field = |name: &str| {
                    body.split(';')
                        .find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
                        .unwrap_or_default()
                        .to_owned()
                };
                Piece::Remote {
                    agent: field("agent"),
                    state: field("state"),
                }
            } else if *kind == b"8;" {
                let uri = body.split_once(';').map(|(_, uri)| uri).unwrap_or("");
                if uri.is_empty() {
                    Piece::End
                } else {
                    Piece::Start(uri.to_owned())
                }
            } else {
                let mut parts = body.split(';');
                let mark = parts.next().and_then(|m| m.chars().next()).unwrap_or('?');
                Piece::Mark(mark, parts.next().and_then(|code| code.parse().ok()))
            });
            i = body_start + len + terminator;
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        pieces
    }
}

/// The sequence that tells the terminal showing a pane elsewhere which
/// agent runs in it and its state; `None` clears it.
pub fn remote_state(agent: Option<(&str, &str)>) -> Vec<u8> {
    let (agent, state) = agent.unwrap_or_default();
    format!("\x1b]7377;agent={agent};state={state}\x07").into_bytes()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A hyperlink on the screen, from one cell to the one after its last, as
/// (absolute row, column).
#[derive(Clone, Debug)]
pub struct Link {
    pub url: String,
    pub from: (usize, u16),
    pub to: (usize, u16),
}

/// How many hyperlinks a pane remembers, the newest.
pub const KEPT: usize = 500;

/// The newest of `links` that covers cell `at`.
pub fn link_at(links: &[Link], at: (usize, u16)) -> Option<&str> {
    links
        .iter()
        .rev()
        .find(|link| link.from <= at && at < link.to)
        .map(|link| link.url.as_str())
}

/// Characters that end a plain URL.
fn ends_url(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '│' | '|')
}

/// The plain-text URL in `line` that covers character `col`, if any.
pub fn url_in(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() || ends_url(chars[col]) {
        return None;
    }
    let start = (0..col)
        .rev()
        .find(|&i| ends_url(chars[i]))
        .map_or(0, |i| i + 1);
    let end = (col..chars.len())
        .find(|&i| ends_url(chars[i]))
        .unwrap_or(chars.len());
    let word: String = chars[start..end].iter().collect();
    // The URL may follow other text in the word, e.g. `(see:https://…`.
    let from = ["https://", "http://", "file://", "mailto:", "www."]
        .iter()
        .filter_map(|scheme| word.find(scheme))
        .min()?;
    // A click before the URL starts is not on it.
    if col < start + word[..from].chars().count() {
        return None;
    }
    let mut url = word[from..].to_owned();
    // Punctuation after a URL in a sentence, and a closing bracket without
    // an opening one inside.
    loop {
        let last = url.chars().last()?;
        let unbalanced = matches!(last, ')' | ']' | '}')
            && url.matches(['(', '[', '{']).count() < url.matches([')', ']', '}']).count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?') || unbalanced {
            url.pop();
        } else {
            break;
        }
    }
    if url.starts_with("www.") {
        url.insert_str(0, "https://");
    }
    openable(&url).then_some(url)
}

/// Whether `url` may be opened: web and mail links and files, not every
/// scheme some program on the machine handles.
pub fn openable(url: &str) -> bool {
    let rest = ["https://", "http://", "file://", "mailto:"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme));
    rest.is_some_and(|rest| !rest.is_empty()) && !url.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_agents_are_taken_out() {
        let mut scanner = LinkScanner::default();
        let mut bytes = b"hi ".to_vec();
        bytes.extend(remote_state(Some(("claude", "working"))));
        bytes.extend(b"there");
        let pieces = scanner.feed(&bytes);
        assert!(matches!(&pieces[0], Piece::Text(t) if t == b"hi "));
        assert!(matches!(&pieces[1], Piece::Remote { agent, state }
            if agent == "claude" && state == "working"));
        assert!(matches!(&pieces[2], Piece::Text(t) if t == b"there"));
        let cleared = scanner.feed(&remote_state(None));
        assert!(matches!(&cleared[0], Piece::Remote { agent, .. } if agent.is_empty()));
    }

    #[test]
    fn splits_hyperlinks_out_of_the_output() {
        let mut scanner = LinkScanner::default();
        let pieces = scanner.feed(b"see \x1b]8;;https://example.com\x1b\\here\x1b]8;;\x1b\\ now");
        assert_eq!(
            pieces,
            [
                Piece::Text(b"see ".to_vec()),
                Piece::Start("https://example.com".into()),
                Piece::Text(b"here".to_vec()),
                Piece::End,
                Piece::Text(b" now".to_vec()),
            ]
        );
    }

    #[test]
    fn hyperlinks_may_span_reads() {
        let mut scanner = LinkScanner::default();
        assert_eq!(scanner.feed(b"a\x1b]"), [Piece::Text(b"a".to_vec())]);
        assert_eq!(scanner.feed(b"8;id=1;https://x.org"), []);
        assert_eq!(
            scanner.feed(b"\x07b"),
            [
                Piece::Start("https://x.org".into()),
                Piece::Text(b"b".to_vec())
            ]
        );
        // Other escape codes pass untouched.
        assert_eq!(
            scanner.feed(b"\x1b]0;title\x07"),
            [Piece::Text(b"\x1b]0;title\x07".to_vec())]
        );
    }

    #[test]
    fn takes_out_shell_marks() {
        let mut scanner = LinkScanner::default();
        let pieces = scanner.feed(b"$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a b\r\n\x1b]13");
        assert_eq!(
            pieces,
            [
                Piece::Text(b"$ ".to_vec()),
                Piece::Mark('B', None),
                Piece::Text(b"ls\r\n".to_vec()),
                Piece::Mark('C', None),
                Piece::Text(b"a b\r\n".to_vec()),
            ]
        );
        assert_eq!(scanner.feed(b"3;D;2\x1b\\"), [Piece::Mark('D', Some(2))]);
    }

    #[test]
    fn finds_the_link_under_a_cell() {
        let links = [Link {
            url: "https://a".into(),
            from: (3, 4),
            to: (3, 8),
        }];
        assert_eq!(link_at(&links, (3, 4)), Some("https://a"));
        assert_eq!(link_at(&links, (3, 8)), None);
        assert_eq!(link_at(&links, (2, 5)), None);
    }

    #[test]
    fn finds_plain_urls() {
        let line = "Docs at https://example.com/a_(b). Done";
        assert_eq!(
            url_in(line, 12).as_deref(),
            Some("https://example.com/a_(b)")
        );
        assert_eq!(url_in(line, 2), None);
        assert_eq!(
            url_in("(see https://x.org/y)", 8).as_deref(),
            Some("https://x.org/y")
        );
        assert_eq!(
            url_in("go to www.rust-lang.org.", 8).as_deref(),
            Some("https://www.rust-lang.org")
        );
        assert_eq!(url_in("javascript:alert(1)", 3), None);
        assert!(!openable("ssh://host"));
    }
}
