use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::app::App;
use crate::sidebar;

pub fn draw(frame: &mut Frame, app: &App) {
    sidebar::render(frame.buffer_mut(), app);

    let pane = app.layout.pane;
    let Some(tab) = app.active_tab() else { return };
    let screen = tab.screen();
    render_screen(frame.buffer_mut(), pane, screen);

    if screen.scrollback() == 0 && !screen.hide_cursor() {
        let (row, col) = screen.cursor_position();
        if row < pane.height && col < pane.width {
            frame.set_cursor_position(Position::new(pane.x + col, pane.y + row));
        }
    }
}

/// Copies the emulator's visible cells into the ratatui buffer.
fn render_screen(buf: &mut Buffer, area: Rect, screen: &vt100::Screen) {
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
            target.set_symbol(if contents.is_empty() { " " } else { contents });
            target.set_style(cell_style(cell));
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
