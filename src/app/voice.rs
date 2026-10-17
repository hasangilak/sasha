//! Talking to the tower: the prefix, then V, records you; again, Whisper
//! hears you and what you said goes to the tower as a prompt. And hearing
//! it: with voice replies on, it says the big picture of each answer.

use std::time::{Duration, Instant};

use super::{App, AppEvent};
use crate::{agents, voice};

/// Between typing what you said into the tower and the Enter that sends it.
const BEFORE_ENTER: Duration = Duration::from_millis(300);

/// Whether sasha is listening to you.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listening {
    No,
    /// Recording, for so long.
    Recording(Duration),
    /// Working out what you said.
    Transcribing,
}

impl App {
    pub fn listening(&self) -> Listening {
        match (&self.recording, self.transcribing) {
            (Some(recording), _) => Listening::Recording(recording.since.elapsed()),
            (None, true) => Listening::Transcribing,
            (None, false) => Listening::No,
        }
    }

    pub fn voice_replies(&self) -> bool {
        self.voice_replies
    }

    /// Starts listening, or sends what you said.
    pub(super) fn talk(&mut self) {
        if let Some(recording) = self.recording.take() {
            self.transcribing = true;
            let events = self.events_tx.clone();
            std::thread::spawn(move || {
                let heard = recording.finish().and_then(|wav| voice::transcribe(&wav)).map_err(|err| format!("{err:#}"));
                let _ = events.send(AppEvent::Heard(heard));
            });
            return self.set_status("working out what you said…");
        }
        if self.transcribing {
            return self.set_status("still working out what you said");
        }
        if let Some(missing) = voice::missing() {
            return self.set_status(missing);
        }
        // It shouldn't talk over you.
        voice::hush();
        match voice::record() {
            Ok(recording) => {
                self.recording = Some(recording);
                self.show_tower();
                let prefix = self.config.prefix.label();
                self.set_status(format!("listening · {prefix} V sends it to the tower"));
            }
            Err(err) => self.set_status(format!("can't listen: {err:#}")),
        }
    }

    /// Stops listening without sending anything.
    pub(super) fn stop_listening(&mut self) {
        if self.recording.take().is_some() {
            self.set_status("stopped listening");
        }
    }

    /// What Whisper heard: it goes to the tower.
    pub(super) fn heard(&mut self, heard: Result<String, String>) {
        self.transcribing = false;
        let text = match heard {
            Ok(text) if text.trim().is_empty() => return self.set_status("didn't catch anything"),
            Ok(text) => text,
            Err(err) => return self.set_status(format!("couldn't hear you: {err}")),
        };
        if self.tower.is_none() {
            self.start_tower(None);
        }
        if let Some(tower) = &self.tower {
            tower.paste(&text);
            self.tower_enter_at = Some(Instant::now() + BEFORE_ENTER);
        }
        self.show_tower();
        self.set_status(format!("you said: {}", agents::clip(&text, 80)));
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
