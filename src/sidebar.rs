//! The Chrome-style vertical tab strip of the current workspace.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Mode};
use crate::layout::{Hit, ListItem};
use crate::tab::Tab;
use crate::workspace::GroupId;
use crate::theme::{self, BG};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draws the sidebar; returns where the text cursor goes while renaming.
pub fn render(buf: &mut Buffer, app: &App) -> Option<Position> {
    let p = theme::palette();
    let layout = &app.layout;
    buf.set_style(layout.sidebar, Style::new().bg(BG).fg(p.text));
    if layout.sidebar.width == 0 {
        return None;
    }

    render_header(buf, app);
    let ws = app.ws();
    let mut cursor = None;
    let renaming = app.mode == Mode::Rename && !layout.collapsed;
    for (item_index, item) in ws.list_items().into_iter().enumerate() {
        let Some(row) = layout.row_of(item_index, ws.scroll) else { continue };
        let pill = layout.pill(row);
        // Tabs in a group hang off a rail in the group's colour.
        if let ListItem::Tab(index) = item
            && let Some(group) = ws.group_of(index)
        {
            buf.set_string(layout.list.x, row, "▎", Style::new().fg(group.color()).bg(BG));
        }
        match item {
            ListItem::Tab(index) if renaming && app.renaming_group.is_none() && index == ws.active => {
                cursor = Some(render_rename(buf, app, pill));
            }
            ListItem::Tab(index) => render_tab(buf, app, index, &ws.tabs[index], pill),
            ListItem::Group(id) if renaming && app.renaming_group == Some(id) => {
                cursor = Some(render_group_rename(buf, app, id, pill));
            }
            ListItem::Group(id) => render_group(buf, app, id, pill),
            ListItem::Separator => {
                let line = "─".repeat(pill.width.saturating_sub(2) as usize);
                buf.set_string(pill.x + 1, row, line, Style::new().fg(p.border).bg(BG));
            }
            ListItem::NewTab => {
                let hovered = app.hover == Some(Hit::NewTab);
                let bg = if hovered { p.button_hover_bg } else { p.button_bg };
                buf.set_style(pill, Style::new().bg(bg));
                buf.set_string(pill.x + pill.width / 2, row, "+", Style::new().fg(p.text).bg(bg));
            }
        }
    }
    render_footer(buf, app);

    let divider_hot = matches!(app.hover, Some(Hit::Divider)) || app.dragging_divider();
    let divider_style = Style::new().fg(if divider_hot { p.accent } else { p.border }).bg(BG);
    for y in layout.divider.top()..layout.divider.bottom() {
        buf.set_string(layout.divider.x, y, "│", divider_style);
    }
    cursor
}

fn render_header(buf: &mut Buffer, app: &App) {
    let p = theme::palette();
    let layout = &app.layout;
    let button = |buf: &mut Buffer, rect: Rect, glyph: &str, hit: Hit, base: Color| {
        if rect.is_empty() {
            return;
        }
        let bg = if app.hover == Some(hit) { p.button_hover_bg } else { base };
        buf.set_style(rect, Style::new().bg(bg));
        buf.set_string(rect.x + 1, rect.y, glyph, Style::new().fg(p.text).bg(bg));
    };
    let toggle = layout.toggle_button();
    let search = layout.search_button();
    button(buf, toggle, "≡", Hit::Toggle, BG);
    button(buf, search, "⌕", Hit::Search, p.button_bg);

    // The workspace name, between the two buttons.
    if !search.is_empty() {
        let x = toggle.right() + 1;
        let room = search.x.saturating_sub(x + 1) as usize;
        let name = &app.ws().name;
        let style = Style::new().fg(p.text).bg(BG).add_modifier(Modifier::BOLD);
        render_faded(buf, x, layout.header.y, name, room, style, BG);
    }

    if layout.header.height > 1 {
        let y = layout.header.y + 1;
        let style = Style::new().fg(p.border).bg(BG);
        for x in layout.header.x + 1..layout.header.right().saturating_sub(1) {
            buf.set_string(x, y, "─", style);
        }
    }
}

