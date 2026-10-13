//! A workspace is a folder on disk with its own set of tabs.

use std::path::{Path, PathBuf};

use ratatui::style::Color;

use crate::layout::ListItem;
use crate::store::SavedWorkspace;
use crate::tab::{Tab, TabId};
use crate::theme;

pub type WorkspaceId = u64;
pub type GroupId = u64;

/// A named, coloured set of tabs, from any workspaces, like a Chrome tab
/// group: workspaces sort tabs by folder, groups by what you're doing.
pub struct Group {
    pub id: GroupId,
    /// May be empty: the header then shows just the colour.
    pub name: String,
    /// Index into `Palette::group_colors`.
    pub color: usize,
    /// Collapsed groups show only their header, plus the tab on screen when
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
    /// Pinned tabs always come first. Grouped tabs are listed under their
    /// group, in this order.
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// The tab that was active before the current one.
    pub previous: Option<TabId>,
    /// Tabs saved last time and not reopened yet: they come back the first
    /// time the workspace is opened.
    pub dormant: Option<SavedWorkspace>,
    /// Its section was closed from the stacked sidebar; going to the
    /// workspace shows it again.
    pub section_hidden: bool,
}

impl Workspace {
    pub fn new(id: WorkspaceId, path: PathBuf) -> Self {
        Self {
            id,
            name: display_name(&path),
            custom_name: None,
            path,
            tabs: Vec::new(),
            active: 0,
            previous: None,
            dormant: None,
            section_hidden: false,
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

    /// Rows of the workspace's section of the sidebar: pinned tabs, a
    /// separator, the others (grouped ones too: a workspace lists every
    /// terminal in its folder), then "+".
    pub fn list_items(&self) -> Vec<ListItem> {
        let pinned: Vec<bool> = self.tabs.iter().map(|t| t.pinned).collect();
        list_items(&pinned)
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

/// Rows for tabs that are pinned or not: the pinned ones, a separator, the
/// others, then "+".
fn list_items(pinned: &[bool]) -> Vec<ListItem> {
    let count = pinned.iter().take_while(|p| **p).count();
    let mut items: Vec<ListItem> = (0..count).map(ListItem::Tab).collect();
    if count > 0 {
        items.push(ListItem::Separator);
    }
    items.extend((count..pinned.len()).map(ListItem::Tab));
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
    fn pinned_tabs_come_first() {
        use ListItem::{NewTab, Separator, Tab as T};
        assert_eq!(list_items(&[true, false, false]), [T(0), Separator, T(1), T(2), NewTab]);
        assert_eq!(list_items(&[false]), [T(0), NewTab]);
        assert_eq!(list_items(&[]), [NewTab]);
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
