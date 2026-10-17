//! What the tower set a terminal to do, and how that's going, kept by sasha
//! so the tower knows what its sessions are up to, between its turns and
//! across restarts. Agents the tower opens report through hooks
//! (`sasha event`); other terminals it prompts are watched for news.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agents;
use crate::tab::TabId;

/// The most updates kept for the tower, and ended missions remembered.
pub const INBOX_MAX: usize = 50;
pub const ENDED_MAX: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    /// At work on its task.
    Working,
    /// Stopped to ask for an approval or an answer.
    Waiting,
    /// Finished its turn.
    Done,
    /// Its terminal closed, or sasha restarted under it.
    Ended,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting for approval",
            State::Done => "done",
            State::Ended => "ended",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mission {
    /// What it was last asked, in short.
    pub task: String,
    pub state: State,
    /// When it got there, in seconds since 1970, so it lasts restarts.
    pub since: u64,
    /// The gist of its last answer, what it waits for, or how it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Its agent reports through hooks: its word, not a guess.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hooked: bool,
    /// The agent's session log, as its hooks said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
}

impl Mission {
    pub fn new(task: &str) -> Self {
        Self { task: brief(task), state: State::Working, since: now(), note: None, hooked: false, log: None }
    }

    pub fn set(&mut self, state: State, note: Option<String>) {
        self.state = state;
        self.since = now();
        self.note = note;
    }

    /// How long it's been in its state.
    pub fn age(&self) -> std::time::Duration {
        std::time::Duration::from_secs(now().saturating_sub(self.since))
    }
}

/// A mission whose terminal is gone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ended {
    pub id: TabId,
    pub name: String,
    pub workspace: String,
    pub mission: Mission,
}

/// What an agent's hooks say, through `sasha event`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Signal {
    /// A prompt went in: it's at work.
    Started { prompt: String },
    /// Its turn ended, with what it did, in short.
    Finished { summary: String, log: Option<PathBuf> },
    /// It stopped to ask for something.
    Waiting { what: String, log: Option<PathBuf> },
}

/// News for the tower about one of its sessions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Update {
    pub id: TabId,
    pub text: String,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A task in a line or two.
pub fn brief(text: &str) -> String {
    let gist = agents::gist(text);
    agents::clip(if gist.is_empty() { text.trim() } else { &gist }, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missions_start_at_work_and_keep_their_task_short() {
        let mut mission = Mission::new(&format!("Fix the build.\n\n{}", "and more ".repeat(60)));
        assert_eq!(mission.state, State::Working);
        assert!(mission.task.chars().count() <= 200);
        mission.set(State::Done, Some("All green".to_owned()));
        assert_eq!((mission.state, mission.note.as_deref()), (State::Done, Some("All green")));
        assert!(mission.age().as_secs() < 2);
    }
}