fn render_tab(buf: &mut Buffer, app: &App, index: usize, tab: &Tab, pill: Rect) {
    let p = theme::palette();
    let active = index == app.ws().active;
    let hovered = matches!(app.hover, Some(Hit::Tab(i) | Hit::Close(i)) if i == index);
    let highlight = app.highlight_active();
    let (bg, fg) = match (active, hovered) {
        (true, _) if highlight => (p.accent, p.on_accent),
        (true, _) => (p.active_bg, p.text),
        (false, true) => (p.hover_bg, p.text),
        (false, false) => (BG, p.text),
    };
    buf.set_style(pill, Style::new().bg(bg).fg(fg));

    let (icon, icon_color) = if app.is_busy(tab) {
        (SPINNER[app.spinner_frame() % SPINNER.len()], p.accent)
    } else {
        theme::process_icon(tab.process_name())
    };
    let icon_color = if active && highlight { fg } else { icon_color };
    // Icon only when collapsed (or squeezed by a tiny window).
    let icon_only = app.layout.collapsed || pill.width < 8;
    let icon_x = if icon_only { pill.x + pill.width / 2 } else { pill.x + 1 };
    if pill.width > 0 {
        buf.set_string(icon_x, pill.y, icon, Style::new().fg(icon_color).bg(bg));
    }
    if icon_only {
        return;
    }

    // Right edge: the pin, a close button on the active / hovered tab, or
    // status dots.
    let button_hot = app.hover == Some(Hit::Close(index));
    let marker = if tab.pinned && (active || hovered || !(tab.bell || tab.unseen)) {
        let color = if button_hot || active || hovered { fg } else { p.text_dim };
        Some(("⚲", color))
    } else if active || hovered {
        Some(("×", if button_hot { p.red } else { fg }))
    } else if tab.bell {
        Some(("•", p.yellow))
    } else if tab.unseen {
        Some(("•", p.accent))
    } else {
        None
    };
    let reserved = if marker.is_some() { 6 } else { 4 };
    if let Some((glyph, color)) = marker {
        buf.set_string(pill.right() - 2, pill.y, glyph, Style::new().fg(color).bg(bg));
    }

    let mut title_style = Style::new().fg(fg).bg(bg);
    if tab.unseen && !active {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    let max_width = pill.width.saturating_sub(reserved) as usize;
    render_faded(buf, pill.x + 3, pill.y, &tab.title(), max_width, title_style, bg);
}

/// A group's header: a chip with its name in its colour, like Chrome's.
/// A collapsed group also shows how many tabs it hides and whether any of
/// them is busy or has news.
fn render_group(buf: &mut Buffer, app: &App, id: GroupId, pill: Rect) {
    let p = theme::palette();
    let ws = app.ws();
    let Some(group) = ws.group(id) else { return };
    let hovered = app.hover == Some(Hit::Group(id));
    let bg = if hovered { p.hover_bg } else { BG };
    buf.set_style(pill, Style::new().bg(bg));
    let chip = Style::new().fg(p.on_accent).bg(group.color()).add_modifier(Modifier::BOLD);
    if app.layout.collapsed || pill.width < 8 {
        let initial = group.name.chars().next().map_or_else(String::new, |c| c.to_uppercase().collect());
        buf.set_stringn(pill.x, pill.y, format!(" {initial:1} "), pill.width as usize, chip);
        return;
    }

    let dim = Style::new().fg(if hovered { p.text } else { p.text_dim }).bg(bg);
    let mut right = pill.right() - 2;
    buf.set_string(right, pill.y, if group.collapsed { "▸" } else { "▾" }, dim);
    if group.collapsed {
        let tabs = &ws.tabs[ws.group_range(id)];
        let count = tabs.len().to_string();
        right -= count.len() as u16 + 1;
        buf.set_string(right, pill.y, &count, dim);
        let marker = if tabs.iter().any(|t| app.is_busy(t)) {
            Some((SPINNER[app.spinner_frame() % SPINNER.len()], p.accent))
        } else if tabs.iter().any(|t| t.bell) {
            Some(("•", p.yellow))
        } else if tabs.iter().any(|t| t.unseen) {
            Some(("•", p.accent))
        } else {
            None
        };
        if let Some((glyph, color)) = marker {
            right -= 2;
            buf.set_string(right, pill.y, glyph, Style::new().fg(color).bg(bg));
        }
    }

    // The chip: " name ", or a dash of colour for an unnamed group.
    let room = right.saturating_sub(pill.x + 3) as usize;
    buf.set_string(pill.x, pill.y, " ", chip);
    let end = render_faded(buf, pill.x + 1, pill.y, &group.name, room, chip, group.color());
    buf.set_string(end, pill.y, if group.name.is_empty() { "  " } else { " " }, chip);
}

/// A group's header turned into a text field for its name.
fn render_group_rename(buf: &mut Buffer, app: &App, id: GroupId, pill: Rect) -> Position {
    let Some(group) = app.ws().group(id) else { return Position::new(pill.x, pill.y) };
    let chip = Style::new().fg(theme::palette().on_accent).bg(group.color()).add_modifier(Modifier::BOLD);
    let (tail, width) = input_tail(&app.input, pill.width.saturating_sub(4) as usize);
    let end = pill.x + 1 + width as u16;
    buf.set_string(pill.x, pill.y, " ", chip);
    buf.set_string(pill.x + 1, pill.y, &tail, chip.add_modifier(Modifier::UNDERLINED));
    buf.set_string(end, pill.y, " ", chip);
    Position::new(end, pill.y)
}

/// The active tab's row turned into a text field.
fn render_rename(buf: &mut Buffer, app: &App, pill: Rect) -> Position {
    let p = theme::palette();
    let style = Style::new().fg(p.text).bg(p.active_bg);
    buf.set_style(pill, style);
    buf.set_string(pill.x + 1, pill.y, "✎", Style::new().fg(p.green).bg(p.active_bg));
    let (tail, width) = input_tail(&app.input, pill.width.saturating_sub(5) as usize);
    buf.set_string(pill.x + 3, pill.y, &tail, style.add_modifier(Modifier::UNDERLINED));
    Position::new(pill.x + 3 + width as u16, pill.y)
}

/// The end of `text` that fits in `room` columns, and its width: while
/// typing, the end is what matters.
fn input_tail(text: &str, room: usize) -> (String, usize) {
    let mut width = 0;
    let mut tail: Vec<char> = Vec::new();
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if width + w > room {
            break;
        }
        width += w;
        tail.push(c);
    }
    (tail.into_iter().rev().collect(), width)
}

