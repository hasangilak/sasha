//! The tower: pi, on a Claude or Codex subscription, watching and steering
//! every terminal through `sasha mcp`. It has a panel of its own at the
//! right, outside the workspaces, and once opened it keeps running, hidden
//! or not, across restarts.

use std::ffi::OsString;
use std::time::Instant;

use anyhow::{Context, Result};

use super::App;
use crate::layout;
use crate::tab::{Tab, TabId};
use crate::tower::{self, Engine};

impl App {
    /// Whether the tower's panel shows.
    pub fn tower_shown(&self) -> bool {
        !self.layout.tower.is_empty()
    }

    /// Whether keys go to the tower rather than the terminal in the pane.
    pub fn tower_focused(&self) -> bool {
        self.tower_focus && self.tower_shown()
    }

    pub fn tower(&self) -> Option<&Tab> {
        self.tower.as_ref()
    }

    /// The tower printed something while its panel was hidden.
    pub fn tower_has_news(&self) -> bool {
        !self.tower_shown() && self.tower.as_ref().is_some_and(|t| t.unseen)
    }

    pub(super) fn is_tower(&self, id: TabId) -> bool {
        self.tower.as_ref().is_some_and(|t| t.id == id)
    }

    /// A tab by id, the tower's included.
    pub(super) fn any_tab(&self, id: TabId) -> Option<&Tab> {
        match self.find_tab(id) {
            Some((w, t)) => Some(&self.workspaces[w].tabs[t]),
            None => self.tower.as_ref().filter(|t| t.id == id),
        }
    }

    /// The terminal keys go to: the tower's, or the one in the pane.
    pub(super) fn focused_tab_mut(&mut self) -> Option<&mut Tab> {
        if self.tower_focused() { self.tower.as_mut() } else { self.ws_mut().active_tab_mut() }
    }

    /// You typed: into the tower, or the terminal in the pane. Messages and
    /// news wait while you do.
    pub(super) fn note_typing(&mut self) {
        let now = Instant::now();
        if self.tower_focused() {
            self.tower_last_key = Some(now);
        } else if let Some(tab) = self.active_tab() {
            self.pane_last_key = Some((tab.id, now));
        }
    }

    pub(super) fn focused_tab(&self) -> Option<&Tab> {
        if self.tower_focused() { self.tower.as_ref() } else { self.active_tab() }
    }

    /// The prefix, then T: shows the tower and gives it the keys; from
    /// there, hides it.
    pub(super) fn toggle_tower(&mut self) {
        if self.tower_focused() { self.hide_tower() } else { self.show_tower() }
    }

    /// The header's ♜: shows or hides the panel.
    pub(super) fn toggle_tower_panel(&mut self) {
        if self.tower_shown() { self.hide_tower() } else { self.show_tower() }
    }

    /// Shows the tower's panel with the keys on it, starting the tower if
    /// it isn't running.
    pub(super) fn show_tower(&mut self) {
        self.tower_enabled = true;
        self.tower_panel = true;
        self.relayout_now();
        if !self.tower_shown() {
            self.tower_panel = false;
            self.relayout_now();
            return self.set_status("no room for the tower: hide a bar or widen the window");
        }
        self.tower_focus = true;
        if let Some(tower) = &mut self.tower {
            tower.unseen = false;
            tower.bell = false;
        } else {
            self.start_tower(None);
        }
        self.save();
    }

    /// Hides the panel; the tower carries on.
    pub(super) fn hide_tower(&mut self) {
        self.tower_panel = false;
        self.tower_focus = false;
        self.relayout_now();
        self.save();
    }

    /// Starts pi in the tower, on `engine` or else the one it ran on last.
    pub(super) fn start_tower(&mut self, engine: Option<Engine>) {
        match self.spawn_tower(engine) {
            Ok(tab) => self.tower = Some(tab),
            Err(err) => self.set_status(format!("can't start the tower: {err:#}")),
        }
    }

    /// Stops pi and starts it again; it picks its conversation up.
    pub(super) fn restart_tower(&mut self) {
        // Dropping the tab hangs pi up.
        self.tower = None;
        self.start_tower(None);
        self.show_tower();
    }

    /// The tower's terminal: `sasha tower` in the tower's folder.
    fn spawn_tower(&mut self, engine: Option<Engine>) -> Result<Tab> {
        let exe = std::env::current_exe().context("can't find the sasha executable")?;
        let dir = tower::dir().context("no home folder")?;
        std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
        let mut program: Vec<OsString> = vec![exe.into(), "tower".into()];
        program.extend(engine.map(|e| e.name().into()));
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        Tab::spawn(id, self.tower_size(), Some(&dir), Some(program), self.events_tx.clone())
    }

    /// Rows and columns of the tower's terminal: its panel's, or what they
    /// would be while it's hidden.
    pub(super) fn tower_size(&self) -> (u16, u16) {
        let screen = self.layout.tower_screen();
        if screen.is_empty() {
            let width = self.tower_width.clamp(layout::TOWER_MIN, layout::TOWER_MAX);
            (self.layout.pane.height.max(1), width - 1)
        } else {
            (screen.height.max(1), screen.width.max(1))
        }
    }

    /// Dragging the panel's left edge.
    pub(super) fn resize_tower(&mut self, column: u16) {
        let width = self.layout.tower.right().saturating_sub(column);
        self.tower_width = width.clamp(layout::TOWER_MIN, layout::TOWER_MAX);
        self.relayout_now();
    }

    /// Runs the tower on Claude (Claude Code) or Codex (pi): starts that
    /// one in its place, which picks up its own last conversation.
    pub(super) fn tower_on(&mut self, engine: Engine) {
        // Dropping the tab hangs the other one up.
        self.tower = None;
        self.start_tower(Some(engine));
        self.show_tower();
    }

    /// Your instructions for the tower, in your editor, in the pane.
    pub(super) fn edit_tower_role(&mut self) {
        match tower::role_file() {
            Ok(path) => {
                self.tower_focus = false;
                self.open_in_editor(path);
                // Not to be taken for the tower itself.
                if let Some(tab) = self.ws_mut().active_tab_mut() {
                    tab.custom_prefix = Some("tower role".to_owned());
                }
                self.set_status("your role for the tower: save, then restart the tower (its menu) to apply");
            }
            Err(err) => self.set_status(format!("can't open the tower's role: {err:#}")),
        }
    }

    /// The subscription the tower runs on, as it last said.
    pub fn tower_engine(&self) -> Engine {
        tower::current_engine()
    }
}
