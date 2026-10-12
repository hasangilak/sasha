//! Web links in a tab's output: finding the one under the mouse, and
//! opening it in the browser.

use std::process::{Command, Stdio};

/// A link on screen, from its first cell to its last (row, col), which may
/// be on different rows when the link wraps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub start: (u16, u16),
    pub end: (u16, u16),
}

impl Link {
    pub fn contains(&self, row: u16, col: u16) -> bool {
        (self.start..=self.end).contains(&(row, col))
    }
}

/// The http(s) link covering cell (row, col), if any.
pub fn at(screen: &vt100::Screen, row: u16, col: u16) -> Option<Link> {
    let (rows, cols) = screen.size();
    // A wrapped line is one line of text, however many rows it takes.
    let mut first = row;
    while first > 0 && screen.row_wrapped(first - 1) {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < rows && screen.row_wrapped(last) {
        last += 1;
    }
    let mut text = String::new();
    let mut cells = Vec::new();
    for r in first..=last {
        for c in 0..cols {
            let Some(cell) = screen.cell(r, c) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let contents = cell.contents();
            for ch in if contents.is_empty() { " " } else { contents }.chars() {
                text.push(ch);
                cells.push((r, c));
            }
        }
    }
    let clicked = cells.iter().position(|&cell| cell == (row, col))?;
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = find_links(&chars).into_iter().find(|(s, e)| (*s..*e).contains(&clicked))?;
    Some(Link { url: chars[start..end].iter().collect(), start: cells[start], end: cells[end - 1] })
}

/// Spans (start, end) of the http(s) links in `chars`.
fn find_links(chars: &[char]) -> Vec<(usize, usize)> {
    let mut links = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
        if !(rest.starts_with("https://") || rest.starts_with("http://")) {
            i += 1;
            continue;
        }
        let mut end = i;
        while end < chars.len() && !chars[end].is_whitespace() && !"<>\"'`".contains(chars[end]) {
            end += 1;
        }
        // Punctuation after a link belongs to the sentence, as does a
        // closing bracket the link didn't open.
        while end > i {
            let last = chars[end - 1];
            let opened = |open: char| chars[i..end].contains(&open);
            let unbalanced = (last == ')' && !opened('(')) || (last == ']' && !opened('['));
            if ".,;:!?".contains(last) || unbalanced {
                end -= 1;
            } else {
                break;
            }
        }
        if end > i + "https://".len() {
            links.push((i, end));
        }
        i = end.max(i + 1);
    }
    links
}

/// Opens `url` in the default browser.
pub fn open(url: &str) {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let mut command = Command::new(program);
    command.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    std::thread::spawn(move || {
        let _ = command.status();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_links(&chars).into_iter().map(|(s, e)| chars[s..e].iter().collect()).collect()
    }

    #[test]
    fn finds_links_in_text() {
        assert_eq!(links("see https://ratatui.rs."), ["https://ratatui.rs"]);
        assert_eq!(links("(docs: http://a.io/x?y=1)"), ["http://a.io/x?y=1"]);
        assert_eq!(links("https://en.wikipedia.org/wiki/Tmux_(software)"), ["https://en.wikipedia.org/wiki/Tmux_(software)"]);
        assert_eq!(links("<https://a.io> and \"https://b.io\""), ["https://a.io", "https://b.io"]);
        assert!(links("no links, just https:// alone").is_empty());
    }

    #[test]
    fn finds_the_link_under_a_cell_even_when_wrapped() {
        let mut parser = vt100::Parser::new(3, 20, 0);
        parser.process(b"go to https://example.com/a/long/path now");
        let screen = parser.screen();
        let link = at(screen, 1, 3).expect("a link on the second row");
        assert_eq!(link.url, "https://example.com/a/long/path");
        assert_eq!((link.start, link.end), ((0, 6), (1, 16)));
        assert!(at(screen, 0, 2).is_none());
    }
}
