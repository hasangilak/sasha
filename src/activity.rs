//! The thin workspace bar on the far left, like VS Code's activity bar.

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::app::{Activity, App};
use crate::layout::{Hit, Layout};
use crate::sidebar::{self, SPINNER};
use crate::theme::{self, BG};
use crate::workspace;

pub fn render(buf: &mut Buffer, app: &App) {
    let p = theme::palette();
    let layout = &app.layout;
    let area = layout.activity;
    if area.is_empty() {
        return;
    }
    buf.set_style(area, Style::new().bg(BG).fg(p.text));

    for (index, ws) in app.workspaces.iter().enumerate() {
        let Some(row) = layout.activity_row(index, app.activity_scroll) else { continue };
        let current = index == app.current;
        let hovered = app.hover == Some(Hit::Workspace(index));
        let color = ws.color();

        // VS Code's active marker: a bar on the left edge.
        if current {
            buf.set_string(area.x, row, "▎", Style::new().fg(color).bg(BG));
        }
        let (fg, bg) = match (current, hovered) {
            (true, _) => (p.on_accent, color),
            (false, true) => (color, theme::blend(color, BG, 0.55)),
            (false, false) => (color, theme::blend(color, BG, 0.78)),
        };
        let mut style = Style::new().fg(fg).bg(bg);
        if current {
            style = style.add_modifier(Modifier::BOLD);
        }
        let badge = layout.badge(row);
        buf.set_style(badge, style);
        let initials = ws.initials();
        let pad = badge.width.saturating_sub(initials.width() as u16) / 2;
        buf.set_stringn(badge.x + pad, row, &initials, badge.width as usize, style);

        // Wide enough: the name next to the badge.
        let label = layout.activity_label(row);
        if !label.is_empty() {
            let mut style = Style::new().fg(if current || hovered { p.text } else { p.text_dim }).bg(BG);
            if current {
                style = style.add_modifier(Modifier::BOLD);
            }
            sidebar::render_faded(buf, label.x, row, &ws.name, label.width as usize, style, BG);
        }

        // What's happening in workspaces you're not looking at.
        let marker_x = layout.activity_marker_x();
        if !current && marker_x < layout.activity_divider.x {
            let marker = match app.workspace_activity(index) {
                Some(Activity::Busy) => Some((SPINNER[app.spinner_frame() % SPINNER.len()], p.accent)),
                Some(Activity::Bell) => Some(("•", p.yellow)),
                Some(Activity::Unseen) => Some(("•", p.accent)),
                None => None,
            };
            if let Some((glyph, color)) = marker {
                buf.set_string(marker_x, row, glyph, Style::new().fg(color).bg(BG));
            }
        }
    }

    if let Some(row) = layout.activity_row(app.workspaces.len(), app.activity_scroll) {
        button(buf, layout, row, "+", "add folder", app.hover == Some(Hit::AddWorkspace));
    }
    let help = layout.help_button();
    if !help.is_empty() {
        button(buf, layout, help.y, "?", "keys & help", app.hover == Some(Hit::HelpButton));
    }

    // The edge you drag to resize the bar.
    let divider = layout.activity_divider;
    let hot = app.hover == Some(Hit::ActivityDivider) || app.dragging_activity_divider();
    let style = Style::new().fg(if hot { p.accent } else { p.border }).bg(BG);
    for y in divider.top()..divider.bottom() {
        buf.set_string(divider.x, y, "│", style);
    }
}

/// A badge-sized button, labelled when the bar is wide enough.
fn button(buf: &mut Buffer, layout: &Layout, row: u16, glyph: &str, label: &str, hovered: bool) {
    let p = theme::palette();
    let rect = layout.badge(row);
    if rect.is_empty() {
        return;
    }
    let (fg, bg) = if hovered { (p.text, p.button_hover_bg) } else { (p.text_dim, BG) };
    buf.set_style(rect, Style::new().bg(bg));
    buf.set_string(rect.x + rect.width.saturating_sub(1) / 2, rect.y, glyph, Style::new().fg(fg).bg(bg));
    let room = layout.activity_label(row);
    if !room.is_empty() {
        let style = Style::new().fg(if hovered { p.text } else { p.text_dim }).bg(BG);
        buf.set_stringn(room.x, row, label, room.width as usize, style);
    }
}

/// A label next to the hovered activity bar item, drawn over the sidebar.
pub fn render_tooltip(buf: &mut Buffer, app: &App) {
    let p = theme::palette();
    let layout = &app.layout;
    let (row, text) = match app.hover {
        Some(Hit::Workspace(index)) => {
            let Some(row) = layout.activity_row(index, app.activity_scroll) else { return };
            let ws = &app.workspaces[index];
            let path = workspace::pretty_path(&ws.path);
            // A wide bar already shows the name.
            if layout.activity_wide() { (row, format!(" {path} ")) } else { (row, format!(" {}  {path} ", ws.name)) }
        }
        _ if layout.activity_wide() => return,
        Some(Hit::AddWorkspace) => {
            let Some(row) = layout.activity_row(app.workspaces.len(), app.activity_scroll) else { return };
            (row, " add a folder as a workspace ".to_owned())
        }
        Some(Hit::HelpButton) => (layout.help_button().y, " keys & help ".to_owned()),
        _ => return,
    };
    let x = layout.activity.right();
    let room = layout.area.right().saturating_sub(x) as usize;
    if room == 0 {
        return;
    }
    let style = Style::new().fg(p.text).bg(p.button_hover_bg);
    buf.set_stringn(x, row, text, room, style);
}
