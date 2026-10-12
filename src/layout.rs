//! Screen layout — workspace activity bar, tab sidebar, terminal pane — and
//! hit-testing for the mouse.

use ratatui::layout::{Position, Rect};

use crate::app::App;
use crate::workspace::GroupId;

/// The workspace bar with badges only: margin, badge, activity dot, edge.
pub const ACTIVITY_COMPACT: u16 = 7;
/// From this width on, the workspace bar shows names; narrower snaps back
/// to badges only.
pub const ACTIVITY_WIDE_MIN: u16 = 12;
pub const ACTIVITY_WIDE_DEFAULT: u16 = 24;
pub const ACTIVITY_MAX: u16 = 40;
pub const DEFAULT_WIDTH: u16 = 30;
pub const MIN_WIDTH: u16 = 16;
pub const COLLAPSED_WIDTH: u16 = 6;
/// Rows per workspace in the activity bar: the badge plus a gap.
const ACTIVITY_ROW: u16 = 2;

/// Something clickable on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Workspace(usize),
    AddWorkspace,
    HelpButton,
    /// The workspace bar's right edge, dragged to resize it.
    ActivityDivider,
    /// The current workspace's name, at the top of the tab sidebar.
    WorkspaceName,
    Toggle,
    Search,
    Tab(usize),
    /// The button on the right of a tab row: close, or unpin for pinned tabs.
    Close(usize),
    /// A tab group's header.
    Group(GroupId),
    NewTab,
    Divider,
    Pane,
}

/// What a row of the tab list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListItem {
    Tab(usize),
    /// A tab group's header, above its tabs.
    Group(GroupId),
    /// The line between pinned and regular tabs.
    Separator,
    NewTab,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Layout {
    pub area: Rect,
    pub activity: Rect,
    /// The last column of `activity`.
    pub activity_divider: Rect,
    pub sidebar: Rect,
    pub header: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub divider: Rect,
    pub pane: Rect,
    pub collapsed: bool,
}

impl Layout {
    /// `activity_width` 0 hides the workspace bar.
    pub fn new(area: Rect, activity_width: u16, width: u16, collapsed: bool) -> Self {
        // Wide workspace bars give way to the sidebar and terminal first.
        let activity_width = activity_width
            .min(area.width.saturating_sub(26).max(ACTIVITY_COMPACT))
            .min(area.width);
        let activity = Rect { width: activity_width, ..area };
        let activity_divider = Rect { x: activity.right().saturating_sub(1), width: activity_width.min(1), ..area };
        let rest = Rect { x: area.x + activity_width, width: area.width - activity_width, ..area };

        let width = if collapsed { COLLAPSED_WIDTH } else { width };
        // Always leave the terminal pane a usable amount of room.
        let width = width
            .min(rest.width.saturating_sub(20).max(COLLAPSED_WIDTH))
            .min(rest.width);
        let sidebar = Rect { width, ..rest };
        let pane = Rect { x: rest.x + width, width: rest.width - width, ..rest };
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
        Self { area, activity, activity_divider, sidebar, header, list, footer, divider, pane, collapsed }
    }

    pub fn toggle_button(&self) -> Rect {
        Rect::new(self.header.x + 1, self.header.y, 3.min(self.header.width), self.header.height.min(1))
    }

    /// Where the workspace's name sits in the header, between the buttons.
    pub fn workspace_name(&self) -> Rect {
        let search = self.search_button();
        if search.is_empty() {
            return Rect::default();
        }
        let x = self.toggle_button().right() + 1;
        Rect::new(x, self.header.y, search.x.saturating_sub(x + 1), 1)
    }

    pub fn search_button(&self) -> Rect {
        if self.collapsed || self.header.width < 10 {
            return Rect::default();
        }
        Rect::new(self.header.right() - 4, self.header.y, 3, self.header.height.min(1))
    }

    /// The highlighted "pill" a tab row is drawn in.
    pub fn pill(&self, row: u16) -> Rect {
        Rect::new(self.list.x + 1, row, self.list.width.saturating_sub(2), 1)
    }

    /// Screen row of list item `item`, if it's scrolled into view.
    pub fn row_of(&self, item: usize, scroll: usize) -> Option<u16> {
        let offset = item.checked_sub(scroll)?;
        (offset < self.list.height as usize).then(|| self.list.y + offset as u16)
    }

