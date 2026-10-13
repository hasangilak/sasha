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
/// The ports bar, with its edge: as it starts, and how far it can be
/// dragged.
pub const PORTS_WIDTH: u16 = 9;
pub const PORTS_MIN: u16 = 7;
pub const PORTS_MAX: u16 = 40;
/// The pull request bar, with its edge.
pub const PRS_WIDTH: u16 = 32;
pub const PRS_MIN: u16 = 16;
pub const PRS_MAX: u16 = 100;
/// Narrower windows leave out the ports bar, and then the pull request bar.
const PORTS_MIN_AREA: u16 = 80;
const PRS_MIN_AREA: u16 = 120;
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
    /// The × on a workspace's heading in the tab sidebar.
    CloseWorkspace(usize),
    /// The – beside it.
    MinimizeWorkspace(usize),
    /// The keep-awake button at the top of the workspace bar.
    Awake,
    /// A port (by position) in the ports bar.
    Port(usize),
    /// The ports bar's right edge, dragged to resize it.
    PortsDivider,
    /// The pull request bar's left edge, dragged to resize it.
    PrsDivider,
    /// A pull request (by position) in the pull request bar.
    Pr(usize),
    /// The pull request bar's heading: refreshes the list.
    PrHeading,
    /// A tab (workspace, tab) of another workspace, in the tab sidebar.
    OtherTab(usize, usize),
    /// A workspace's heading in the stacked tab sidebar.
    Section(usize),
    Toggle,
    Search,
    Tab(usize),
    /// The button on the right of a tab row: close, or unpin for pinned tabs.
    Close(usize),
    /// A tab group's header.
    Group(GroupId),
    /// The + on a group's header: a new terminal in the group.
    GroupNewTab(GroupId),
    /// The × on a group's header: deletes the group.
    GroupDelete(GroupId),
    NewTab,
    Divider,
    Pane,
}

/// What a row of the tab list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListItem {
    Tab(usize),
    /// The line under a tab that says what's running in it.
    Running(usize),
    /// The line between pinned and regular tabs.
    Separator,
    NewTab,
}

