//! The tower's tab: pi, on a Claude or Codex subscription, watching and
//! steering the other terminals through `sasha mcp`.

use std::ffi::OsString;

use anyhow::{Context, Result};

use super::App;
use crate::tab::Tab;
use crate::tower::{self, Engine};

impl App {
    /// Workspace and index of the tower's tab, if it's open.
    pub fn tower_tab(&self) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(w, ws)| ws.tabs.iter().position(|t| t.tower).map(|t| (w, t)))
    }

    /// Whether the tower is on screen.
    pub fn tower_on_screen(&self) -> bool {
        self.active_tab().is_some_and(|t| t.tower)
    }

    /// Goes to the tower, starting it the first time; from the tower, back
    /// to the terminal you came from.
    pub(super) fn toggle_tower(&mut self) {
        if self.tower_on_screen() {
            // Where you came from, else the workspace's last tab, else any.
            let back = [self.before_tower, self.ws().previous]
                .into_iter()
                .flatten()
                .find_map(|id| self.find_tab(id))
                .or_else(|| self.ws().tabs.iter().position(|t| !t.tower).map(|t| (self.current, t)));
            if let Some((w, t)) = back {
                self.go_to(w, t);
            }
            return;
        }
        self.before_tower = self.active_tab().map(|t| t.id);
        match self.tower_tab() {
            Some((w, t)) => self.go_to(w, t),
            None => self.start_tower(None),
        }
    }

    /// Opens the tower's tab, pinned first in the workspace on screen.
    fn start_tower(&mut self, engine: Option<Engine>) {
        let tab = match self.spawn_tower(engine) {
            Ok(tab) => tab,
            Err(err) => return self.set_status(format!("can't start the tower: {err:#}")),
        };
        let ws = self.ws_mut();
        ws.tabs.insert(0, tab);
        // The tab on screen moved down one.
        ws.active += 1;
        self.activate(0);
        self.reveal_active();
        self.save();
    }

    /// The tower's terminal: `sasha tower` in the tower's folder.
    pub(super) fn spawn_tower(&mut self, engine: Option<Engine>) -> Result<Tab> {
        let exe = std::env::current_exe().context("can't find the sasha executable")?;
        let dir = tower::dir().context("no home folder")?;
        std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
        let mut program: Vec<OsString> = vec![exe.into(), "tower".into()];
        program.extend(engine.map(|e| e.name().into()));
        let mut tab = self.spawn_program(&dir, Some(program))?;
        tab.tower = true;
        tab.pinned = true;
        tab.custom_prefix = Some("tower".to_owned());
        Ok(tab)
    }

    /// Runs the tower on Claude or Codex: pi switches and keeps the
    /// conversation; a tower not open yet starts on it.
    pub(super) fn tower_on(&mut self, engine: Engine) {
        let Some((w, t)) = self.tower_tab() else {
            self.before_tower = self.active_tab().map(|t| t.id);
            return self.start_tower(Some(engine));
        };
        self.workspaces[w].tabs[t].write(format!("/{}\r", engine.name()));
        self.go_to(w, t);
    }

    /// The subscription the tower runs on, as it last said.
    pub fn tower_engine(&self) -> Engine {
        tower::dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("engine")).ok())
            .and_then(|name| Engine::parse(&name))
            .unwrap_or(Engine::Claude)
    }
}
