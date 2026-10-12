//! The Chrome-style vertical tab strip: layout, hit-testing and drawing.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Mode};
use crate::theme::{self, ACCENT, ACTIVE_BG, BORDER, SIDEBAR_BG, TEXT, TEXT_DIM};

pub const DEFAULT_WIDTH: u16 = 30;
pub const MIN_WIDTH: u16 = 16;
pub const COLLAPSED_WIDTH: u16 = 6;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Something clickable on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Toggle,
    Search,
    Tab(usize),
    Close(usize),
    NewTab,
    Divider,
    Pane,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Layout {
    pub sidebar: Rect,
    pub header: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub divider: Rect,
    pub pane: Rect,
    pub collapsed: bool,
}

impl Layout {
    pub fn new(area: Rect, width: u16, collapsed: bool) -> Self {
        let width = if collapsed { COLLAPSED_WIDTH } else { width };
        // Always leave the terminal pane a usable amount of room.
        let width = width
            .min(area.width.saturating_sub(20).max(COLLAPSED_WIDTH))
            .min(area.width);
        let sidebar = Rect { width, ..area };
        let pane = Rect { x: area.x + width, width: area.width - width, ..area };
        let inner = Rect { width: width.saturating_sub(1), ..sidebar };
        let divider = Rect { x: inner.right(), width: width.min(1), ..sidebar };
        let header = Rect { height: inner.height.min(2), ..inner };
        let footer_height = u16::from(inner.height > 3);
        let footer = Rect { y: inner.bottom() - footer_height, height: footer_height, ..inner };
        let list = Rect {
            y: header.bottom(),
            height: inner.height - header.height - footer_height,
            ..inner
        };
        Self { sidebar, header, list, footer, divider, pane, collapsed }
    }

    pub fn toggle_button(&self) -> Rect {
        Rect::new(self.header.x + 1, self.header.y, 3.min(self.header.width), self.header.height.min(1))
    }

    pub fn search_button(&self) -> Rect {
        if self.collapsed || self.header.width < 10 {
            return Rect::default();
        }
        Rect::new(self.header.right() - 4, self.header.y, 3, self.header.height.min(1))
    }

    /// The highlighted "pill" a tab row is drawn in.
    fn pill(&self, row: u16) -> Rect {
        Rect::new(self.list.x + 1, row, self.list.width.saturating_sub(2), 1)
    }

    fn row_of(&self, item: usize, scroll: usize) -> Option<u16> {
        let offset = item.checked_sub(scroll)?;
        (offset < self.list.height as usize).then(|| self.list.y + offset as u16)
    }

    /// Scroll offset that keeps `item` (and the clamp limits) in view.
    pub fn clamp_scroll(&self, scroll: usize, tab_count: usize, reveal: Option<usize>) -> usize {
        let rows = (self.list.height as usize).max(1);
        // Tabs plus the "new tab" button.
        let max = (tab_count + 1).saturating_sub(rows);
        let mut scroll = scroll.min(max);
        if let Some(item) = reveal {
            if item < scroll {
                scroll = item;
            } else if item >= scroll + rows {
                scroll = item + 1 - rows;
            }
        }
        scroll
    }

    pub fn hit(&self, x: u16, y: u16, tab_count: usize, scroll: usize) -> Option<Hit> {
        let pos = Position::new(x, y);
        if self.pane.contains(pos) {
            return Some(Hit::Pane);
        }
        if self.divider.contains(pos) {
            return Some(Hit::Divider);
        }
        if self.toggle_button().contains(pos) {
            return Some(Hit::Toggle);
        }
        if self.search_button().contains(pos) {
            return Some(Hit::Search);
        }
        if !self.list.contains(pos) {
            return None;
        }
        let item = scroll + (y - self.list.y) as usize;
        if item == tab_count {
            return Some(Hit::NewTab);
        }
        if item > tab_count {
            return None;
        }
        let pill = self.pill(y);
        if !self.collapsed && x + 4 >= pill.right() && x < pill.right() {
            Some(Hit::Close(item))
        } else {
            Some(Hit::Tab(item))
        }
    }
}

/// Draws the sidebar; returns where the text cursor goes while renaming.
pub fn render(buf: &mut Buffer, app: &App) -> Option<Position> {
    let layout = &app.layout;
    buf.set_style(layout.sidebar, Style::new().bg(SIDEBAR_BG).fg(TEXT));
    if layout.sidebar.width == 0 {
        return None;
    }

    render_header(buf, app);
    let mut cursor = None;
    for (index, tab) in app.tabs.iter().enumerate() {
        if let Some(row) = layout.row_of(index, app.scroll) {
            let pill = layout.pill(row);
            if app.mode == Mode::Rename && index == app.active && !layout.collapsed {
                cursor = Some(render_rename(buf, app, pill));
            } else {
                render_tab(buf, app, index, tab, pill);
            }
        }
    }
    if let Some(row) = layout.row_of(app.tabs.len(), app.scroll) {
        let hovered = app.hover == Some(Hit::NewTab);
        let bg = if hovered { theme::BUTTON_HOVER_BG } else { theme::BUTTON_BG };
        let pill = layout.pill(row);
        buf.set_style(pill, Style::new().bg(bg));
        buf.set_string(pill.x + pill.width / 2, row, "+", Style::new().fg(TEXT).bg(bg));
    }
    render_footer(buf, app);

    let divider_hot = matches!(app.hover, Some(Hit::Divider)) || app.dragging_divider();
    let divider_style = Style::new().fg(if divider_hot { ACCENT } else { BORDER }).bg(SIDEBAR_BG);
    for y in layout.divider.top()..layout.divider.bottom() {
        buf.set_string(layout.divider.x, y, "│", divider_style);
    }
    cursor
}

