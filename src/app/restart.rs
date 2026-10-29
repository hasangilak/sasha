//! Restarting the Claude Code or Codex in a terminal, say once it's been
//! updated: it's stopped once its turn is over, and started again in the
//! same shell on the same conversation, with the flags it had.

use super::App;

impl App {
    /// Restarts the agent in the terminal on screen: now if it's idle,
    /// else once its turn is over; asked again while it waits, now.
    pub(super) fn restart_agent(&mut self) {
        let key = format!("{} R", self.config.prefix.label());
        let Some(tab) = self.ws_mut().active_tab_mut() else { return };
        let Some(agent) = tab.agent() else {
            self.set_status("no Claude or Codex running in this terminal");
            return;
        };
        let name = agent.name();
        let wait = !tab.restart_waiting() && tab.agent_busy();
        if !tab.restart_agent(wait) {
            self.set_status(format!("can't tell how to start {name} again"));
        } else if wait {
            self.set_status(format!("{name} restarts once this turn is over · {key} restarts it now"));
        } else {
            self.set_status(format!("restarting {name}…"));
        }
    }

    /// Restarts the agent in every terminal, each once its turn is over.
    pub(super) fn restart_agents(&mut self) {
        let (mut now, mut later) = (0, 0);
        for tab in self.workspaces.iter_mut().flat_map(|w| &mut w.tabs) {
            if tab.agent().is_none() {
                continue;
            }
            let wait = tab.agent_busy();
            if tab.restart_agent(wait) {
                if wait { later += 1 } else { now += 1 }
            }
        }
        let status = match (now, later) {
            (0, 0) => "no Claude or Codex running in any terminal".to_owned(),
            (now, 0) => format!("restarting {now} · same conversations"),
            (0, later) => format!("{later} restart once their turns are over"),
            (now, later) => format!("restarting {now}; {later} more once their turns are over"),
        };
        self.set_status(status);
    }

    pub(super) fn cancel_restart(&mut self) {
        let Some(tab) = self.ws_mut().active_tab_mut() else { return };
        tab.cancel_restart();
        let name = tab.agent().map_or("it", |agent| agent.name());
        self.set_status(format!("{name} won't restart"));
    }

    /// Says what came of the restarts that finished.
    pub(super) fn report_restarts(&mut self) {
        let mut news = Vec::new();
        for tab in self.workspaces.iter_mut().flat_map(|w| &mut w.tabs) {
            if let Some(restarted) = tab.take_restarted() {
                news.push(format!("{}: {restarted}", tab.prefix()));
            }
        }
        match news.len() {
            0 => {}
            1 => self.set_status(news.remove(0)),
            n => self.set_status(format!("restarted {n} · same conversations")),
        }
    }

    /// How many terminals run Claude or Codex, and how many of them are on
    /// an older version than the one installed.
    pub fn agents_running(&self) -> (usize, usize) {
        let tabs = self.workspaces.iter().flat_map(|w| &w.tabs).filter(|tab| tab.agent().is_some());
        tabs.fold((0, 0), |(all, old), tab| (all + 1, old + usize::from(tab.newer_agent().is_some())))
    }
}
