//! Drawing the panels over the terminal pane: pull requests with their
//! files and diffs, the ports the tabs listen on, and a file from the files
//! bar.

use std::time::Instant;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Clear;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Confirm, Loadable, OpenPr, Panel, PortsPanel, PrPanel};
use crate::files::printable;
use crate::github::Change;
use crate::highlight::{Kind, Span};
use crate::theme;
use crate::ui::popup_block;

pub fn render(frame: &mut Frame, app: &App) {
    let Some(panel) = app.panel() else { return };
    let area = app.layout.pane;
    if area.width < 24 || area.height < 6 {
        return;
    }
    let title = match panel {
        Panel::Prs(PrPanel { open: Some(open), .. }) => format!(" #{} {} ", open.number, open.title),
        Panel::Prs(panel) => {
            let repo = panel.dir.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            format!(" pull requests · {repo} ")
        }
        Panel::Ports(_) => " ports open in your tabs ".to_owned(),
    };
    frame.render_widget(Clear, area);
    let block = popup_block(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let buf = frame.buffer_mut();
    let body = body(area);
    let hint_y = inner.bottom().saturating_sub(1);
    let p = theme::palette();
    let (hint, hint_color) = match panel {
        Panel::Prs(PrPanel { open: Some(open), .. }) => {
            render_files(buf, body, open);
            ("↑↓ file · PgUp PgDn ^u ^d scroll · o browser · esc back".to_owned(), p.text_dim)
        }
        Panel::Prs(panel) => {
            render_prs(buf, body, app, panel);
            ("↑↓ choose · ⏎ files · o browser · r reload · esc close".to_owned(), p.text_dim)
        }
        Panel::Ports(panel) => {
            render_ports(buf, app, body, panel);
            let confirm = doomed(app).and_then(|pid| app.ports.ready()?.iter().find(|port| port.pid == pid));
            match confirm {
                Some(port) => (format!("press x again to force-kill {} ({})", port.command, port.pid), p.red),
                None => ("↑↓ choose · x force-kill · ⏎ go to tab · r refresh · esc close".to_owned(), p.text_dim),
            }
        }
    };
    buf.set_stringn(inner.x, hint_y, hint, inner.width as usize, Style::new().fg(hint_color).bg(p.popup_bg));
}

/// The file picked in the files bar, with line numbers.
pub fn render_preview(frame: &mut Frame, app: &App) {
    let Some(preview) = app.preview() else { return };
    let area = app.layout.pane;
    if area.width < 24 || area.height < 6 {
        return;
    }
    let root = &app.ws().path;
    let name = preview.path.strip_prefix(root).unwrap_or(&preview.path).display().to_string();
    let lines = preview.lines.as_ref().map_or(0, Vec::len);
    let lang = preview.lang.map_or_else(String::new, |lang| format!(" · {}", lang.name()));
    let title = match lines {
        0 => format!(" {name}{lang} "),
        1 => format!(" {name}{lang} · 1 line "),
        n => format!(" {name}{lang} · {n} lines "),
    };
    frame.render_widget(Clear, area);
    let block = popup_block(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let p = theme::palette();
    let buf = frame.buffer_mut();
    let close = app.layout.preview_close();
    buf.set_string(close.x, close.y, " × ", Style::new().fg(p.text).bg(p.popup_bg).add_modifier(Modifier::BOLD));
    let body = body(area);
    let hint = "↑↓ files · PgUp PgDn ^u ^d g G scroll · e edit · y copy path · esc";
    let hint_y = inner.bottom().saturating_sub(1);
    buf.set_stringn(inner.x, hint_y, hint, inner.width as usize, Style::new().fg(p.text_dim).bg(p.popup_bg));
    let text = match &preview.lines {
        Ok(text) => text,
        Err(why) => {
            buf.set_stringn(body.x, body.y, why, body.width as usize, Style::new().fg(p.text_dim).bg(p.popup_bg));
            return;
        }
    };
    if text.is_empty() {
        buf.set_string(body.x, body.y, "an empty file", Style::new().fg(p.text_dim).bg(p.popup_bg));
        return;
    }
    let gutter = text.len().to_string().len() as u16;
    let x = body.x + gutter + 2;
    let room = body.right().saturating_sub(x) as usize;
    for (row, (index, line)) in text.iter().enumerate().skip(preview.scroll).take(body.height as usize).enumerate() {
        let y = body.y + row as u16;
        let number = format!("{:>width$}", index + 1, width = gutter as usize);
        buf.set_string(body.x, y, number, Style::new().fg(p.border).bg(p.popup_bg));
        let spans = preview.spans.get(index).map_or(&[][..], Vec::as_slice);
        render_code(buf, x, y, room, line, spans);
    }
    let last_shown = preview.scroll + body.height as usize;
    if preview.truncated && last_shown >= text.len() {
        let note = "… only the first megabyte is shown";
        let y = body.y + (text.len() - preview.scroll).min(body.height as usize - 1) as u16;
        buf.set_stringn(x, y, note, room, Style::new().fg(p.text_dim).bg(p.popup_bg));
    }
}

/// A line of code, in its colours.
fn render_code(buf: &mut Buffer, x: u16, y: u16, room: usize, line: &str, spans: &[Span]) {
    let right = x.saturating_add(u16::try_from(room).unwrap_or(u16::MAX));
    // Plain stretches between the coloured ones.
    let mut pieces = Vec::with_capacity(spans.len() * 2 + 1);
    let mut at = 0;
    for span in spans {
        pieces.push((at, span.start, None));
        pieces.push((span.start, span.end, Some(span.kind)));
        at = span.end;
    }
    pieces.push((at, line.len(), None));
    let mut x = x;
    for (start, end, kind) in pieces {
        let Some(text) = line.get(start..end).filter(|text| !text.is_empty()) else { continue };
        if x >= right {
            break;
        }
        (x, _) = buf.set_stringn(x, y, text, usize::from(right - x), code_style(kind));
    }
}

fn code_style(kind: Option<Kind>) -> Style {
    let p = theme::palette();
    let style = Style::new().bg(p.popup_bg);
    let Some(kind) = kind else { return style.fg(p.text) };
    match kind {
        Kind::Comment => style.fg(p.text_dim).add_modifier(Modifier::ITALIC),
        Kind::String => style.fg(p.green),
        Kind::Number | Kind::Constant => style.fg(p.orange),
        Kind::Keyword => style.fg(p.purple),
        Kind::Type => style.fg(p.yellow),
        Kind::Function => style.fg(p.blue),
        Kind::Macro => style.fg(p.cyan),
        Kind::Key | Kind::Tag => style.fg(p.red),
        Kind::Variable => style.fg(p.pink),
        Kind::Heading => style.fg(p.blue).add_modifier(Modifier::BOLD),
    }
}

/// The program waiting for a second `x`, if any.
fn doomed(app: &App) -> Option<i32> {
    match app.pending.waiting(Instant::now()) {
        Some(Confirm::KillPort(pid)) => Some(pid),
        _ => None,
    }
}

/// The list's contents, or what to say instead.
fn ready_or_say<'a, T>(buf: &mut Buffer, area: Rect, data: &'a Loadable<Vec<T>>, waiting: &str, empty: &str) -> Option<&'a [T]> {
    let p = theme::palette();
    let (text, color) = match data {
        Loadable::Loading => (waiting.to_owned(), p.text_dim),
        Loadable::Failed(err) => (err.clone(), p.red),
        Loadable::Ready(items) if items.is_empty() => (empty.to_owned(), p.text_dim),
        Loadable::Ready(items) => return Some(items),
    };
    buf.set_stringn(area.x, area.y, text, area.width as usize, Style::new().fg(color).bg(p.popup_bg));
    None
}