    /// Scroll offset of a tab list of `items` rows, clamped and moved to
    /// reveal item `reveal`.
    pub fn clamp_scroll(&self, scroll: usize, items: usize, reveal: Option<usize>) -> usize {
        let rows = (self.list.height as usize).max(1);
        let max = items.saturating_sub(rows);
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

    /// Last row (exclusive) usable for workspace badges; the bottom rows
    /// hold the help button.
    fn activity_items_bottom(&self) -> u16 {
        self.activity.bottom().saturating_sub(2).max(self.activity.y)
    }

    fn activity_capacity(&self) -> usize {
        (self.activity_items_bottom() - self.activity.y).div_ceil(ACTIVITY_ROW) as usize
    }

    /// Screen row of activity item `item` (workspaces, then the add button).
    pub fn activity_row(&self, item: usize, scroll: usize) -> Option<u16> {
        let offset = item.checked_sub(scroll)?;
        if offset >= self.activity_capacity() {
            return None;
        }
        Some(self.activity.y + offset as u16 * ACTIVITY_ROW)
    }

    pub fn badge(&self, row: u16) -> Rect {
        Rect::new(self.activity.x + 1, row, self.activity.width.saturating_sub(3).min(4), 1)
    }

    /// Whether the workspace bar is wide enough to show names.
    pub fn activity_wide(&self) -> bool {
        self.activity.width >= ACTIVITY_WIDE_MIN
    }

    /// Room for a name next to the badge on `row`, before the activity dot.
    pub fn activity_label(&self, row: u16) -> Rect {
        if !self.activity_wide() {
            return Rect::default();
        }
        let x = self.badge(row).right() + 1;
        Rect::new(x, row, self.activity_marker_x().saturating_sub(x + 1), 1)
    }

    /// Column of the dot or spinner showing what a workspace is up to.
    pub fn activity_marker_x(&self) -> u16 {
        if self.activity_wide() { self.activity_divider.x - 2 } else { self.activity.x + 5 }
    }

    pub fn help_button(&self) -> Rect {
        if self.activity.height < 4 {
            return Rect::default();
        }
        self.badge(self.activity.bottom() - 1)
    }

    pub fn clamp_activity_scroll(&self, scroll: usize, workspaces: usize, reveal: Option<usize>) -> usize {
        let rows = self.activity_capacity().max(1);
        // Workspaces plus the add button.
        let max = (workspaces + 1).saturating_sub(rows);
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

    pub fn hit(&self, x: u16, y: u16, app: &App) -> Option<Hit> {
        let pos = Position::new(x, y);
        if self.pane.contains(pos) {
            return Some(Hit::Pane);
        }
        if self.activity_divider.contains(pos) {
            return Some(Hit::ActivityDivider);
        }
        if self.activity.contains(pos) {
            return self.hit_activity(pos, app);
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
        if self.workspace_name().contains(pos) {
            return Some(Hit::WorkspaceName);
        }
        if !self.list.contains(pos) {
            return None;
        }
        let ws = app.ws();
        let items = ws.list_items();
        match items.get(ws.scroll + (y - self.list.y) as usize)? {
            ListItem::Tab(index) => {
                let pill = self.pill(y);
                if !self.collapsed && x + 4 >= pill.right() && x < pill.right() {
                    Some(Hit::Close(*index))
                } else {
                    Some(Hit::Tab(*index))
                }
            }
            ListItem::Group(id) => Some(Hit::Group(*id)),
            ListItem::NewTab => Some(Hit::NewTab),
            ListItem::Separator => None,
        }
    }

    fn hit_activity(&self, pos: Position, app: &App) -> Option<Hit> {
        if self.help_button().contains(pos) {
            return Some(Hit::HelpButton);
        }
        let offset = pos.y - self.activity.y;
        if !offset.is_multiple_of(ACTIVITY_ROW) || pos.y >= self.activity_items_bottom() {
            return None;
        }
        let item = app.activity_scroll + (offset / ACTIVITY_ROW) as usize;
        match item.cmp(&app.workspaces.len()) {
            std::cmp::Ordering::Less => Some(Hit::Workspace(item)),
            std::cmp::Ordering::Equal => Some(Hit::AddWorkspace),
            std::cmp::Ordering::Greater => None,
        }
    }
}
