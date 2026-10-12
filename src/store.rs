//! What sasha remembers between runs: the workspaces and their pinned
//! tabs, stored as JSON in ~/.config/sasha/state.json.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub path: PathBuf,
    /// The name the user gave it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<SavedTab>,
}

/// A pinned tab: reopened as a fresh shell in `cwd` on startup.
#[derive(Debug, Serialize, Deserialize)]
pub struct SavedTab {
    pub cwd: PathBuf,
    /// The prefix the user gave the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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

pub fn save(state: &State) -> Result<()> {
    let path = path().context("no home directory to save state in")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    // Write then rename, so a crash never leaves a half-written file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
