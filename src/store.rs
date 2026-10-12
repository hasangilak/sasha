//! What sasha remembers between runs: the workspaces, their tabs and groups,
//! and the layout, stored as JSON in ~/.config/sasha/state.json.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
    /// Index of the workspace that was on screen.
    #[serde(default)]
    pub current: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<SavedLayout>,
}

/// Sidebar and workspace bar sizes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SavedLayout {
    pub sidebar_width: u16,
    pub collapsed: bool,
    pub activity_width: u16,
    pub wide_activity_width: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub path: PathBuf,
    /// The name the user gave it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Every tab, in order; they come back as fresh shells.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<SavedTab>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<SavedGroup>,
    /// Index of the active tab.
    #[serde(default)]
    pub active: usize,
    /// Older versions saved only the pinned tabs, here.
    #[serde(default, skip_serializing)]
    pub pinned: Vec<SavedTab>,
}

impl SavedWorkspace {
    /// The tabs to reopen, from either format.
    pub fn take_tabs(&mut self) -> Vec<SavedTab> {
        if self.tabs.is_empty() {
            for tab in &mut self.pinned {
                tab.pinned = true;
            }
            return std::mem::take(&mut self.pinned);
        }
        std::mem::take(&mut self.tabs)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedTab {
    /// The folder it was in.
    pub cwd: PathBuf,
    /// The prefix the user gave the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Index into the workspace's `groups`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedGroup {
    pub name: String,
    pub color: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
}

/// `$SASHA_STATE`, else `$XDG_CONFIG_HOME/sasha/state.json`, else
/// `~/.config/sasha/state.json`.
pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SASHA_STATE") {
        return Some(PathBuf::from(path));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
    Some(config.join("sasha").join("state.json"))
}

/// Loads the saved state. A missing file is a fresh start; an unreadable
/// one is moved aside (so it isn't overwritten) and reported.
pub fn load() -> (State, Option<String>) {
    let Some(path) = path() else { return (State::default(), None) };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (State::default(), None),
        Err(err) => return (State::default(), Some(format!("can't read {}: {err}", path.display()))),
    };
    match serde_json::from_str(&text) {
        Ok(state) => (state, None),
        Err(err) => {
            let aside = path.with_extension("json.broken");
            let _ = std::fs::rename(&path, &aside);
            (State::default(), Some(format!("ignored broken state ({err}); kept it as {}", aside.display())))
        }
    }
}

pub fn encode(state: &State) -> Result<String> {
    Ok(serde_json::to_string_pretty(state)? + "\n")
}

pub fn write(json: &str) -> Result<()> {
    let path = path().context("no home directory to save state in")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    // Write then rename, so a crash never leaves a half-written file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_old_pinned_only_format() {
        let old = r#"{"workspaces":[{"path":"/w","pinned":[{"cwd":"/w/a","title":"api"}]}]}"#;
        let mut state: State = serde_json::from_str(old).unwrap();
        let tabs = state.workspaces[0].take_tabs();
        assert_eq!(tabs.len(), 1);
        assert!(tabs[0].pinned);
        assert_eq!(tabs[0].title.as_deref(), Some("api"));
        // The old field isn't written back.
        assert!(!encode(&state).unwrap().contains("pinned"));
    }
}
