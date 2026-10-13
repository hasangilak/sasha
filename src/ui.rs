use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, GroupEntry, Mode, PickerEntry, PickerKind, Selection};
use crate::links::Link;
use crate::workspace::pretty_path;
use crate::{activity, bars, panel_view, sidebar, theme};

pub fn draw(frame: &mut Frame, app: &App) {
    activity::render(frame.buffer_mut(), app);
    bars::render(frame.buffer_mut(), app);
    let rename_cursor = sidebar::render(frame.buffer_mut(), app);

    let pane = app.layout.pane;
    let Some(tab) = app.active_tab() else { return };
    let screen = tab.screen();
    let selection = app.selection.as_ref().filter(|s| s.tab == tab.id);
    let link = app.hover_link.as_ref().filter(|(id, _)| *id == tab.id).map(|(_, link)| link);
    render_screen(frame.buffer_mut(), pane, screen, selection, link);
    if screen.scrollback() > 0 {
        render_scroll_badge(frame.buffer_mut(), pane, screen.scrollback());
    }
    activity::render_tooltip(frame.buffer_mut(), app);
    bars::render_tooltip(frame.buffer_mut(), app);

    match &app.mode {
        Mode::Help => render_help(frame, app),
        Mode::ConfirmQuit => render_confirm_quit(frame, app),
        Mode::Scroll => {}
        Mode::Rename(_) => {
            if let Some(cursor) = rename_cursor {
                frame.set_cursor_position(cursor);
            }
        }
        Mode::Picker(picker) => match picker.kind {
            PickerKind::Search => render_search(frame, app),
            PickerKind::Workspaces => render_workspaces(frame, app),
            PickerKind::Groups => render_groups(frame, app),
        },
        Mode::Panel(_) => panel_view::render(frame, app),
        Mode::Normal | Mode::Tab(_) => {
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
        ("j k  ↓ ↑", "next / previous tab, through every workspace"),
        ("h l  ← →", "previous / next workspace"),
        ("1 … 9", "go to tab (9 = last)"),
        ("Tab  ⇧Tab", "previous tab / previous workspace"),
        ("g G", "first / last tab"),
        ("J K", "move tab down / up"),
        ("t n c", "new tab (in the tab's group)"),
        ("x", "close tab (pinned: press twice)"),
        ("p", "pin / unpin tab"),
        ("m", "add tab to a group, or name a new one"),
        ("u", "take tab out of its group"),
        ("z", "collapse / expand the tab's group"),
        ("e", "edit group: name, ⇥ colour"),
        ("r ,", "set the tab's prefix (empty = its folder)"),
        ("w", "workspaces: switch, add a folder, ^D remove"),
        ("W", "rename the workspace (empty = its folder)"),
        ("/ f", "search tabs in every workspace"),
        ("[ v PgUp", "scroll mode (j k ^u ^d g G, q exits)"),
        ("b s", "collapse / expand the tab sidebar"),
        ("a", "workspace bar: badges → names → hidden"),
        ("o", "sidebar: every workspace's tabs / just this one's"),
        ("-", "minimize this workspace's section (its badge restores)"),
        ("C", "keep the computer awake, or let it sleep (☀ / ☾)"),
        ("P", "open pull requests (gh) and their diffs"),
        ("O", "open ports in your tabs; x x force-kills"),
        ("i", "show / hide the ports and pull request bars"),
        ("< >", "narrower / wider sidebar"),
        ("{p}", "send {p} to the shell"),
        ("d", "detach: tabs keep running, sasha brings them back"),
        ("q", "quit: stop everything; tabs reopen next time"),
        ("Esc ⏎", "back to the terminal"),
    ]),
    ("Anywhere", &[
        ("^PgDn ^PgUp", "next / previous tab"),
        ("^⇧PgDn ^⇧PgUp", "move tab down / up"),
        ("Alt 1 … 9", "go to tab"),
        ("⇧PgUp ⇧PgDn", "scroll back / forward"),
    ]),
    ("Mouse", &[
        ("click / drag", "switch / reorder tabs (any workspace's)"),
        ("double-click tab", "rename it"),
        ("middle-click ×", "close tab"),
        ("right-click tab", "pin / unpin (click ⚲ to unpin)"),
        ("click group", "collapse / expand"),
        ("group + ×", "new terminal in it / delete the group"),
        ("2×-click group", "rename it"),
        ("right-click group", "change its colour"),
        ("drag tab to group", "add it / take it out"),
        ("workspace bar", "switch workspace, + adds a folder"),
        ("2×-click badge", "rename the workspace (or 2× its name)"),
        ("heading – ×", "minimize / close a workspace's section"),
        ("drag an edge", "resize that bar (wide workspace bar: names)"),
        ("drag in terminal", "select & copy (2× word, 3× line)"),
        ("⇧ drag", "select even when an app uses the mouse"),
        ("drag the divider", "resize the sidebar"),
    ]),
];

