//! The tower's sessions: what it set each terminal to do, how that's going,
//! and the news it hasn't heard yet. With auto-reply on, the news goes
//! straight to the tower whenever it's idle. Sessions also message each
//! other by name: a message waits for its reader's turn to end.

use std::time::{Duration, Instant};

use super::App;
use crate::mission::{self, Ended, Mission, Signal, State, Update};
use crate::tab::{Tab, TabId};

/// The tower counts as idle once it has printed nothing for this long.
const TOWER_IDLE: Duration = Duration::from_secs(3);
/// Auto-reply leaves the tower alone this long after you typed there.
const YOUR_TURN: Duration = Duration::from_secs(30);
/// Between typing the news into the tower and the Enter that sends it.
const BEFORE_ENTER: Duration = Duration::from_millis(300);
/// A session stays watched by a handler this long after its last look.
const WATCHED: Duration = Duration::from_secs(10);
/// How a message from another session starts, as its reader sees it.
const MAIL_MARK: &str = "[message from";

impl App {
    /// The tower handed terminal `id` a task: it's one of its sessions now.
    pub(super) fn start_mission(&mut self, id: TabId, task: &str) {
        let Some(tab) = self.tab_mut_by_id(id) else { return };
        match &mut tab.mission {
            Some(mission) => {
                mission.task = mission::brief(task);
                mission.set(State::Working, None);
            }
            None => tab.mission = Some(Mission::new(task)),
        }
    }

    /// What an agent's hooks say about one of the tower's sessions.
    pub(super) fn signal(&mut self, id: TabId, signal: Signal) {
        let Some(tab) = self.tab_mut_by_id(id) else { return };
        let label = format!("#{id} {}", tab.prefix());
        let Some(mission) = &mut tab.mission else { return };
        mission.hooked = true;
        let update = match signal {
            Signal::Started { prompt } => {
                // A message from another session isn't a new task.
                if !prompt.starts_with(MAIL_MARK) {
                    mission.task = mission::brief(&prompt);
                }
                mission.set(State::Working, None);
                None
            }
            Signal::Finished { summary, log } => {
                mission.log = log.or(mission.log.take());
                mission.set(State::Done, Some(summary.clone()));
                Some(format!("{label} finished: {summary}"))
            }
            Signal::Waiting { what, log } => {
                mission.log = log.or(mission.log.take());
                mission.set(State::Waiting, Some(what.clone()));
                Some(format!("{label} is waiting for approval: {what}"))
            }
        };
        if let Some(text) = update {
            self.tell_tower(id, text);
        }
        // Its turn is over: what other sessions wrote can go in.
        self.deliver_mail(id);
    }

