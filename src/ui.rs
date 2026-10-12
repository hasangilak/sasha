use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::{App, Mode, Selection};
use crate::{sidebar, theme};
use crate::theme::{ACCENT, BORDER, ON_ACCENT, SIDEBAR_BG, TEXT, TEXT_DIM, YELLOW};

pub fn draw(frame: &mut Frame, app: &App) {
    let rename_cursor = sidebar::render(frame.buffer_mut(), app);

    let pane = app.layout.pane;
    let Some(tab) = app.active_tab() else { return };
    let screen = tab.screen();
    let selection = app.selection.as_ref().filter(|s| s.tab == tab.id);
    render_screen(frame.buffer_mut(), pane, screen, selection);
    if screen.scrollback() > 0 {
        render_scroll_badge(frame.buffer_mut(), pane, screen.scrollback());
    }

    match app.mode {
        Mode::Help => render_help(frame, app),
        Mode::ConfirmQuit => render_confirm_quit(frame, app),
        Mode::Scroll => {}
        Mode::Rename => {
            if let Some(cursor) = rename_cursor {
                frame.set_cursor_position(cursor);
            }
        }
        Mode::Search => render_search(frame, app),
        Mode::Normal | Mode::Tab => {
            if screen.scrollback() == 0 && !screen.hide_cursor() {
                let (row, col) = screen.cursor_position();
                if row < pane.height && col < pane.width {
                    frame.set_cursor_position(Position::new(pane.x + col, pane.y + row));
                }
            }
        }
    }
}

/// Every binding, grouped; `{p}` is replaced by the prefix label.
const HELP: &[(&str, &[(&str, &str)])] = &[
    ("Tab mode — press {p}, then", &[
        ("j k  ↓ ↑", "next / previous tab"),
        ("1 … 9", "go to tab (9 = last)"),
        ("Tab", "back to the previous tab"),
        ("g G", "first / last tab"),
        ("J K", "move tab down / up"),
        ("t c", "new tab"),
        ("r ,", "rename tab (empty name = automatic)"),
        ("/ f", "search tabs"),
        ("[ v PgUp", "scroll mode (j k ^u ^d g G, q exits)"),
        ("x w", "close tab"),
        ("b s", "collapse / expand sidebar"),
        ("h l  ← →", "narrower / wider sidebar"),
        ("{p}", "send {p} to the shell"),
        ("q", "quit"),
        ("Esc ⏎", "back to the terminal"),
    ]),
    ("Anywhere", &[
        ("^PgDn ^PgUp", "next / previous tab"),
        ("^⇧PgDn ^⇧PgUp", "move tab down / up"),
        ("Alt 1 … 9", "go to tab"),
        ("⇧PgUp ⇧PgDn", "scroll back / forward"),
    ]),
    ("Mouse", &[
        ("click / drag", "switch / reorder tabs"),
        ("double-click tab", "rename it"),
        ("drag in terminal", "select & copy (2× word, 3× line)"),
        ("⇧ drag", "select even when an app uses the mouse"),
        ("middle-click ×", "close tab"),
        ("drag the divider", "resize the sidebar"),
    ]),
];

