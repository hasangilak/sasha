//! A workspace is a folder on disk with its own set of tabs.

use std::ops::Range;
use std::path::{Path, PathBuf};

use ratatui::style::Color;

use crate::layout::ListItem;
use crate::store::SavedWorkspace;
use crate::tab::{Tab, TabId};
use crate::theme;

pub type WorkspaceId = u64;
pub type GroupId = u64;

/// A named, coloured run of neighbouring tabs, like a Chrome tab group.
pub struct Group {
    pub id: GroupId,
    /// May be empty: the header then shows just the colour.
    pub name: String,
    /// Index into `Palette::group_colors`.
    pub color: usize,
    /// Collapsed groups show only their header, plus the active tab when
    /// it's one of theirs.
    pub collapsed: bool,
    /// The tab to go to when the collapsed group is selected.
    pub last_active: Option<TabId>,
}

impl Group {
    pub fn color(&self) -> Color {
        let colors = theme::palette().group_colors();
        colors[self.color % colors.len()]
    }
}

pub struct Workspace {
    pub id: WorkspaceId,
    /// The name the user gave it, else the folder's name.
    pub name: String,
    pub custom_name: Option<String>,
    pub path: PathBuf,
    /// Pinned tabs always come first, and the tabs of a group are always
    /// next to each other.
    pub tabs: Vec<Tab>,
    /// In no particular order: the tabs decide where a group appears.
    pub groups: Vec<Group>,
    pub active: usize,
    /// The tab that was active before the current one.
    pub previous: Option<TabId>,
    /// Tabs saved last time and not reopened yet: they come back the first
    /// time the workspace is opened.
    pub dormant: Option<SavedWorkspace>,
}

impl Workspace {
    pub fn new(id: WorkspaceId, path: PathBuf) -> Self {
        Self {
            id,
            name: display_name(&path),
            custom_name: None,
            path,
            tabs: Vec::new(),
            groups: Vec::new(),
            active: 0,
            previous: None,
            dormant: None,
        }
    }

