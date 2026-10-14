//! The slim bars: the ports the tabs listen on, left of the tab sidebar,
//! and right of the pane the workspace's files and the open pull requests
//! of the tab's repository.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::{App, Loadable};
use crate::files::Mark;
use crate::layout::Hit;
use crate::sidebar::{SPINNER, render_faded};
use crate::theme::{self, BG};

pub fn render(buf: &mut Buffer, app: &App) {
    render_ports(buf, app);
    render_files(buf, app);
    render_prs(buf, app);
}

/// A heading, with a rule under it, over `width` columns from `x`.
fn heading(buf: &mut Buffer, area: Rect, x: u16, width: u16, text: &str, hot: bool) {
    let p = theme::palette();
    let style = Style::new().fg(if hot { p.accent } else { p.text }).bg(BG).add_modifier(Modifier::BOLD);
    buf.set_stringn(x, area.y, text, width as usize, style);
    if area.height > 1 {
        buf.set_string(x, area.y + 1, "─".repeat(width as usize), Style::new().fg(p.border).bg(BG));
    }
}

/// `text` wrapped onto the rows from `y`, in `color`.
fn say(buf: &mut Buffer, area: Rect, x: u16, y: u16, width: u16, text: &str, color: ratatui::style::Color) {
    let mut line = String::new();
    let mut row = y;
    let mut flush = |line: &mut String, row: &mut u16| {
        if *row < area.bottom() && !line.is_empty() {
            buf.set_stringn(x, *row, line.as_str(), width as usize, Style::new().fg(color).bg(BG));
        }
        line.clear();
        *row += 1;
    };
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width as usize {
            flush(&mut line, &mut row);
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    flush(&mut line, &mut row);
}

fn render_ports(buf: &mut Buffer, app: &App) {
    let area = app.layout.ports_bar;
    if area.is_empty() {
        return;
    }
    let p = theme::palette();
    buf.set_style(area, Style::new().bg(BG).fg(p.text));
    let width = area.width.saturating_sub(2);
    heading(buf, area, area.x, width + 1, "ports", false);
    let first = area.y + 2;
    match &app.ports {
        Loadable::Ready(ports) if ports.is_empty() => say(buf, area, area.x + 1, first, width, "none", p.text_dim),
        Loadable::Ready(ports) => {
            for (i, port) in ports.iter().enumerate() {
                let y = first + i as u16;
                if y >= area.bottom() {
                    break;
                }
                let hovered = app.hover == Some(Hit::Port(i));
                let bg = if hovered { p.hover_bg } else { BG };
                buf.set_style(Rect::new(area.x, y, width + 1, 1), Style::new().bg(bg));
                let number = format!(":{}", port.port);
                buf.set_stringn(area.x + 1, y, &number, width as usize, Style::new().fg(p.accent).bg(bg));
                // Wide enough: the program too.
                let room = (width as usize).saturating_sub(number.len() + 1);
                if room > 2 {
                    let x = area.x + 2 + number.len() as u16;
                    render_faded(buf, x, y, &port.command, room, Style::new().fg(p.text_dim).bg(bg), bg);
                }
            }
        }
        Loadable::Loading => say(buf, area, area.x + 1, first, width, "…", p.text_dim),
        Loadable::Failed(_) => say(buf, area, area.x + 1, first, width, "lsof?", p.red),
    }
    // The edge between it and the tab sidebar, which drags to resize.
    let edge = if app.edge_hot(Hit::PortsDivider) { p.accent } else { p.border };
    for y in area.top()..area.bottom() {
        buf.set_string(area.right() - 1, y, "│", Style::new().fg(edge).bg(BG));
    }
}

fn render_prs(buf: &mut Buffer, app: &App) {
    let area = app.layout.pr_bar;
    if area.is_empty() {
        return;
    }
    let p = theme::palette();
    buf.set_style(area, Style::new().bg(BG).fg(p.text));
    let edge = if app.edge_hot(Hit::PrsDivider) { p.accent } else { p.border };
    for y in area.top()..area.bottom() {
        buf.set_string(area.x, y, "│", Style::new().fg(edge).bg(BG));
    }
    let x = area.x + 2;
    let width = area.width.saturating_sub(3);
    let hot = app.hover == Some(Hit::PrHeading);
    heading(buf, area, x, width, "pull requests", hot);
    let first = area.y + 2;
    let Some(dir) = app.active_repo() else {
        say(buf, area, x, first, width, "this tab isn't in a git repository", p.text_dim);
        return;
    };
    // The refresh button, spinning while it fetches; the heading clicks.
    let (glyph, color) = if app.prs_fetching(&dir) {
        (SPINNER[app.spinner_frame() % SPINNER.len()], p.accent)
    } else {
        ("↻", if hot { p.accent } else { p.text })
    };
    let button_x = (x + width).saturating_sub(1);
    buf.set_string(button_x, area.y, glyph, Style::new().fg(color).bg(BG).add_modifier(Modifier::BOLD));
    let prs = match app.prs_of(&dir) {
        Loadable::Loading => return say(buf, area, x, first, width, "asking GitHub…", p.text_dim),
        Loadable::Failed(err) => return say(buf, area, x, first, width, err, p.red),
        Loadable::Ready(prs) if prs.is_empty() => {
            return say(buf, area, x, first, width, "no open pull requests", p.text_dim);
        }
        Loadable::Ready(prs) => prs,
    };
    // How many, left of the refresh button.
    let count = prs.len().to_string();
    let count_x = button_x.saturating_sub(count.chars().count() as u16 + 1);
    buf.set_string(count_x, area.y, &count, Style::new().fg(p.text_dim).bg(BG));
    for (i, pr) in prs.iter().enumerate() {
        let y = first + i as u16;
        if y >= area.bottom() {
            break;
        }
        let hovered = app.hover == Some(Hit::Pr(i));
        let bg = if hovered { p.hover_bg } else { BG };
        buf.set_style(Rect::new(x - 1, y, width + 1, 1), Style::new().bg(bg));
        let number = format!("#{}", pr.number);
        buf.set_string(x, y, &number, Style::new().fg(p.accent).bg(bg));
        let title_x = x + number.chars().count() as u16 + 1;
        let room = (x + width).saturating_sub(title_x) as usize;
        let color = if pr.draft { p.text_dim } else { p.text };
        render_faded(buf, title_x, y, &pr.title, room, Style::new().fg(color).bg(bg), bg);
    }
}

fn render_files(buf: &mut Buffer, app: &App) {
    let area = app.layout.files_bar;
    if area.is_empty() {
        return;
    }
    let p = theme::palette();
    buf.set_style(area, Style::new().bg(BG).fg(p.text));
    let edge = if app.edge_hot(Hit::FilesDivider) { p.accent } else { p.border };
    for y in area.top()..area.bottom() {
        buf.set_string(area.x, y, "│", Style::new().fg(edge).bg(BG));
    }
    let x = area.x + 2;
    let width = area.width.saturating_sub(3);
    let focused = app.files_focused();
    heading(buf, area, x, width, &format!("files · {}", app.ws().name), focused);
    // The × that hides the bar, over the end of the heading.
    let hide_x = app.layout.files_hide_x();
    let hot = app.hover == Some(Hit::FilesHide);
    buf.set_string(hide_x - 1, area.y, " ×", Style::new().fg(if hot { p.accent } else { p.text_dim }).bg(BG));

    let first = area.y + 2;
    let Some(tree) = app.tree() else { return };
    if let Some(err) = &tree.error {
        return say(buf, area, x, first, width, err, p.red);
    }
    let rows = tree.rows();
    if rows.is_empty() {
        return say(buf, area, x, first, width, "an empty folder", p.text_dim);
    }
    let selected = tree.selected_row(&rows).filter(|_| focused);
    for (i, row) in rows.iter().enumerate().skip(tree.scroll) {
        let y = first + (i - tree.scroll) as u16;
        if y >= area.bottom() {
            break;
        }
        let bg = if selected == Some(i) {
            p.active_bg
        } else if app.hover == Some(Hit::File(i)) {
            p.hover_bg
        } else {
            BG
        };
        buf.set_style(Rect::new(x - 1, y, width + 1, 1), Style::new().bg(bg));
        let entry = row.entry;
        let mark = tree.marks.of(&entry.path);
        let color = match mark {
            Some(Mark::Modified) => p.yellow,
            Some(Mark::Added | Mark::Untracked) => p.green,
            Some(Mark::Conflict) => p.red,
            Some(Mark::Ignored) => p.text_dim,
            None if entry.name.starts_with('.') => p.text_dim,
            None => p.text,
        };
        // Deep folders keep room for their names.
        let indent = (row.depth as u16 * 2).min(width.saturating_sub(8) / 2 * 2);
        let chevron_x = x + indent;
        if entry.dir {
            let chevron = if tree.is_open(&entry.path) { "▾" } else { "▸" };
            buf.set_string(chevron_x, y, chevron, Style::new().fg(p.text_dim).bg(bg));
        }
        // A changed file's letter, or a dot on a folder with changes in it.
        let badge = match mark {
            Some(mark) if mark != Mark::Ignored && entry.dir => "•",
            Some(mark) => mark.letter(),
            None => "",
        };
        let badge_x = (x + width).saturating_sub(badge.chars().count() as u16 + 1);
        buf.set_string(badge_x, y, badge, Style::new().fg(color).bg(bg));
        let name_x = chevron_x + 2;
        let room = badge_x.saturating_sub(name_x + 1) as usize;
        let mut style = Style::new().fg(color).bg(bg);
        if selected == Some(i) {
            style = style.add_modifier(Modifier::BOLD);
        }
        render_faded(buf, name_x, y, &entry.name, room, style, bg);
    }
}

/// What the hovered port is, beside the ports bar.
pub fn render_tooltip(buf: &mut Buffer, app: &App) {
    let Some(Hit::Port(index)) = app.hover else { return };
    let Some(port) = app.ports.ready().and_then(|ports| ports.get(index)) else { return };
    let area = app.layout.ports_bar;
    if area.is_empty() {
        return;
    }
    let y = area.y + 2 + index as u16;
    let place = app.find_tab(port.tab).map(|(w, t)| {
        let ws = &app.workspaces[w];
        format!(" · {} · {}", ws.name, ws.tabs[t].title())
    });
    let text = format!(" {} ({}){} ", port.command, port.pid, place.unwrap_or_default());
    let x = area.right();
    let room = app.layout.area.right().saturating_sub(x) as usize;
    let p = theme::palette();
    buf.set_stringn(x, y, text, room, Style::new().fg(p.text).bg(p.button_hover_bg));
}