/// Where a panel covering `pane` lists things: inside its border, above
/// the hint line.
pub fn body(pane: Rect) -> Rect {
    let inner = popup_block("").inner(pane);
    Rect { height: inner.height.saturating_sub(2), ..inner }
}

/// A pull request's file list and diff, side by side in `body`.
pub fn split(body: Rect) -> (Rect, Rect) {
    let list_width = (body.width / 3).clamp(20, 48).min(body.width.saturating_sub(10));
    let list = Rect { width: list_width, ..body };
    let diff = Rect { x: body.x + list_width + 1, width: body.width.saturating_sub(list_width + 1), ..body };
    (list, diff)
}

/// The first item to show so that `selected` is in view.
pub fn first_shown(selected: usize, rows: u16) -> usize {
    selected.saturating_sub(usize::from(rows).saturating_sub(1))
}

fn render_prs(buf: &mut Buffer, area: Rect, app: &App, panel: &PrPanel) {
    let p = theme::palette();
    let waiting = "asking GitHub…";
    let Some(prs) = ready_or_say(buf, area, app.prs_of(&panel.dir), waiting, "no open pull requests here") else {
        return;
    };
    let first = first_shown(panel.selected, area.height);
    for (i, pr) in prs.iter().enumerate().skip(first).take(area.height as usize) {
        let y = area.y + (i - first) as u16;
        let selected = i == panel.selected;
        let bg = if selected { p.active_bg } else { p.popup_bg };
        buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(bg));

        let number = format!("#{:<5}", pr.number);
        buf.set_string(area.x, y, &number, Style::new().fg(p.accent).bg(bg).add_modifier(Modifier::BOLD));
        let files = if pr.changed_files == 1 { "file" } else { "files" };
        let detail = format!(
            "{} · +{} −{} · {} {files}",
            pr.author.login, pr.additions, pr.deletions, pr.changed_files
        );
        let detail_width = (detail.width() as u16).min(area.width / 2);
        let mut x = area.x + number.width() as u16;
        if pr.draft {
            buf.set_string(x, y, "draft ", Style::new().fg(p.text_dim).bg(bg));
            x += 6;
        }
        let room = area.right().saturating_sub(x + detail_width + 2) as usize;
        let mut title = Style::new().fg(p.text).bg(bg);
        if selected {
            title = title.add_modifier(Modifier::BOLD);
        }
        buf.set_stringn(x, y, &pr.title, room, title);
        let detail_x = area.right() - detail_width;
        buf.set_stringn(detail_x, y, &detail, detail_width as usize, Style::new().fg(p.text_dim).bg(bg));
    }
}