/// The active tab's row turned into a text field.
fn render_rename(buf: &mut Buffer, app: &App, pill: Rect) -> Position {
    let style = Style::new().fg(TEXT).bg(ACTIVE_BG);
    buf.set_style(pill, style);
    buf.set_string(pill.x + 1, pill.y, "✎", Style::new().fg(theme::GREEN).bg(ACTIVE_BG));
    // Show the end of the text when it's longer than the row.
    let room = pill.width.saturating_sub(5) as usize;
    let mut width = 0;
    let tail: String = {
        let mut chars: Vec<char> = Vec::new();
        for c in app.input.chars().rev() {
            let w = c.width().unwrap_or(0);
            if width + w > room {
                break;
            }
            width += w;
            chars.push(c);
        }
        chars.into_iter().rev().collect()
    };
    buf.set_string(pill.x + 3, pill.y, &tail, style.add_modifier(Modifier::UNDERLINED));
    Position::new(pill.x + 3 + width as u16, pill.y)
}

fn render_header(buf: &mut Buffer, app: &App) {
    let layout = &app.layout;
    let button = |buf: &mut Buffer, rect: Rect, glyph: &str, hit: Hit, base: Color| {
        if rect.is_empty() {
            return;
        }
        let bg = if app.hover == Some(hit) { theme::BUTTON_HOVER_BG } else { base };
        buf.set_style(rect, Style::new().bg(bg));
        buf.set_string(rect.x + 1, rect.y, glyph, Style::new().fg(TEXT).bg(bg));
    };
    button(buf, layout.toggle_button(), "≡", Hit::Toggle, SIDEBAR_BG);
    button(buf, layout.search_button(), "⌕", Hit::Search, theme::BUTTON_BG);

    if layout.header.height > 1 {
        let y = layout.header.y + 1;
        let style = Style::new().fg(BORDER).bg(SIDEBAR_BG);
        for x in layout.header.x + 1..layout.header.right().saturating_sub(1) {
            buf.set_string(x, y, "─", style);
        }
    }
}

fn render_tab(buf: &mut Buffer, app: &App, index: usize, tab: &crate::tab::Tab, pill: Rect) {
    let active = index == app.active;
    let hovered = matches!(app.hover, Some(Hit::Tab(i) | Hit::Close(i)) if i == index);
    let highlight = app.highlight_active();
    let (bg, fg) = match (active, hovered) {
        (true, _) if highlight => (ACCENT, theme::ON_ACCENT),
        (true, _) => (ACTIVE_BG, TEXT),
        (false, true) => (theme::HOVER_BG, TEXT),
        (false, false) => (SIDEBAR_BG, TEXT),
    };
    buf.set_style(pill, Style::new().bg(bg).fg(fg));

    let (icon, icon_color) = if app.is_busy(index) {
        (SPINNER[app.spinner_frame() % SPINNER.len()], ACCENT)
    } else {
        theme::process_icon(tab.process_name())
    };
    let icon_color = if active && highlight { fg } else { icon_color };
    let icon_x = if app.layout.collapsed { pill.x + pill.width / 2 } else { pill.x + 1 };
    buf.set_string(icon_x, pill.y, icon, Style::new().fg(icon_color).bg(bg));
    if app.layout.collapsed {
        return;
    }

    // Right edge: close button on the active / hovered tab, otherwise status dots.
    let marker = if active || hovered {
        let close_hot = app.hover == Some(Hit::Close(index));
        Some(("×", if close_hot { theme::RED } else { fg }))
    } else if tab.bell {
        Some(("•", theme::YELLOW))
    } else if tab.unseen {
        Some(("•", ACCENT))
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

/// Draws `text` clipped to `max_width`; clipped text fades out into the
/// background over its last few columns, like Chrome's tab titles.
fn render_faded(buf: &mut Buffer, x: u16, y: u16, text: &str, max_width: usize, style: Style, bg: Color) {
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
    let fg = style.fg.unwrap_or(TEXT);
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
}

fn render_footer(buf: &mut Buffer, app: &App) {
    let footer = app.layout.footer;
    if footer.is_empty() {
        return;
    }
    let (badge, hint) = app.footer();
    let mut x = if app.layout.collapsed { footer.x } else { footer.x + 1 };
    if let Some((label, color)) = badge {
        let label = format!(" {label} ");
        let style = Style::new().fg(theme::ON_ACCENT).bg(color).add_modifier(Modifier::BOLD);
        let room = footer.right().saturating_sub(x) as usize;
        let (end, _) = buf.set_stringn(x, footer.y, &label, room, style);
        x = end + 1;
    }
    let room = footer.right().saturating_sub(x + 1) as usize;
    if room > 0 && !app.layout.collapsed {
        buf.set_stringn(x, footer.y, hint, room, Style::new().fg(TEXT_DIM).bg(SIDEBAR_BG));
    }
}
