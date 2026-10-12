//! Links in pane output: hyperlinks programs mark with OSC 8, and URLs
//! that are plain text. `Ctrl`+click opens one on the user's machine.

/// What `LinkScanner` splits pane output into.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    /// Output for the terminal, without the OSC 8 sequences.
    Text(Vec<u8>),
    /// A hyperlink to this URL starts at the cursor.
    Start(String),
    /// The hyperlink ends at the cursor.
    End,
}

/// Finds `ESC ] 8 ; params ; URI` (ended by BEL or `ESC \`) in pane output,
/// also across reads.
#[derive(Default)]
pub struct LinkScanner {
    /// The start of a sequence that the last read cut off.
    pending: Vec<u8>,
}

const START: &[u8] = b"\x1b]8;";
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
            let Some(at) = find(&data[i..], START) else {
                // A sequence may begin in the last bytes.
                let rest = &data[i..];
                let keep = (1..START.len())
                    .rev()
                    .find(|&n| rest.len() >= n && rest.ends_with(&START[..n]))
                    .unwrap_or(0);
                text.extend_from_slice(&rest[..rest.len() - keep]);
                self.pending = rest[rest.len() - keep..].to_vec();
                break;
            };
            text.extend_from_slice(&data[i..i + at]);
            let body_start = i + at + START.len();
            let end = data[body_start..]
                .iter()
                .enumerate()
                .find_map(|(j, &b)| match b {
                    0x07 => Some((j, 1)),
                    0x1b if data.get(body_start + j + 1) == Some(&b'\\') => Some((j, 2)),
                    _ => None,
                });
            let Some((len, terminator)) = end else {
                if data.len() - (i + at) > MAX_SEQUENCE {
                    text.extend_from_slice(&data[i + at..]);
                } else {
                    self.pending = data[i + at..].to_vec();
                }
                break;
            };
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            let body = String::from_utf8_lossy(&data[body_start..body_start + len]);
            let uri = body.split_once(';').map(|(_, uri)| uri).unwrap_or("");
            pieces.push(if uri.is_empty() {
                Piece::End
            } else {
                Piece::Start(uri.to_owned())
            });
            i = body_start + len + terminator;
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        pieces
    }
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