/// A row of the tab sidebar: a group's header or a workspace's heading,
/// a tab (or separator, or "+") of a workspace, or a gap between sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Group(GroupId),
    Workspace(usize),
    Item(usize, ListItem),
    Gap,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Layout {
    pub area: Rect,
    pub activity: Rect,
    /// The last column of `activity`.
    pub activity_divider: Rect,
    /// The slim bar of open ports, right of the workspace bar.
    pub ports_bar: Rect,
    /// The open pull requests, right of the terminal pane.
    pub pr_bar: Rect,
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
    /// `bars` are the widths of the ports and pull request bars (0 hides
    /// one); they're left out where there isn't room.
    pub fn new(area: Rect, activity_width: u16, width: u16, collapsed: bool, bars: (u16, u16)) -> Self {
        // Wide workspace bars give way to the sidebar and terminal first.
        let activity_width = activity_width
            .min(area.width.saturating_sub(26).max(ACTIVITY_COMPACT))
            .min(area.width);
        let activity = Rect { width: activity_width, ..area };
        let activity_divider = Rect { x: activity.right().saturating_sub(1), width: activity_width.min(1), ..area };
        let rest = Rect { x: area.x + activity_width, width: area.width - activity_width, ..area };
        let ports_width = if area.width >= PORTS_MIN_AREA { bars.0.min(rest.width / 4) } else { 0 };
        let ports_bar = Rect { width: ports_width, ..rest };
        let prs_width = if area.width >= PRS_MIN_AREA { bars.1.min(rest.width / 2) } else { 0 };
        let rest = Rect { x: rest.x + ports_width, width: rest.width - ports_width - prs_width, ..rest };
        let pr_bar = Rect { x: rest.right(), width: prs_width, ..rest };

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
        Self {
            area,
            activity,
            activity_divider,
            ports_bar,
            pr_bar,
            sidebar,
            header,
            list,
            footer,
            divider,
            pane,
            collapsed,
        }
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

    /// The keep-awake button, at the top of the workspace bar, above the
    /// workspaces (when the bar is tall enough to spare the rows).
    pub fn awake_button(&self) -> Rect {
        if self.activity.is_empty() || self.activity.height < 8 {
            return Rect::default();
        }
        Rect::new(self.activity.x + 1, self.activity.y, self.activity.width.saturating_sub(2), 1)
    }

    /// First row for workspace badges, below the keep-awake button and a
    /// rule.
    fn activity_items_top(&self) -> u16 {
        if self.awake_button().is_empty() { self.activity.y } else { self.activity.y + 2 }
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
        self.activity_items_bottom().saturating_sub(self.activity_items_top()).div_ceil(ACTIVITY_ROW) as usize
    }

    /// Screen row of activity item `item` (workspaces, then the add button).
    pub fn activity_row(&self, item: usize, scroll: usize) -> Option<u16> {
        let offset = item.checked_sub(scroll)?;
        if offset >= self.activity_capacity() {
            return None;
        }
        Some(self.activity_items_top() + offset as u16 * ACTIVITY_ROW)
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
        if self.ports_bar.width > 0 && x == self.ports_bar.right() - 1 {
            return Some(Hit::PortsDivider);
        }
        if self.pr_bar.width > 0 && x == self.pr_bar.x && y > self.pr_bar.y {
            return Some(Hit::PrsDivider);
        }
        if self.ports_bar.contains(pos) {
            let index = (y.checked_sub(self.ports_bar.y + 2)?) as usize;
            return (index < app.port_count()).then_some(Hit::Port(index));
        }
        if self.pr_bar.contains(pos) {
            if y == self.pr_bar.y {
                return Some(Hit::PrHeading);
            }
            let index = (y.checked_sub(self.pr_bar.y + 2)?) as usize;
            return (index < app.bar_prs().map_or(0, <[_]>::len)).then_some(Hit::Pr(index));
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
        let current = app.current;
        match *app.sidebar_rows().get(app.list_scroll + (y - self.list.y) as usize)? {
            // – and × at the right of a heading.
            Row::Workspace(w) => {
                let pill = self.pill(y);
                let from_right = pill.right().saturating_sub(x);
                match from_right {
                    1..=3 if !self.collapsed => Some(Hit::CloseWorkspace(w)),
                    4..=5 if !self.collapsed => Some(Hit::MinimizeWorkspace(w)),
                    _ => Some(Hit::Section(w)),
                }
            }
            Row::Item(w, ListItem::Tab(index)) if w == current => {
                let pill = self.pill(y);
                if !self.collapsed && x + 4 >= pill.right() && x < pill.right() {
                    Some(Hit::Close(index))
                } else {
                    Some(Hit::Tab(index))
                }
            }
            // The line under a tab is part of it.
            Row::Item(w, ListItem::Running(index)) if w == current => Some(Hit::Tab(index)),
            Row::Item(w, ListItem::Tab(index) | ListItem::Running(index)) => Some(Hit::OtherTab(w, index)),
            // + and × left of a group header's chevron.
            Row::Group(id) => {
                let from_right = self.pill(y).right().saturating_sub(x);
                match from_right {
                    5..=6 if !self.collapsed => Some(Hit::GroupNewTab(id)),
                    3..=4 if !self.collapsed => Some(Hit::GroupDelete(id)),
                    _ => Some(Hit::Group(id)),
                }
            }
            Row::Item(_, ListItem::NewTab) => Some(Hit::NewTab),
            Row::Item(_, ListItem::Separator) | Row::Gap => None,
        }
    }

    fn hit_activity(&self, pos: Position, app: &App) -> Option<Hit> {
        if self.help_button().contains(pos) {
            return Some(Hit::HelpButton);
        }
        if self.awake_button().contains(pos) {
            return Some(Hit::Awake);
        }
        let offset = pos.y.checked_sub(self.activity_items_top())?;
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