    /// Names the workspace; `None` (or an empty name) goes back to the
    /// folder's name.
    pub fn rename(&mut self, name: Option<String>) {
        self.custom_name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        self.name = self.custom_name.clone().unwrap_or_else(|| display_name(&self.path));
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn pinned_count(&self) -> usize {
        self.tabs.iter().take_while(|t| t.pinned).count()
    }

    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn group_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// The group of tab `index`, if it has one.
    pub fn group_of(&self, index: usize) -> Option<&Group> {
        self.group(self.tabs.get(index)?.group?)
    }

    /// Indices of the tabs in group `id`.
    pub fn group_range(&self, id: GroupId) -> Range<usize> {
        let start = self.tabs.iter().position(|t| t.group == Some(id)).unwrap_or(self.tabs.len());
        let len = self.tabs[start..].iter().take_while(|t| t.group == Some(id)).count();
        start..start + len
    }

    /// Groups in the order they appear in the tab list.
    pub fn groups_in_order(&self) -> Vec<&Group> {
        let mut ids: Vec<GroupId> = self.tabs.iter().filter_map(|t| t.group).collect();
        ids.dedup();
        ids.into_iter().filter_map(|id| self.group(id)).collect()
    }

    /// Forgets groups whose last tab has gone.
    pub fn prune_groups(&mut self) {
        let tabs = &self.tabs;
        self.groups.retain(|g| tabs.iter().any(|t| t.group == Some(g.id)));
    }

    /// Rows of the tab list.
    pub fn list_items(&self) -> Vec<ListItem> {
        let tabs: Vec<_> = self.tabs.iter().map(|t| (t.pinned, t.group)).collect();
        let collapsed = |id| self.group(id).is_some_and(|g| g.collapsed);
        list_items(&tabs, collapsed, self.active)
    }

    /// Where keyboard navigation stops, in order: every visible tab, and
    /// one tab standing in for each collapsed group.
    pub fn stops(&self) -> Vec<usize> {
        self.list_items()
            .into_iter()
            .filter_map(|item| match item {
                ListItem::Tab(index) => Some(index),
                ListItem::Group(id) => {
                    let group = self.group(id)?;
                    let range = self.group_range(id);
                    if !group.collapsed || range.contains(&self.active) {
                        return None;
                    }
                    let last = range.clone().find(|&i| Some(self.tabs[i].id) == group.last_active);
                    Some(last.unwrap_or(range.start))
                }
                ListItem::Separator | ListItem::NewTab => None,
            })
            .collect()
    }

    /// Up to two letters for the activity bar badge: the initials of the
    /// folder name ("web-dashboard" → "WD", "sasha" → "SA").
    pub fn initials(&self) -> String {
        if self.custom_name.is_none() && is_home(&self.path) {
            return "~".to_owned();
        }
        let words: Vec<&str> = self
            .name
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let initials: String = match words.as_slice() {
            [] => self.name.chars().take(2).collect(),
            [word] => word.chars().take(2).collect(),
            [first, second, ..] => first.chars().take(1).chain(second.chars().take(1)).collect(),
        };
        initials.to_uppercase()
    }

    /// A colour derived from the path, so a workspace keeps its colour.
    pub fn color(&self) -> Color {
        let colors = theme::palette().badge_colors();
        // FNV-1a, stable across runs (unlike std's hasher).
        let hash = self
            .path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf29ce484222325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
        colors[(hash % colors.len() as u64) as usize]
    }
}

/// Rows of a tab list whose tabs are (pinned, group): pinned tabs, a
/// separator, the other tabs with a header above each group, then "+".
/// A collapsed group hides its tabs, except the active one.
fn list_items(tabs: &[(bool, Option<GroupId>)], collapsed: impl Fn(GroupId) -> bool, active: usize) -> Vec<ListItem> {
    let pinned = tabs.iter().take_while(|(pinned, _)| *pinned).count();
    let mut items: Vec<ListItem> = (0..pinned).map(ListItem::Tab).collect();
    if pinned > 0 {
        items.push(ListItem::Separator);
    }
    let mut previous = None;
    for (index, &(_, group)) in tabs.iter().enumerate().skip(pinned) {
        if let Some(id) = group
            && group != previous
        {
            items.push(ListItem::Group(id));
        }
        previous = group;
        if !group.is_some_and(&collapsed) || index == active {
            items.push(ListItem::Tab(index));
        }
    }
    items.push(ListItem::NewTab);
    items
}

fn is_home(path: &Path) -> bool {
    std::env::home_dir().is_some_and(|home| home == path)
}

fn display_name(path: &Path) -> String {
    if is_home(path) {
        return "home".to_owned();
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// A path with the home directory shown as `~`.
pub fn pretty_path(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(ToOwned::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Expands `~` and resolves relative paths against `base`.
pub fn expand_path(input: &str, base: &Path) -> PathBuf {
    let home = std::env::home_dir();
    match (input, home) {
        ("~", Some(home)) => home,
        (rest, Some(home)) if rest.starts_with("~/") => home.join(&rest[2..]),
        (path, _) if path.starts_with('/') => PathBuf::from(path),
        (path, _) => base.join(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_from_folder_names() {
        let initials = |p: &str| Workspace::new(0, PathBuf::from(p)).initials();
        assert_eq!(initials("/work/web-dashboard-api"), "WD");
        assert_eq!(initials("/work/sasha"), "SA");
        assert_eq!(initials("/work/my_app"), "MA");
        assert_eq!(initials("/work/x"), "X");
        assert_eq!(initials("/work/.dotfiles"), "DO");
    }

    #[test]
    fn groups_get_headers_and_collapse() {
        use ListItem::{Group as G, NewTab, Separator, Tab as T};
        let tabs = [(true, None), (false, None), (false, Some(7)), (false, Some(7)), (false, Some(9)), (false, None)];
        assert_eq!(list_items(&tabs, |_| false, 0), [T(0), Separator, T(1), G(7), T(2), T(3), G(9), T(4), T(5), NewTab]);
        // Collapsed, group 7 keeps only the active tab on show.
        assert_eq!(list_items(&tabs, |id| id == 7, 3), [T(0), Separator, T(1), G(7), T(3), G(9), T(4), T(5), NewTab]);
        assert_eq!(list_items(&tabs, |id| id == 7, 1), [T(0), Separator, T(1), G(7), G(9), T(4), T(5), NewTab]);
        assert_eq!(list_items(&[], |_| false, 0), [NewTab]);
    }

    #[test]
    fn expands_paths() {
        let base = Path::new("/base");
        assert_eq!(expand_path("/abs", base), PathBuf::from("/abs"));
        assert_eq!(expand_path("rel/dir", base), PathBuf::from("/base/rel/dir"));
        if let Some(home) = std::env::home_dir() {
            assert_eq!(expand_path("~", base), home);
            assert_eq!(expand_path("~/x", base), home.join("x"));
        }
    }
}