/// The pull request's files on the left, the selected one's diff on the
/// right.
fn render_files(buf: &mut Buffer, area: Rect, open: &OpenPr) {
    let p = theme::palette();
    let Some(files) = ready_or_say(buf, area, &open.files, "loading the changes…", "no changes") else { return };
    let (list, diff) = split(area);
    for y in area.top()..area.bottom() {
        buf.set_string(list.right(), y, "│", Style::new().fg(p.border).bg(p.popup_bg));
    }

    let first = first_shown(open.selected, list.height);
    for (i, file) in files.iter().enumerate().skip(first).take(list.height as usize) {
        let y = list.y + (i - first) as u16;
        let selected = i == open.selected;
        let bg = if selected { p.active_bg } else { p.popup_bg };
        buf.set_style(Rect::new(list.x, y, list.width.saturating_sub(1), 1), Style::new().bg(bg));
        let color = match file.change {
            Change::Added => p.green,
            Change::Modified => p.yellow,
            Change::Deleted => p.red,
            Change::Renamed => p.purple,
        };
        buf.set_string(list.x, y, file.change.letter(), Style::new().fg(color).bg(bg).add_modifier(Modifier::BOLD));
        let counts = match (file.added, file.removed) {
            (0, 0) => String::new(),
            (a, 0) => format!("+{a}"),
            (0, r) => format!("−{r}"),
            (a, r) => format!("+{a} −{r}"),
        };
        let counts_x = list.right().saturating_sub(counts.width() as u16 + 1);
        buf.set_string(counts_x, y, &counts, Style::new().fg(p.text_dim).bg(bg));
        // The end of a long path matters most.
        let room = counts_x.saturating_sub(list.x + 3) as usize;
        let path = if file.path.width() > room {
            let skip = file.path.width() + 1 - room;
            format!("…{}", file.path.chars().skip(skip).collect::<String>())
        } else {
            file.path.clone()
        };
        let mut style = Style::new().fg(p.text).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        buf.set_stringn(list.x + 2, y, path, room, style);
    }

    let Some(file) = files.get(open.selected) else { return };
    let heading = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let bold = Style::new().fg(p.text).bg(p.popup_bg).add_modifier(Modifier::BOLD);
    buf.set_stringn(diff.x, diff.y, heading, diff.width as usize, bold);
    let rows = diff.height.saturating_sub(1) as usize;
    for (row, line) in file.lines.iter().skip(open.scroll).take(rows).enumerate() {
        let y = diff.y + 1 + row as u16;
        let color: Color = match line.as_bytes().first() {
            Some(b'+') => p.green,
            Some(b'-') => p.red,
            Some(b'@') => p.cyan,
            _ => p.text_dim,
        };
        buf.set_stringn(diff.x, y, printable(line), diff.width as usize, Style::new().fg(color).bg(p.popup_bg));
    }
}

fn render_ports(buf: &mut Buffer, app: &App, area: Rect, panel: &PortsPanel) {
    let p = theme::palette();
    let empty = "nothing in your tabs is listening on a port";
    let Some(ports) = ready_or_say(buf, area, &app.ports, "looking for ports…", empty) else { return };
    let first = first_shown(panel.selected, area.height);
    for (i, port) in ports.iter().enumerate().skip(first).take(area.height as usize) {
        let y = area.y + (i - first) as u16;
        let selected = i == panel.selected;
        let doomed = doomed(app) == Some(port.pid);
        let bg = if selected { p.active_bg } else { p.popup_bg };
        buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(bg));

        let number = format!(":{:<6}", port.port);
        let color = if doomed { p.red } else { p.accent };
        buf.set_string(area.x, y, &number, Style::new().fg(color).bg(bg).add_modifier(Modifier::BOLD));
        let mut what = format!("{} ({})", port.command, port.pid);
        if port.address != "*" {
            what.push_str(&format!(" on {}", port.address));
        }
        let x = area.x + number.width() as u16;
        let place = app.find_tab(port.tab).map(|(w, t)| {
            let ws = &app.workspaces[w];
            format!("{} · {}", ws.name, ws.tabs[t].title())
        });
        let place = place.unwrap_or_default();
        let place_width = (place.width() as u16).min(area.width / 2);
        let room = area.right().saturating_sub(x + place_width + 2) as usize;
        buf.set_stringn(x, y, &what, room, Style::new().fg(if doomed { p.red } else { p.text }).bg(bg));
        let place_x = area.right() - place_width;
        buf.set_stringn(place_x, y, &place, place_width as usize, Style::new().fg(p.text_dim).bg(bg));
    }
}