fn render_help(frame: &mut Frame, app: &App) {
    let prefix = app.config.prefix.label();
    let key_width = 18;
    let mut lines = Vec::new();
    for (section, bindings) in HELP {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let section = section.replace("{p}", &prefix);
        lines.push(Line::styled(section, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
        for (keys, action) in *bindings {
            let keys = keys.replace("{p}", &prefix);
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys:<key_width$}"), Style::new().fg(TEXT)),
                Span::styled(action.replace("{p}", &prefix), Style::new().fg(TEXT_DIM)),
            ]));
        }
    }
    let area = popup_area(frame.area(), 62, lines.len() as u16 + 2);
    let block = popup_block(" sasha keys ");
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Chrome's tab search: a dropdown next to the sidebar's ⌕ button.
fn render_search(frame: &mut Frame, app: &App) {
    let results = app.search_results();
    let pane = app.layout.pane;
    let visible = results.len().clamp(1, 12);
    let width = pane.width.saturating_sub(2).min(64);
    let height = (visible as u16 + 4).min(pane.height);
    let area = Rect::new(pane.x + 1, pane.y, width, height);
    if area.width < 12 || area.height < 5 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = popup_block(" search tabs ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let buf = frame.buffer_mut();
    buf.set_string(inner.x, inner.y, "⌕ ", Style::new().fg(theme::PURPLE).bg(SIDEBAR_BG));
    let query_x = inner.x + 2;
    let (end, _) = buf.set_stringn(query_x, inner.y, &app.input, inner.width.saturating_sub(3) as usize, Style::new().fg(TEXT).bg(SIDEBAR_BG));
    if app.input.is_empty() {
        buf.set_string(query_x, inner.y, "title, program or folder", Style::new().fg(TEXT_DIM).bg(SIDEBAR_BG));
    }
    let separator = "─".repeat(inner.width as usize);
    buf.set_string(inner.x, inner.y + 1, &separator, Style::new().fg(BORDER).bg(SIDEBAR_BG));

    let list_y = inner.y + 2;
    if results.is_empty() {
        buf.set_string(inner.x, list_y, "no matching tabs", Style::new().fg(TEXT_DIM).bg(SIDEBAR_BG));
    }
    // Keep the highlighted row in view.
    let first = app.search_selected.saturating_sub(visible - 1);
    for (row, &index) in results.iter().enumerate().skip(first).take(visible) {
        let tab = &app.tabs[index];
        let y = list_y + (row - first) as u16;
        let bg = if row == app.search_selected { theme::ACTIVE_BG } else { SIDEBAR_BG };
        buf.set_style(Rect::new(inner.x, y, inner.width, 1), Style::new().bg(bg));
        let (icon, color) = theme::process_icon(tab.process_name());
        buf.set_string(inner.x + 1, y, icon, Style::new().fg(color).bg(bg));

        let dir = tab.cwd().map(pretty_path).unwrap_or_default();
        let dir_width = (dir.chars().count() as u16).min(inner.width / 2);
        let title_room = inner.width.saturating_sub(dir_width + 5) as usize;
        buf.set_stringn(inner.x + 3, y, tab.title(), title_room, Style::new().fg(TEXT).bg(bg));
        if dir_width > 0 {
            let dir: String = dir.chars().rev().take(dir_width as usize).collect::<Vec<_>>().into_iter().rev().collect();
            buf.set_string(inner.right() - dir_width - 1, y, dir, Style::new().fg(TEXT_DIM).bg(bg));
        }
    }
    frame.set_cursor_position(Position::new(end, inner.y));
}

/// A path with the home directory shown as `~`.
fn pretty_path(path: &std::path::Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(ToOwned::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn render_confirm_quit(frame: &mut Frame, app: &App) {
    let count = app.tabs.len();
    let lines = vec![
        Line::styled(format!("Close {count} tabs and quit?"), Style::new().fg(TEXT)),
        Line::default(),
        Line::from(vec![
            Span::styled(" y ", Style::new().fg(SIDEBAR_BG).bg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(" quit    ", Style::new().fg(TEXT_DIM)),
            Span::styled(" n ", Style::new().fg(TEXT).bg(BORDER).add_modifier(Modifier::BOLD)),
            Span::styled(" cancel", Style::new().fg(TEXT_DIM)),
        ]),
    ];
    let area = popup_area(frame.area(), 34, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(popup_block(" quit ")), area);
}

fn popup_block(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .title(Span::styled(title, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)))
        .style(Style::new().bg(SIDEBAR_BG).fg(TEXT))
        .padding(ratatui::widgets::Padding::horizontal(1))
}

fn popup_area(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn render_scroll_badge(buf: &mut Buffer, pane: Rect, offset: usize) {
    let label = format!(" ↑ {offset} ");
    let width = label.chars().count() as u16;
    if pane.width > width + 1 {
        let style = Style::new().fg(ON_ACCENT).bg(YELLOW).add_modifier(Modifier::BOLD);
        buf.set_string(pane.right() - width - 1, pane.y, label, style);
    }
}

/// Copies the emulator's visible cells into the ratatui buffer.
fn render_screen(buf: &mut Buffer, area: Rect, screen: &vt100::Screen, selection: Option<&Selection>) {
    let (rows, cols) = screen.size();
    for row in 0..rows.min(area.height) {
        for col in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(row, col) else { continue };
            let target = &mut buf[(area.x + col, area.y + row)];
            if cell.is_wide_continuation() {
                // The wide glyph to the left already covers this column.
                target.reset();
                continue;
            }
            let contents = cell.contents();
            let mut style = cell_style(cell);
            if selection.is_some_and(|s| s.contains(row, col)) {
                style.add_modifier.toggle(Modifier::REVERSED);
            }
            target.set_symbol(if contents.is_empty() { " " } else { contents });
            target.set_style(style);
        }
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut modifier = Modifier::empty();
    if cell.bold() {
        modifier |= Modifier::BOLD;
    }
    if cell.dim() {
        modifier |= Modifier::DIM;
    }
    if cell.italic() {
        modifier |= Modifier::ITALIC;
    }
    if cell.underline() {
        modifier |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        modifier |= Modifier::REVERSED;
    }
    Style::new()
        .fg(convert_color(cell.fgcolor()))
        .bg(convert_color(cell.bgcolor()))
        .add_modifier(modifier)
}

fn convert_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
