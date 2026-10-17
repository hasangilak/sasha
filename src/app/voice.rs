//! Hearing the tower: with voice replies on, it says the big picture of
//! each answer aloud.

use super::App;
use crate::voice;

impl App {
    pub fn voice_replies(&self) -> bool {
        self.voice_replies
    }

    /// Voice replies: the tower says the big picture of its answers.
    pub(super) fn toggle_voice(&mut self) {
        self.voice_replies = !self.voice_replies;
        voice::set_replies(self.voice_replies);
        if !self.voice_replies {
            voice::hush();
        }
        let state = if self.voice_replies { "on: the tower says the gist of its answers" } else { "off" };
        self.set_status(format!("voice replies {state}"));
        self.save();
    }
}