fn render_help(frame: &mut Frame, app: &App) {
    let p = theme::palette();
    let prefix = app.config.prefix.label();
    let key_width = 18;
    let mut lines = Vec::new();
    for (section, bindings) in HELP {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let section = section.replace("{p}", &prefix);
        lines.push(Line::styled(section, Style::new().fg(p.accent).add_modifier(Modifier::BOLD)));
        for (keys, action) in *bindings {
            let keys = keys.replace("{p}", &prefix);
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys:<key_width$}"), Style::new().fg(p.text)),
                Span::styled(action.replace("{p}", &prefix), Style::new().fg(p.text_dim)),
            ]));
        }
    }
    let area = popup_area(frame.area(), 66, lines.len() as u16 + 2);
    let block = popup_block(" sasha keys ");
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One row of a picker popup.
struct PickerRow {
    /// Workspace initials and colour.
    badge: Option<(String, Color)>,
    icon: (&'static str, Color),
    title: String,
    detail: String,
}

struct Picker<'a> {
    title: &'a str,
    prompt: (&'a str, Color),
    placeholder: &'a str,
    empty: &'a str,
    rows: Vec<PickerRow>,
    hint: Option<(String, Color)>,
}

/// Chrome's tab search, across every workspace.
fn render_search(frame: &mut Frame, app: &App) {
    let p = theme::palette();
    let rows = app
        .search_results()
        .into_iter()
        .map(|(w, t)| {
            let ws = &app.workspaces[w];
            let tab = &ws.tabs[t];
            PickerRow {
                badge: Some((ws.initials(), ws.color())),
                icon: theme::process_icon(tab.process_name()),
                title: tab.title(),
                detail: tab.cwd().map(pretty_path).unwrap_or_default(),
            }
        })
        .collect();
    render_picker(frame, app, Picker {
        title: " search tabs ",
        prompt: ("⌕", p.purple),
        placeholder: "title, program, folder or workspace",
        empty: "no matching tabs",
        rows,
        hint: None,
    });
}

fn render_workspaces(frame: &mut Frame, app: &App) {
    let p = theme::palette();
    let rows = app
        .picker_entries()
        .into_iter()
        .map(|entry| match entry {
            PickerEntry::Workspace(index) => {
                let ws = &app.workspaces[index];
                let count = ws.tabs.len();
                let tabs = match count {
                    0 => String::new(),
                    1 => "1 tab · ".to_owned(),
                    n => format!("{n} tabs · "),
                };
                let icon = if index == app.current { ("●", ws.color()) } else { (" ", p.text) };
                PickerRow {
                    badge: Some((ws.initials(), ws.color())),
                    icon,
                    title: ws.name.clone(),
                    detail: format!("{tabs}{}", pretty_path(&ws.path)),
                }
            }
            PickerEntry::AddFolder(path) => PickerRow {
                badge: Some(("+".to_owned(), p.green)),
                icon: (" ", p.text),
                title: format!(
                    "add {}",
                    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
                ),
                detail: pretty_path(&path),
            },
        })
        .collect();
    let hint = match app.remove_warning() {
        Some(warning) => (warning, p.red),
        None => ("⏎ open · ⇥ complete · ^D remove · esc".to_owned(), p.text_dim),
    };
    render_picker(frame, app, Picker {
        title: " workspaces ",
        prompt: ("▸", p.orange),
        placeholder: "name, or a folder path like ~/Work",
        empty: "no matching folders",
        rows,
        hint: Some(hint),
    });
}

/// Putting the active tab into a new or existing tab group.
fn render_groups(frame: &mut Frame, app: &App) {
    let p = theme::palette();
    let current = app.active_tab().and_then(|t| t.group);
    let rows = app
        .group_entries()
        .into_iter()
        .filter_map(|entry| match entry {
            GroupEntry::New(name) => Some(PickerRow {
                badge: Some(("+".to_owned(), p.green)),
                icon: (" ", p.text),
                title: if name.is_empty() { "new group".to_owned() } else { format!("new group “{name}”") },
                detail: String::new(),
            }),
            GroupEntry::Group(id) => {
                let group = app.group(id)?;
                let count = app.members(id).len();
                let initial = group.name.chars().next().map_or_else(String::new, |c| c.to_uppercase().collect());
                Some(PickerRow {
                    badge: Some((initial, group.color())),
                    icon: if current == Some(id) { ("●", group.color()) } else { (" ", p.text) },
                    title: if group.name.is_empty() { "unnamed group".to_owned() } else { group.name.clone() },
                    detail: format!(
                        "{count} tab{}{}",
                        if count == 1 { "" } else { "s" },
                        if group.collapsed { " · collapsed" } else { "" }
                    ),
                })
            }
        })
        .collect();
    render_picker(frame, app, Picker {
        title: " add tab to group ",
        prompt: ("▾", p.cyan),
        placeholder: "name a new group, or pick one",
        empty: "",
        rows,
        hint: Some(("⏎ add tab · ^D delete group · esc".to_owned(), p.text_dim)),
    });
}

/// A dropdown at the top of the terminal pane with a query line and rows.
fn render_picker(frame: &mut Frame, app: &App, picker: Picker) {
    let p = theme::palette();
    let pane = app.layout.pane;
    let visible = picker.rows.len().clamp(1, 12);
    let hint_rows = u16::from(picker.hint.is_some());
    let width = pane.width.saturating_sub(2).min(72);
    let height = (visible as u16 + 4 + hint_rows).min(pane.height);
    let area = Rect::new(pane.x + 1, pane.y, width, height);
    if area.width < 16 || area.height < 5 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = popup_block(picker.title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let buf = frame.buffer_mut();
    let plain = Style::new().fg(p.text).bg(p.popup_bg);
    let (prompt, prompt_color) = picker.prompt;
    buf.set_string(inner.x, inner.y, prompt, Style::new().fg(prompt_color).bg(p.popup_bg));
    let query_x = inner.x + 2;
    let room = inner.width.saturating_sub(3) as usize;
    // Keep the end of a long query (e.g. a path) visible.
    let query = app.typed();
    let skip = query.width().saturating_sub(room);
    let shown: String = query.chars().skip(skip).collect();
    let (end, _) = buf.set_stringn(query_x, inner.y, &shown, room, plain);
    if query.is_empty() {
        buf.set_stringn(query_x, inner.y, picker.placeholder, room, Style::new().fg(p.text_dim).bg(p.popup_bg));
    }
    let separator = "─".repeat(inner.width as usize);
    buf.set_string(inner.x, inner.y + 1, &separator, Style::new().fg(p.border).bg(p.popup_bg));

    let list_y = inner.y + 2;
    let list_rows = inner.height.saturating_sub(2 + hint_rows) as usize;
    if picker.rows.is_empty() {
        buf.set_string(inner.x, list_y, picker.empty, Style::new().fg(p.text_dim).bg(p.popup_bg));
    }
    // Keep the highlighted row in view.
    let selected = app.picker().map_or(0, |picker| picker.selected);
    let first = selected.saturating_sub(list_rows.saturating_sub(1));
    for (index, row) in picker.rows.iter().enumerate().skip(first).take(list_rows) {
        let y = list_y + (index - first) as u16;
        let bg = if index == selected { p.active_bg } else { p.popup_bg };
        buf.set_style(Rect::new(inner.x, y, inner.width, 1), Style::new().bg(bg));
        let mut x = inner.x;
        if let Some((initials, color)) = &row.badge {
            let style = Style::new().fg(p.on_accent).bg(*color).add_modifier(Modifier::BOLD);
            buf.set_string(x, y, format!("{initials:^4}"), style);
            x += 5;
        }
        let (icon, icon_color) = row.icon;
        buf.set_string(x, y, icon, Style::new().fg(icon_color).bg(bg));
        x += 2;

        let detail_width = (row.detail.width() as u16).min(inner.width / 2);
        let title_room = inner.right().saturating_sub(x + detail_width + 2) as usize;
        buf.set_stringn(x, y, &row.title, title_room, Style::new().fg(p.text).bg(bg));
        if detail_width > 0 {
            // Show the end of long paths: the folder name matters most.
            let skip = row.detail.width().saturating_sub(detail_width as usize);
            let detail: String = row.detail.chars().skip(skip).collect();
            buf.set_string(inner.right() - detail_width, y, detail, Style::new().fg(p.text_dim).bg(bg));
        }
    }
    if let Some((hint, color)) = &picker.hint {
        buf.set_stringn(inner.x, inner.bottom() - 1, hint, inner.width as usize, Style::new().fg(*color).bg(p.popup_bg));
    }
    frame.set_cursor_position(Position::new(end, inner.y));
}

fn render_confirm_quit(frame: &mut Frame, app: &App) {
    let p = theme::palette();
    let count = app.tab_count();
    let key = |label, bg| Span::styled(label, Style::new().fg(p.on_accent).bg(bg).add_modifier(Modifier::BOLD));
    let lines = vec![
        Line::styled(format!("Quit and stop what's running in {count} tabs?"), Style::new().fg(p.text)),
        Line::styled("The tabs reopen next time, as fresh shells.", Style::new().fg(p.text_dim)),
        Line::styled("Detaching keeps them running.", Style::new().fg(p.text_dim)),
        Line::default(),
        Line::from(vec![
            key(" y ", p.red),
            Span::styled(" or ", Style::new().fg(p.text_dim)),
            key(" q ", p.red),
            Span::styled(" quit  ", Style::new().fg(p.text_dim)),
            key(" d ", p.accent),
            Span::styled(" detach  ", Style::new().fg(p.text_dim)),
            Span::styled(" n ", Style::new().fg(p.text).bg(p.border).add_modifier(Modifier::BOLD)),
            Span::styled(" cancel", Style::new().fg(p.text_dim)),
        ]),
    ];
    let area = popup_area(frame.area(), 54, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(popup_block(" quit ")), area);
}

pub fn popup_block(title: &str) -> Block<'_> {
    let p = theme::palette();
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(p.border))
        .title(Span::styled(title, Style::new().fg(p.text).add_modifier(Modifier::BOLD)))
        .style(Style::new().bg(p.popup_bg).fg(p.text))
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
    let p = theme::palette();
    let label = format!(" ↑ {offset} ");
    let width = label.chars().count() as u16;
    if pane.width > width + 1 {
        let style = Style::new().fg(p.on_accent).bg(p.yellow).add_modifier(Modifier::BOLD);
        buf.set_string(pane.right() - width - 1, pane.y, label, style);
    }
}

/// Copies the emulator's visible cells into the ratatui buffer.
fn render_screen(
    buf: &mut Buffer,
    area: Rect,
    screen: &vt100::Screen,
    selection: Option<&Selection>,
    link: Option<&Link>,
) {
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
            // The link under the mouse, ready to click.
            if link.is_some_and(|l| l.contains(row, col)) {
                style = style.add_modifier(Modifier::UNDERLINED);
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
