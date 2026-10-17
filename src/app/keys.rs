//! Keyboard handling: each mode reads keys its own way, and every change of
//! mode goes through `App::enter`.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::{
    After, Command, Confirm, Mode, Picker, PickerKind, Prefix, Rename, Renaming, TabMode, edit_line,
};
use super::{App, GroupEntry, PickerEntry};
use crate::input;
use crate::workspace::{self, GroupId};

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) {
        if self.config.prefix.matches(&key) {
            self.pending.keep();
        }
        match &self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Tab(_) => self.tab_mode_key(key),
            Mode::Scroll => self.scroll_mode_key(key),
            Mode::Rename(_) => self.rename_key(key),
            Mode::Picker(picker) => match picker.kind {
                PickerKind::Search => self.search_key(key),
                PickerKind::Workspaces => self.workspaces_key(key),
                PickerKind::Groups => self.groups_key(key),
                PickerKind::Menu(menu) => self.menu_key(key, menu),
            },
            Mode::Panel(_) => self.panel_key(key),
            Mode::Files(_) => self.files_key(key),
            Mode::Help => self.leave(),
            Mode::ConfirmQuit => {
                self.leave();
                match key.code {
                    // q again confirms: ^B q q.
                    KeyCode::Char('y' | 'Y' | 'q' | 'Q') | KeyCode::Enter => self.quit = true,
                    KeyCode::Char('d' | 'D') => self.detach(),
                    _ => {}
                }
            }
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        if self.config.prefix.matches(&key) {
            self.enter(Mode::Tab(TabMode::new(Instant::now())));
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = self.layout.pane.height as isize;
        let full_screen = self.active_tab().is_some_and(|t| t.screen().alternate_screen());
        // Browser-style shortcuts that don't need the prefix.
        match key.code {
            KeyCode::PageUp if shift && !ctrl && !full_screen => self.scroll_active(page),
            KeyCode::PageDown if shift && !ctrl && !full_screen => self.scroll_active(-page),
            KeyCode::PageDown if ctrl && shift => self.shift_active(1),
            KeyCode::PageUp if ctrl && shift => self.shift_active(-1),
            KeyCode::PageDown if ctrl => self.cycle(1),
            KeyCode::PageUp if ctrl => self.cycle(-1),
            KeyCode::Char(c @ '1'..='9') if alt && !ctrl => self.jump(c),
            _ => self.send_key(key),
        }
    }

    fn tab_mode_key(&mut self, key: KeyEvent) {
        let now = Instant::now();
        if self.config.prefix.matches(&key) {
            let Mode::Tab(tab) = &mut self.mode else { return };
            if tab.prefix(now) == Prefix::PassThrough {
                let byte = self.config.prefix.byte();
                if let Some(tab) = self.focused_tab() {
                    tab.write([byte]);
                }
                self.leave();
            }
            return;
        }
        let command = Command::from_key(&key);
        self.run_command(command);
        match command.after() {
            After::Leave => self.leave(),
            // Moving keys (j j k…) may follow; tab mode waits a moment.
            After::Stay => {
                if let Mode::Tab(tab) = &mut self.mode {
                    tab.repeat(now);
                }
            }
            After::Own => {}
        }
    }

    /// Does what a tab-mode key (or a menu's row) asks.
    pub(super) fn run_command(&mut self, command: Command) {
        match command {
            Command::NextTab => self.cycle(1),
            Command::PrevTab => self.cycle(-1),
            Command::MoveDown => self.shift_active(1),
            Command::MoveUp => self.shift_active(-1),
            Command::NextWorkspace => self.cycle_workspace(1),
            Command::PrevWorkspace => self.cycle_workspace(-1),
            Command::FirstTab => self.jump('1'),
            Command::LastTab => self.jump('9'),
            Command::Jump(c) => self.jump(c),
            Command::PreviousTab => self.activate_previous(),
            Command::PreviousWorkspace => self.switch_to_previous_workspace(),
            Command::NewTab => {
                if let Err(err) = self.new_tab_in_group() {
                    self.set_status(format!("can't start a shell: {err}"));
                }
            }
            Command::Close => self.close_tab_guarded(self.ws().active),
            Command::Pin => self.toggle_pin(self.ws().active),
            Command::GroupPicker => self.enter(Mode::Picker(Picker::new(PickerKind::Groups, String::new()))),
            Command::Ungroup => self.remove_from_group(),
            Command::ToggleGroup => {
                if let Some(id) = self.active_group() {
                    self.toggle_group(id);
                }
            }
            Command::RenameGroup => {
                if let Some(id) = self.active_group() {
                    self.start_group_rename(id);
                }
            }
            Command::Workspaces => self.start_workspaces(String::new()),
            Command::RenameWorkspace => self.start_workspace_rename(),
            Command::ToggleSidebar => self.set_collapsed(!self.collapsed),
            Command::CycleWorkspaceBar => self.cycle_activity_bar(),
            Command::ToggleStacked => self.toggle_stacked(),
            Command::Minimize => self.toggle_section(self.current),
            Command::KeepAwake => self.toggle_awake(),
            Command::PullRequests => self.open_prs(),
            Command::Ports => self.open_ports(0),
            Command::Files => self.browse_files(),
            Command::Tower => self.toggle_tower(),
            Command::TowerOn(engine) => self.tower_on(engine),
            Command::TowerRestart => self.restart_tower(),
            Command::TowerHide => self.hide_tower(),
            Command::SideBars => self.toggle_side_bars(),
            Command::Narrower => self.resize_sidebar(-2),
            Command::Wider => self.resize_sidebar(2),
            Command::Scroll => self.enter(Mode::Scroll),
            Command::Rename => self.start_rename(),
            Command::Search => self.enter(Mode::Picker(Picker::new(PickerKind::Search, String::new()))),
            Command::Help => self.enter(Mode::Help),
            Command::Detach => self.detach(),
            Command::Quit => self.request_quit(),
            Command::Leave => {}
        }
    }

    fn scroll_mode_key(&mut self, key: KeyEvent) {
        if self.config.prefix.matches(&key) {
            self.enter(Mode::Tab(TabMode::new(Instant::now())));
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.layout.pane.height as isize;
        let delta = match key.code {
            KeyCode::Char('k') | KeyCode::Up => 1,
            KeyCode::Char('j') | KeyCode::Down => -1,
            KeyCode::Char('y') if ctrl => 1,
            KeyCode::Char('e') if ctrl => -1,
            KeyCode::Char('u') if ctrl => page / 2,
            KeyCode::Char('d') if ctrl => -page / 2,
            KeyCode::Char('b') if ctrl => page,
            KeyCode::Char('f') if ctrl => -page,
            KeyCode::PageUp => page,
            KeyCode::PageDown | KeyCode::Char(' ') => -page,
            KeyCode::Char('g') | KeyCode::Home => isize::MAX,
            KeyCode::Char('G') | KeyCode::End => isize::MIN,
            KeyCode::Char('q' | 'i') | KeyCode::Esc | KeyCode::Enter => return self.leave(),
            _ => return,
        };
        self.scroll_active(delta);
    }

    /// Moves the active tab's view `delta` lines into the scrollback
    /// (negative goes back towards the live screen).
    pub(super) fn scroll_active(&mut self, delta: isize) {
        self.selection = None;
        if let Some(tab) = self.ws_mut().active_tab_mut() {
            let offset = tab.screen().scrollback().saturating_add_signed(delta);
            tab.screen_mut().set_scrollback(offset);
        }
    }

    // ── Renaming ────────────────────────────────────────────────────────

    pub(super) fn start_rename(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        let text = tab.prefix();
        self.enter(Mode::Rename(Rename { target: Renaming::Tab, text }));
    }

    pub(super) fn start_group_rename(&mut self, id: GroupId) {
        let Some(group) = self.group(id) else { return };
        let text = group.name.clone();
        self.enter(Mode::Rename(Rename { target: Renaming::Group(id), text }));
    }

    pub(super) fn start_workspace_rename(&mut self) {
        let text = self.ws().name.clone();
        self.enter(Mode::Rename(Rename { target: Renaming::Workspace, text }));
    }

    /// Saves the name being typed and leaves rename mode.
    pub(super) fn finish_rename(&mut self) {
        let Mode::Rename(rename) = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        self.leave();
        let name = rename.text.trim().to_owned();
        match rename.target {
            Renaming::Group(id) => {
                if let Some(group) = self.group_mut(id) {
                    group.name = name;
                }
            }
            Renaming::Workspace => {
                self.ws_mut().rename(Some(name));
                self.save();
            }
            Renaming::Tab => {
                let mut pinned = false;
                if let Some(tab) = self.ws_mut().active_tab_mut() {
                    // An empty name goes back to the automatic title.
                    tab.custom_prefix = (!name.is_empty()).then_some(name);
                    pinned = tab.pinned;
                }
                if pinned {
                    self.save();
                }
            }
        }
    }

    fn rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.finish_rename(),
            KeyCode::Esc => self.leave(),
            KeyCode::Tab => {
                if let Mode::Rename(Rename { target: Renaming::Group(id), .. }) = self.mode {
                    self.cycle_group_color(id);
                }
            }
            _ => {
                if let Mode::Rename(rename) = &mut self.mode {
                    edit_line(&mut rename.text, &key);
                }
            }
        }
    }

    // ── Pickers ─────────────────────────────────────────────────────────

    /// The picker being shown, to change.
    fn picker_mut(&mut self) -> Option<&mut Picker> {
        match &mut self.mode {
            Mode::Picker(picker) => Some(picker),
            _ => None,
        }
    }

    /// Moves the highlight or types into the query: what every picker does
    /// with keys it has no use of its own for.
    pub(super) fn picker_key(&mut self, key: &KeyEvent, rows: usize) {
        if let Some(picker) = self.picker_mut()
            && !picker.step(key, rows)
        {
            picker.edit(key);
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        let results = self.search_results();
        let selected = self.picker().map_or(0, |p| p.selected);
        match key.code {
            KeyCode::Esc => self.leave(),
            KeyCode::Enter => {
                self.leave();
                if let Some(&(w, t)) = results.get(selected) {
                    self.go_to(w, t);
                }
            }
            _ => self.picker_key(&key, results.len()),
        }
    }

    pub(super) fn start_workspaces(&mut self, query: String) {
        self.enter(Mode::Picker(Picker::new(PickerKind::Workspaces, query)));
    }

    fn workspaces_key(&mut self, key: KeyEvent) {
        let entries = self.picker_entries();
        let selected = self.picker().and_then(|p| entries.get(p.selected)).cloned();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.leave(),
            KeyCode::Enter => {
                self.leave();
                match selected {
                    Some(PickerEntry::Workspace(index)) => self.switch_workspace(index),
                    Some(PickerEntry::AddFolder(path)) => {
                        let index = self.add_workspace(path);
                        self.save();
                        self.switch_workspace(index);
                    }
                    None => {}
                }
            }
            // Complete the highlighted folder into the query.
            KeyCode::Tab => {
                let path = match selected {
                    Some(PickerEntry::AddFolder(path)) => Some(path),
                    Some(PickerEntry::Workspace(index)) => Some(self.workspaces[index].path.clone()),
                    None => None,
                };
                if let (Some(path), Some(picker)) = (path, self.picker_mut()) {
                    picker.query = format!("{}/", workspace::pretty_path(&path).trim_end_matches('/'));
                    picker.selected = 0;
                }
            }
            KeyCode::Char('d') if ctrl => {
                let Some(PickerEntry::Workspace(index)) = selected else { return };
                let ws = &self.workspaces[index];
                // Removing closes the workspace's shells, so ask twice.
                if !ws.tabs.is_empty() && !self.pending.confirm(Confirm::RemoveWorkspace(ws.id), Instant::now()) {
                    return;
                }
                self.remove_workspace(index);
                let rows = self.picker_entries().len();
                if let Some(picker) = self.picker_mut() {
                    picker.selected = picker.selected.min(rows.saturating_sub(1));
                }
            }
            _ => self.picker_key(&key, entries.len()),
        }
    }

    fn groups_key(&mut self, key: KeyEvent) {
        let entries = self.group_entries();
        let selected = self.picker().and_then(|p| entries.get(p.selected)).cloned();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.leave(),
            KeyCode::Enter => {
                self.leave();
                match selected {
                    Some(GroupEntry::New(name)) => self.new_group(name),
                    Some(GroupEntry::Group(id)) => self.add_to_group(id),
                    None => {}
                }
            }
            KeyCode::Char('d') if ctrl => {
                if let Some(GroupEntry::Group(id)) = selected {
                    self.delete_group(id);
                    let rows = self.group_entries().len();
                    if let Some(picker) = self.picker_mut() {
                        picker.selected = picker.selected.min(rows.saturating_sub(1));
                    }
                }
            }
            _ => self.picker_key(&key, entries.len()),
        }
    }

    fn send_key(&mut self, key: KeyEvent) {
        self.selection = None;
        // A stopped tower starts again with Enter.
        if self.tower_focused() && self.tower.is_none() {
            if key.code == KeyCode::Enter {
                self.start_tower(None);
            }
            return;
        }
        let Some(tab) = self.focused_tab_mut() else { return };
        // Typing always jumps back to the live screen.
        tab.screen_mut().set_scrollback(0);
        let bytes = input::encode_key(&key, tab.screen().application_cursor());
        if !bytes.is_empty() {
            tab.write(bytes);
        }
    }
}
