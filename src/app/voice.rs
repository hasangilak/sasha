//! Hearing the tower: with voice replies on, it says the big picture of
//! each answer aloud; prefix, then S, or saying "hush" (to Claude Code, or
//! out loud while it talks: the speaker listens), stops it. And its
//! control center (prefix, then V): what it's saying, with its sound
//! drawn as it plays, ▶ and ■, and every message it has said, to play
//! again.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Position;

use super::state::Mode;
use super::{App, Panel, VoicePanel};
use crate::{agents, panel_view, voice};

/// How long after it heard you a newly opened sasha still says so.
const HEARD_SHOWN: f64 = 10.0;

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

    /// Checks whether the tower is talking, and what it's playing, for its
    /// stop button and control center.
    pub(super) fn refresh_voice(&mut self) {
        self.voice_polled = std::time::Instant::now();
        if !self.tower_enabled && !self.voice_panel_open() {
            return;
        }
        let status = voice::status();
        self.voice_now = status.playing;
        self.voice_speaking = self.voice_now.is_some() || voice::mac_speaking();
        // It heard you say "hush": say so, once.
        if let Some(heard) = status.heard
            && heard.time > self.voice_heard
        {
            let fresh = self.voice_heard > 0.0 || crate::mission::now() as f64 - heard.time < HEARD_SHOWN;
            self.voice_heard = heard.time;
            if fresh {
                self.set_status(format!("stopped: heard “{}”", agents::clip(&heard.text, 60)));
            }
        }
        // A message just finished joins the library.
        if let Mode::Panel(Panel::Voice(panel)) = &mut self.mode
            && self.voice_now.is_none()
            && panel.clips.len() != voice::library().len()
        {
            panel.clips = voice::library();
        }
    }

    /// What the voice is playing, as last checked.
    pub fn voice_now(&self) -> Option<&voice::Playing> {
        self.voice_now.as_ref()
    }

    pub fn voice_panel_open(&self) -> bool {
        matches!(self.mode, Mode::Panel(Panel::Voice(_)))
    }

    /// The voice's control center.
    pub(super) fn open_voice(&mut self) {
        self.enter(Mode::Panel(Panel::Voice(VoicePanel { selected: 0, clips: voice::library() })));
        self.refresh_voice();
    }

    /// Plays the highlighted message again.
    fn replay_selected(&mut self) {
        let Mode::Panel(Panel::Voice(panel)) = &self.mode else { return };
        let Some(clip) = panel.clips.get(panel.selected).cloned() else {
            return self.set_status("nothing in the library yet");
        };
        // Starting the speaker can take a moment: not on this thread.
        std::thread::spawn(move || voice::replay(&clip));
        self.set_status("playing it again");
    }

    pub(super) fn voice_key(&mut self, key: KeyEvent) {
        let Mode::Panel(Panel::Voice(panel)) = &mut self.mode else { return };
        let count = panel.clips.len();
        let last = count.saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.leave(),
            KeyCode::Up | KeyCode::Char('k') => panel.selected = panel.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => panel.selected = (panel.selected + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => panel.selected = 0,
            KeyCode::End | KeyCode::Char('G') => panel.selected = last,
            KeyCode::Enter | KeyCode::Char('p') => self.replay_selected(),
            KeyCode::Char(' ' | 's') => self.hush(),
            KeyCode::Char('r') => {
                panel.clips = voice::library();
                panel.selected = panel.selected.min(panel.clips.len().saturating_sub(1));
            }
            _ => {}
        }
    }

    /// A click in the control center: ▶, ■, or a message (a second click on
    /// it plays it).
    pub(super) fn voice_click(&mut self, pos: Position) {
        let body = panel_view::body(self.layout.pane);
        let (play, stop) = panel_view::voice_buttons(body);
        if play.contains(pos) {
            return self.replay_selected();
        }
        if stop.contains(pos) {
            return self.hush();
        }
        let list = panel_view::voice_list(body);
        let Mode::Panel(Panel::Voice(panel)) = &mut self.mode else { return };
        if !list.contains(pos) {
            return;
        }
        let index = panel_view::first_shown(panel.selected, list.height) + usize::from(pos.y - list.y);
        if index >= panel.clips.len() {
            return;
        }
        if index == panel.selected {
            self.replay_selected();
        } else {
            panel.selected = index;
        }
    }

    /// Listening for "hush" while it talks, or not.
    pub(super) fn toggle_listening(&mut self) {
        let on = !voice::listening_on();
        voice::set_listening(on);
        let state = match voice::hearing_missing() {
            _ if !on => "off".to_owned(),
            Some(missing) => format!("on, once it can: {missing}"),
            None => "on: say “hush” while it talks".to_owned(),
        };
        self.set_status(format!("listening for “hush” {state}"));
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
