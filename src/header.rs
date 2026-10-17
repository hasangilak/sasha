//! The header across the top of the window: above the terminal its icon and
//! title, which opens its menu, and the buttons for a new terminal,
//! everything else and the bars that are hidden; above each bar that shows,
//! lit up, the button that hides it.

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Menu};
use crate::layout::{HeaderButton, Hit};
use crate::sidebar::{self, render_faded};
use crate::theme;

pub fn render(buf: &mut Buffer, app: &App) {
    let top = app.layout.top;
    if top.is_empty() {
        return;
    }
    let p = theme::palette();
    let bg = p.popup_bg;
    buf.set_style(top, Style::new().bg(bg).fg(p.text));

    // The edges of the bars below, carried up.
    for x in app.layout.edges() {
        buf.set_string(x, top.y, "│", Style::new().fg(p.border).bg(bg));
    }

    // The terminal's icon and title, and ▾ for its menu.
    let title = app.layout.title();
    if title.width >= 6 {
        let mut x = title.x + 1;
        let end = title.right().saturating_sub(2);
        if let Some(tab) = app.active_tab() {
            let (icon, color) = sidebar::tab_icon(app, tab);
            buf.set_string(x, top.y, &icon, Style::new().fg(color).bg(bg));
            x += icon.width() as u16 + 1;
        }
        let hot = app.hover == Some(Hit::Title) || app.menu() == Some(Menu::Terminal);
        let style = Style::new().fg(if hot { p.accent } else { p.text }).bg(bg).add_modifier(Modifier::BOLD);
        let room = end.saturating_sub(x + 2) as usize;
        let title_end = render_faded(buf, x, top.y, &app.header_title(), room, style, bg);
        buf.set_string(title_end + 1, top.y, "▾", Style::new().fg(if hot { p.accent } else { p.text_dim }).bg(bg));
    }

    render_tower_title(buf, app);

    for (button, rect) in app.layout.header_buttons() {
        let on = app.header_on(button);
        let hovered = app.hover == Some(Hit::Header(button));
        let button_bg = if hovered {
            p.button_hover_bg
        } else if on {
            p.active_bg
        } else {
            bg
        };
        // The hidden tower has something to say.
        let news = button == HeaderButton::Tower && app.tower_has_news();
        let fg = if news {
            p.yellow
        } else if on || hovered {
            p.text
        } else {
            p.text_dim
        };
        buf.set_string(rect.x, rect.y, format!(" {} ", button.icon()), Style::new().fg(fg).bg(button_bg));
    }
}

/// Over the tower's panel: its name and what it runs on, and ▾ for its
/// menu; bright while it has the keys.
fn render_tower_title(buf: &mut Buffer, app: &App) {
    let area = app.layout.tower_title();
    if area.width < 8 {
        return;
    }
    let p = theme::palette();
    let bg = p.popup_bg;
    let hot = app.hover == Some(Hit::TowerTitle) || app.menu() == Some(Menu::Tower);
    let focused = app.tower_focused();
    let color = if hot || focused { p.accent } else { p.text };
    let x = area.x + 1;
    buf.set_string(x, area.y, "♜", Style::new().fg(color).bg(bg));
    let mut label = match app.tower() {
        Some(_) => format!("tower · {}", app.tower_engine().label()),
        None => "tower · stopped".to_owned(),
    };
    if app.tower_autopilot() {
        label.push_str(" · auto");
    }
    let style = Style::new().fg(color).bg(bg);
    let style = if focused { style.add_modifier(Modifier::BOLD) } else { style };
    let news = app.tower_news();
    let news = (news > 0).then(|| format!(" {news} new "));
    let room = area.right().saturating_sub(x + 4 + news.as_ref().map_or(0, |n| n.width() as u16 + 1)) as usize;
    let mut end = render_faded(buf, x + 2, area.y, &label, room, style, bg);
    if let Some(news) = news {
        buf.set_string(end + 1, area.y, &news, Style::new().fg(p.on_accent).bg(p.yellow));
        end += news.width() as u16 + 1;
    }
    buf.set_string(end + 1, area.y, "▾", Style::new().fg(if hot { p.accent } else { p.text_dim }).bg(bg));
}

/// What the hovered header button does, and its key, under it.
pub fn render_tooltip(buf: &mut Buffer, app: &App) {
    let Some(Hit::Header(button)) = app.hover else { return };
    let Some((_, rect)) = app.layout.header_buttons().into_iter().find(|(b, _)| *b == button) else { return };
    let (what, key) = button.tooltip();
    let text = format!(" {what} · {} {key} ", app.config.prefix.label());
    let screen = app.layout.area;
    let x = rect.right().saturating_sub(text.width() as u16).max(screen.x);
    let p = theme::palette();
    buf.set_stringn(x, rect.y + 1, text, screen.right().saturating_sub(x) as usize, Style::new().fg(p.text).bg(p.button_hover_bg));
}
