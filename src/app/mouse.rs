//! Mouse handling: the activity bar, tab sidebar and terminal pane.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{App, Drag, Mode, Selection};
use crate::layout::{self, Hit};
use crate::{input, links};

const MULTI_CLICK: Duration = Duration::from_millis(400);
const WHEEL_LINES: usize = 3;

impl App {
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        let hit = self.layout.hit(mouse.column, mouse.row, self);
        self.hover = hit.filter(|h| *h != Hit::Pane);
        if hit != Some(Hit::Pane) {
            self.hover_link = None;
        }
        if self.handle_drag(&mouse, hit) {
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            match self.mode {
                Mode::Help | Mode::ConfirmQuit | Mode::Search | Mode::Workspaces | Mode::Groups => {
                    self.mode = Mode::Normal;
                    return;
                }
                // Clicking away saves the name, like leaving a text field.
                Mode::Rename => self.finish_rename(),
                Mode::Tab if hit == Some(Hit::Pane) => self.mode = Mode::Normal,
                _ => {}
            }
        }

        match (mouse.kind, hit) {
            (_, Some(Hit::Pane)) => self.mouse_in_pane(mouse),
            (MouseEventKind::Down(MouseButton::Left), Some(hit)) => self.click(hit),
            // Middle-click closes, like in a browser (pinned tabs excepted).
            (MouseEventKind::Down(MouseButton::Middle), Some(Hit::Tab(index) | Hit::Close(index))) => {
                if self.ws().tabs[index].pinned {
                    self.set_status("pinned tab · unpin it first, or ^B x x");
                } else {
                    self.close_tab(index);
                }
            }
            // Right-click pins and unpins, and changes a group's colour.
            (MouseEventKind::Down(MouseButton::Right), Some(Hit::Tab(index) | Hit::Close(index))) => {
                self.toggle_pin(index);
            }
            (MouseEventKind::Down(MouseButton::Right), Some(Hit::Group(id))) => self.cycle_group_color(id),
            (MouseEventKind::ScrollDown | MouseEventKind::ScrollUp, Some(hit)) => {
                let down = mouse.kind == MouseEventKind::ScrollDown;
                if matches!(hit, Hit::Workspace(_) | Hit::AddWorkspace | Hit::HelpButton | Hit::ActivityDivider) {
                    let scroll = if down { self.activity_scroll + 1 } else { self.activity_scroll.saturating_sub(1) };
                    self.activity_scroll = self.layout.clamp_activity_scroll(scroll, self.workspaces.len(), None);
                } else {
                    let ws = &mut self.workspaces[self.current];
                    ws.scroll = if down { ws.scroll + 1 } else { ws.scroll.saturating_sub(1) };
                    ws.clamp_scroll(&self.layout, None);
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, hit: Hit) {
        match hit {
            // A double-click renames the workspace.
            Hit::Workspace(index) => {
                let double = self.last_workspace_click.is_some_and(|(at, i)| i == index && at.elapsed() < MULTI_CLICK);
                self.last_workspace_click = if double { None } else { Some((Instant::now(), index)) };
                self.switch_workspace(index);
                if double {
                    self.start_workspace_rename();
                }
            }
            Hit::WorkspaceName => {
                let double = self.last_name_click.is_some_and(|at| at.elapsed() < MULTI_CLICK);
                self.last_name_click = if double { None } else { Some(Instant::now()) };
                if double {
                    self.start_workspace_rename();
                }
            }
            Hit::ActivityDivider => self.drag = Some(Drag::ActivityDivider),
            Hit::AddWorkspace => self.start_workspaces("~/".to_owned()),
            Hit::HelpButton => self.mode = Mode::Help,
            Hit::Toggle => self.set_collapsed(!self.collapsed),
            Hit::Search => self.start_search(),
            Hit::Tab(index) => {
                let double = self
                    .last_tab_click
                    .is_some_and(|(at, i)| i == index && at.elapsed() < MULTI_CLICK);
                self.last_tab_click = Some((Instant::now(), index));
                self.activate(index);
                if double {
                    self.start_rename();
                } else {
                    self.drag = Some(Drag::Tab(self.ws().tabs[index].id));
                }
            }
            // Click collapses or expands; a double-click undoes the first
            // click's toggle and renames instead.
            Hit::Group(id) => {
                let double = self.last_group_click.is_some_and(|(at, g)| g == id && at.elapsed() < MULTI_CLICK);
                self.last_group_click = if double { None } else { Some((Instant::now(), id)) };
                self.toggle_group(id);
                if double {
                    self.start_group_rename(id);
                }
            }
            // On a pinned tab the button is the pin itself.
            Hit::Close(index) if self.ws().tabs[index].pinned => self.toggle_pin(index),
            Hit::Close(index) => self.close_tab(index),
            Hit::NewTab => {
                if let Err(err) = self.new_tab() {
                    self.set_status(format!("can't start a shell: {err}"));
                }
            }
            Hit::Divider => self.drag = Some(Drag::Divider),
            Hit::Pane => {}
        }
    }

    /// Continues or ends a drag; returns true when the event was consumed.
    fn handle_drag(&mut self, mouse: &MouseEvent, hit: Option<Hit>) -> bool {
        match (mouse.kind, self.drag) {
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::ActivityDivider)) => {
                self.resize_activity_bar(mouse.column.saturating_sub(self.layout.activity.x) + 1);
            }
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::Divider)) => {
                let width = mouse.column.saturating_sub(self.layout.sidebar.x) + 1;
                self.sidebar_width = width.max(layout::MIN_WIDTH);
                self.collapsed = false;
                self.relayout_now();
            }
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::Tab(id))) => {
                let Some(from) = self.ws().tabs.iter().position(|t| t.id == id) else { return true };
                match hit {
                    Some(Hit::Tab(to) | Hit::Close(to)) => self.move_tab(from, to),
                    Some(Hit::Group(group)) => self.drop_on_group(from, group),
                    // Past the last tab: to the end, out of any group.
                    Some(Hit::NewTab) => self.place_tab(from, usize::MAX, false, None),
                    _ => {}
                }
            }
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::Select)) => {
                let pane = self.layout.pane;
                let col = mouse.column.clamp(pane.left(), pane.right().saturating_sub(1)) - pane.x;
                let row = mouse.row.clamp(pane.top(), pane.bottom().saturating_sub(1)) - pane.y;
                if let Some(selection) = &mut self.selection {
                    selection.head = (row, col);
                }
            }
            (MouseEventKind::Up(_), Some(Drag::Select)) => {
                self.drag = None;
                self.copy_selection();
            }
            (MouseEventKind::Up(_), Some(_)) => self.drag = None,
            _ => return false,
        }
        true
    }

    fn mouse_in_pane(&mut self, mouse: MouseEvent) {
        let pane = self.layout.pane;
        let (col, row) = (mouse.column - pane.x, mouse.row - pane.y);
        // Scrolling moves the text out from under an underlined link.
        if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            self.hover_link = None;
        }
        let Some(tab) = self.ws_mut().active_tab_mut() else { return };
        let screen = tab.screen();
        let mode = screen.mouse_protocol_mode();
        // Shift bypasses the program's mouse handling, as in xterm.
        if mode != vt100::MouseProtocolMode::None
            && !mouse.modifiers.contains(KeyModifiers::SHIFT)
            && screen.scrollback() == 0
        {
            let encoding = screen.mouse_protocol_encoding();
            if let Some(bytes) = input::encode_mouse(&mouse, col, row, mode, encoding) {
                tab.write(bytes);
            }
            return;
        }

        match mouse.kind {
            // Underline the link under the mouse.
            MouseEventKind::Moved => {
                let link = links::at(screen, row, col).map(|link| (tab.id, link));
                self.hover_link = link;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if screen.alternate_screen() {
                    // Full-screen apps without mouse support (less, man) get arrow keys.
                    let key = KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down });
                    let bytes = input::encode_key(&key, screen.application_cursor()).repeat(WHEEL_LINES);
                    tab.write(bytes);
                } else {
                    let lines = WHEEL_LINES as isize;
                    self.scroll_active(if up { lines } else { -lines });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let tab_id = tab.id;
                let clicks = match self.last_click {
                    Some((at, pos, n)) if pos == (row, col) && at.elapsed() < MULTI_CLICK => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((Instant::now(), (row, col), clicks));
                // A second click selects; it doesn't open the link.
                if clicks > 1 {
                    self.pending_link = None;
                }
                let (anchor, head) = match clicks {
                    2 => match self.active_tab() {
                        Some(tab) => word_bounds(tab.screen(), row, col),
                        None => ((row, col), (row, col)),
                    },
                    3 => ((row, 0), (row, pane.width.saturating_sub(1))),
                    _ => ((row, col), (row, col)),
                };
                self.selection = Some(Selection { tab: tab_id, anchor, head });
                self.drag = Some(Drag::Select);
            }
            _ => {}
        }
    }

    fn copy_selection(&mut self) {
        let Some(selection) = self.selection else { return };
        if selection.anchor == selection.head {
            self.selection = None;
            // A plain click on a link opens it, unless a second click follows.
            let single = self.last_click.is_some_and(|(_, _, clicks)| clicks == 1);
            let (row, col) = selection.anchor;
            if single
                && let Some(tab) = self.active_tab().filter(|t| t.id == selection.tab)
                && let Some(link) = links::at(tab.screen(), row, col)
            {
                self.pending_link = Some((link.url, Instant::now()));
            }
            return;
        }
        let Some(tab) = self.active_tab().filter(|t| t.id == selection.tab) else { return };
        let ((start_row, start_col), (end_row, end_col)) = selection.ordered();
        let text = tab.screen().contents_between(start_row, start_col, end_row, end_col + 1);
        if text.is_empty() {
            return;
        }
        self.copy_to_clipboard(&text);
        let count = text.chars().count();
        self.set_status(format!("copied {count} character{}", if count == 1 { "" } else { "s" }));
    }
}

/// The run of non-separator characters around (row, col), for double-click.
fn word_bounds(screen: &vt100::Screen, row: u16, col: u16) -> ((u16, u16), (u16, u16)) {
    let is_word = |c: u16| {
        screen.cell(row, c).is_some_and(|cell| {
            let text = cell.contents();
            !text.is_empty() && !text.chars().any(|ch| ch.is_whitespace() || "\"'`()[]{}<>|,;".contains(ch))
        })
    };
    if !is_word(col) {
        return ((row, col), (row, col));
    }
    let (_, cols) = screen.size();
    let mut start = col;
    while start > 0 && is_word(start - 1) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cols && is_word(end + 1) {
        end += 1;
    }
    ((row, start), (row, end))
}
