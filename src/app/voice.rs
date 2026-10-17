//! Hearing the tower: with voice replies on, it says the big picture of
//! each answer aloud.

use super::App;
use crate::voice;

impl App {
    pub fn voice_replies(&self) -> bool {
        self.voice_replies
    }

    /// Whether the tower is talking, as last checked.
    pub fn voice_speaking(&self) -> bool {
        self.voice_speaking
    }

    /// Stops the tower talking.
    pub(super) fn hush(&mut self) {
        voice::hush();
        self.voice_speaking = false;
        self.set_status("stopped talking");
    }

    /// Checks whether the tower is talking, for its stop button.
    pub(super) fn refresh_voice(&mut self) {
        if self.tower_enabled {
            self.voice_speaking = voice::speaking_now();
        }
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