    /// A message from the session in `from` to the one called `to`, or to
    /// the tower. It goes in once the reader's turn is over.
    pub(super) fn message(&mut self, from: Option<TabId>, to: &str, text: &str) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("the message is empty".to_owned());
        }
        let sender = from.and_then(|id| self.any_tab(id)).map_or_else(|| "a session".to_owned(), |t| format!("#{} {}", t.id, t.prefix()));
        if to.trim().eq_ignore_ascii_case("tower") {
            self.tell_tower(from.unwrap_or_default(), format!("{sender} says: {text}"));
            return Ok("The tower reads it before its next turn.".to_owned());
        }
        let id = self.session_named(to)?;
        if Some(id) == from {
            return Err("that's your own session".to_owned());
        }
        let Some(tab) = self.tab_mut_by_id(id) else { return Err(format!("no session #{id}")) };
        let reader = format!("#{id} {}", tab.prefix());
        // Typing in is safe only when its hooks say its turn is over.
        if !tab.mission.as_ref().is_some_and(|m| m.hooked) {
            return Err(format!(
                "{reader} isn't a session the tower started with an agent: sasha can't tell when it's safe to type into it"
            ));
        }
        tab.mailbox.push(format!("{MAIL_MARK} {sender}] {text}"));
        self.tell_tower(id, format!("{sender} → {reader}: {text}"));
        Ok(if self.deliver_mail(id) {
            format!("Delivered to {reader}.")
        } else {
            format!("{reader} is busy: it gets your message when its turn ends.")
        })
    }

    /// The terminal a session's name, `#id` or id stands for.
    fn session_named(&self, to: &str) -> Result<TabId, String> {
        let to = to.trim();
        if let Ok(id) = to.trim_start_matches('#').parse::<TabId>()
            && self.find_tab(id).is_some()
        {
            return Ok(id);
        }
        let named: Vec<TabId> = self
            .workspaces
            .iter()
            .flat_map(|ws| &ws.tabs)
            .filter(|t| t.prefix().eq_ignore_ascii_case(to))
            .map(|t| t.id)
            .collect();
        match named[..] {
            [id] => Ok(id),
            [] => Err(format!("no session called {to}: list_sessions shows their names")),
            _ => Err(format!("{} sessions are called {to}: give its #id", named.len())),
        }
    }

    /// Types a session's waiting messages in, if its turn is over and you
    /// aren't typing there; Enter follows a moment later.
    fn deliver_mail(&mut self, id: TabId) -> bool {
        let typing = self.pane_last_key.is_some_and(|(tab, at)| tab == id && at.elapsed() < YOUR_TURN);
        let Some(tab) = self.tab_mut_by_id(id) else { return false };
        let free = tab.mission.as_ref().is_some_and(|m| m.hooked && m.state == State::Done);
        if tab.mailbox.is_empty() || !free || typing {
            return false;
        }
        let mail = std::mem::take(&mut tab.mailbox).join("\n\n");
        tab.paste(&format!("{mail}\n\n(Answer with message_session if it needs a reply.)"));
        if let Some(mission) = &mut tab.mission {
            mission.set(State::Working, None);
        }
        self.enters.push((id, Instant::now() + BEFORE_ENTER));
        true
    }

    /// News a session without hooks gave (it went quiet, its program
    /// ended, it rang): it's probably done.
    pub(super) fn mission_news(&mut self, id: TabId, news: &str) {
        let Some(tab) = self.tab_mut_by_id(id) else { return };
        let text = format!("#{id} {}: {news} (agent_report or read_terminal says how it went)", tab.prefix());
        let Some(mission) = &mut tab.mission else { return };
        if mission.hooked || mission.state != State::Working {
            return;
        }
        mission.set(State::Done, Some(news.to_owned()));
        self.tell_tower(id, text);
    }

    /// One of the tower's sessions closed.
    pub(super) fn end_mission(&mut self, tab: &Tab, workspace: &str) {
        let Some(mission) = &tab.mission else { return };
        let mut mission = mission.clone();
        let was = mission.state.word();
        mission.set(State::Ended, Some(format!("its terminal closed while it was {was}")));
        self.tell_tower(tab.id, format!("#{} {} closed", tab.id, tab.prefix()));
        self.ended_missions.push(Ended { id: tab.id, name: tab.prefix(), workspace: workspace.to_owned(), mission });
        let over = self.ended_missions.len().saturating_sub(mission::ENDED_MAX);
        self.ended_missions.drain(..over);
    }

    fn tell_tower(&mut self, id: TabId, text: String) {
        self.tower_inbox.push(Update { id, text });
        let over = self.tower_inbox.len().saturating_sub(mission::INBOX_MAX);
        self.tower_inbox.drain(..over);
    }

    /// The news about `ids` (else all), taken off the pile with `take`.
    /// A wait on particular sessions (a session handler's) marks them
    /// watched: their news stays for it rather than going to the tower.
    pub(super) fn updates(&mut self, ids: Option<&[TabId]>, take: bool) -> Vec<Update> {
        let now = Instant::now();
        if let Some(ids) = ids {
            for id in ids {
                self.watched.insert(*id, now);
            }
        }
        self.watched.retain(|_, at| at.elapsed() < WATCHED);
        let watched = &self.watched;
        let wanted = |u: &Update| match ids {
            Some(ids) => ids.contains(&u.id),
            None => !watched.contains_key(&u.id),
        };
        let news: Vec<Update> = self.tower_inbox.iter().filter(|u| wanted(u)).cloned().collect();
        if take {
            self.tower_inbox.retain(|u| !wanted(u));
        }
        news
    }

    /// How much news the tower hasn't heard, but what handlers wait for.
    pub fn tower_news(&self) -> usize {
        let watched = |id: &TabId| self.watched.get(id).is_some_and(|at| at.elapsed() < WATCHED);
        self.tower_inbox.iter().filter(|u| !watched(&u.id)).count()
    }

    pub fn tower_autopilot(&self) -> bool {
        self.tower_autopilot
    }

    pub(super) fn toggle_autopilot(&mut self) {
        self.tower_autopilot = !self.tower_autopilot;
        let state = if self.tower_autopilot { "on: news goes to the tower when it's idle" } else { "off" };
        self.set_status(format!("auto-reply {state}"));
        self.save();
    }

    /// Auto-reply: hands the tower its news once it's idle and you aren't
    /// typing there, then presses Enter a moment later.
    pub(super) fn tower_tick(&mut self) {
        // Enter for messages typed into sessions.
        let now = Instant::now();
        let due: Vec<TabId> = self.enters.iter().filter(|(_, at)| *at <= now).map(|(id, _)| *id).collect();
        self.enters.retain(|(_, at)| *at > now);
        for id in due {
            if let Some(tab) = self.tab_mut_by_id(id) {
                tab.write("\r");
            }
        }
        // Mail held back while you typed there.
        let waiting: Vec<TabId> =
            self.workspaces.iter().flat_map(|ws| &ws.tabs).filter(|t| !t.mailbox.is_empty()).map(|t| t.id).collect();
        for id in waiting {
            self.deliver_mail(id);
        }
        if let Some(at) = self.tower_enter_at
            && Instant::now() >= at
        {
            self.tower_enter_at = None;
            if let Some(tower) = &self.tower {
                tower.write("\r");
            }
            return;
        }
        if !self.tower_autopilot || self.tower_news() == 0 || self.tower_enter_at.is_some() {
            return;
        }
        let Some(tower) = &self.tower else { return };
        let idle = tower.last_activity.elapsed() >= TOWER_IDLE && !tower.settling();
        let typing = self.tower_last_key.is_some_and(|at| at.elapsed() < YOUR_TURN);
        if !idle || typing {
            return;
        }
        // News a session handler is waiting for is its, not the tower's.
        let news: Vec<String> = self.updates(None, true).into_iter().map(|u| format!("- {}", u.text)).collect();
        if news.is_empty() {
            return;
        }
        let message = format!(
            "[sasha] News from your sessions:\n{}\nLook into what needs it, carry on with what you were asked to see through, and tell me briefly.",
            news.join("\n")
        );
        if let Some(tower) = &self.tower {
            tower.paste(&message);
        }
        self.tower_enter_at = Some(Instant::now() + BEFORE_ENTER);
    }

    fn tab_mut_by_id(&mut self, id: TabId) -> Option<&mut Tab> {
        let (w, t) = self.find_tab(id)?;
        Some(&mut self.workspaces[w].tabs[t])
    }
}
