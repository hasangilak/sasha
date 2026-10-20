//! What sasha remembers between runs: the workspaces, their tabs and groups,
//! and the layout, stored as JSON in ~/.config/sasha/state.json.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agents::Conversation;
use crate::mission::{Ended, Mission};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
    /// Index of the workspace that was on screen.
    #[serde(default)]
    pub current: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<SavedLayout>,
    /// Tab groups, which can hold tabs from any workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<SavedGroup>,
    /// Keeping the computer awake was on: it's switched on again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_awake: bool,
    /// The tower's sessions whose terminals are gone, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_missions: Vec<Ended>,
}

/// Sidebar and workspace bar sizes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SavedLayout {
    pub sidebar_width: u16,
    pub collapsed: bool,
    pub activity_width: u16,
    pub wide_activity_width: u16,
    /// The sidebar shows only the current workspace's tabs.
    #[serde(default)]
    pub current_only: bool,
    /// The ports and pull request bars are hidden (older versions hid
    /// them together, and only that).
    #[serde(default)]
    pub hide_side_bars: bool,
    #[serde(default)]
    pub hide_ports: bool,
    #[serde(default)]
    pub hide_prs: bool,
    /// Their widths (0: as they start).
    #[serde(default)]
    pub ports_width: u16,
    #[serde(default)]
    pub prs_width: u16,
    /// The files bar is hidden.
    #[serde(default)]
    pub hide_files: bool,
    #[serde(default)]
    pub files_width: u16,
    /// The tower was opened: it starts with sasha.
    #[serde(default)]
    pub tower: bool,
    /// Its panel is hidden.
    #[serde(default)]
    pub hide_tower: bool,
    #[serde(default)]
    pub tower_width: u16,
    /// News from its sessions goes straight to the tower when it's idle.
    #[serde(default)]
    pub tower_autopilot: bool,
    /// The tower says the big picture of its answers aloud.
    #[serde(default)]
    pub tower_voice: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub path: PathBuf,
    /// The name the user gave it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Every tab, in order; they come back as fresh shells.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<SavedTab>,
    /// Older versions kept groups per workspace, here.
    #[serde(default, skip_serializing)]
    pub groups: Vec<SavedGroup>,
    /// Index of the active tab.
    #[serde(default)]
    pub active: usize,
    /// Older versions saved only the pinned tabs, here.
    #[serde(default, skip_serializing)]
    pub pinned: Vec<SavedTab>,
}

impl State {
    /// Moves groups that older versions kept per workspace up to the state,
    /// where they can hold tabs from any workspace.
    fn lift_groups(&mut self) {
        for ws in &mut self.workspaces {
            if ws.groups.is_empty() {
                continue;
            }
            let offset = self.groups.len();
            self.groups.append(&mut ws.groups);
            for tab in ws.tabs.iter_mut().chain(&mut ws.pinned) {
                tab.group = tab.group.map(|g| g + offset);
            }
        }
    }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedTab {
    /// The folder it was in.
    pub cwd: PathBuf,
    /// The prefix the user gave the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Index into the state's `groups`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<usize>,
    /// The tower's tab, from the version that kept it in a workspace.
    #[serde(default, skip_serializing)]
    pub tower: bool,
    /// What the tower set it to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<Mission>,
    /// The Claude Code or Codex conversation it was having: it's resumed
    /// when the tab comes back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Conversation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    match serde_json::from_str::<State>(&text) {
        Ok(mut state) => {
            state.lift_groups();
            (state, None)
        }
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

    #[test]
    fn lifts_per_workspace_groups() {
        let old = r#"{"workspaces":[
            {"path":"/a","tabs":[{"cwd":"/a","group":0}],"groups":[{"name":"x","color":1}]},
            {"path":"/b","tabs":[{"cwd":"/b"},{"cwd":"/b","group":0}],"groups":[{"name":"y","color":2}]}]}"#;
        let mut state: State = serde_json::from_str(old).unwrap();
        state.lift_groups();
        let names: Vec<_> = state.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["x", "y"]);
        assert_eq!(state.workspaces[0].tabs[0].group, Some(0));
        assert_eq!(state.workspaces[1].tabs[1].group, Some(1));
    }
}