/// Draws `text` clipped to `max_width`; clipped text fades out into the
/// background over its last few columns, like Chrome's tab titles. Returns
/// the column after the text.
pub fn render_faded(buf: &mut Buffer, x: u16, y: u16, text: &str, max_width: usize, style: Style, bg: Color) -> u16 {
    let p = theme::palette();
    let mut glyphs = Vec::new();
    let mut width = 0;
    let mut truncated = false;
    for c in text.chars().filter(|c| !c.is_control()) {
        let w = c.width().unwrap_or(0);
        if width + w > max_width {
            truncated = true;
            break;
        }
        width += w;
        glyphs.push((c, w));
    }
    let fade_from = if truncated { glyphs.len().saturating_sub(3) } else { glyphs.len() };
    let fg = style.fg.unwrap_or(p.text);
    let mut col = x;
    for (i, (c, w)) in glyphs.iter().enumerate() {
        let mut style = style;
        if i >= fade_from {
            let step = (i - fade_from + 1) as f32;
            style = style.fg(theme::blend(fg, bg, step * 0.25));
        }
        buf.set_string(col, y, c.to_string(), style);
        col += *w as u16;
    }
    col
}

fn render_footer(buf: &mut Buffer, app: &App) {
    let p = theme::palette();
    let footer = app.layout.footer;
    if footer.is_empty() {
        return;
    }
    let (badge, hint) = app.footer();
    let mut x = if app.layout.collapsed { footer.x } else { footer.x + 1 };
    if let Some((label, color)) = badge {
        let label = format!(" {label} ");
        let style = Style::new().fg(p.on_accent).bg(color).add_modifier(Modifier::BOLD);
        let room = footer.right().saturating_sub(x) as usize;
        let (end, _) = buf.set_stringn(x, footer.y, &label, room, style);
        x = end + 1;
    }
    let room = footer.right().saturating_sub(x + 1) as usize;
    if room > 0 && !app.layout.collapsed {
        buf.set_stringn(x, footer.y, hint, room, Style::new().fg(p.text_dim).bg(BG));
    }
}
