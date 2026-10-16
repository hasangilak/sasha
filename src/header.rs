//! The header across the top of the window: the terminal on screen's icon
//! and title, which opens its menu, and on the right the buttons for the
//! tab sidebar, a new terminal, the bars and everything else, lit up while
//! what they show is showing.

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

    // The terminal's icon and title, and ▾ for its menu.
    let mut x = top.x + 2;
    let end = app.layout.header_buttons_left().saturating_sub(2);
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
        let fg = if on || hovered { p.text } else { p.text_dim };
        buf.set_string(rect.x, rect.y, format!(" {} ", button.icon()), Style::new().fg(fg).bg(button_bg));
        if button == HeaderButton::Sidebar {
            buf.set_string(rect.right() + 1, rect.y, "│", Style::new().fg(p.border).bg(bg));
        }
    }
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
