//! Keyboard handling for every mode.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, GroupEntry, Mode, PickerEntry, Renaming};
use crate::workspace::GroupId;
use crate::input;
use crate::workspace;

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Tab => self.tab_mode_key(key),
            Mode::Scroll => self.scroll_mode_key(key),
            Mode::Rename => self.rename_key(key),
            Mode::Search => self.search_key(key),
            Mode::Workspaces => self.workspaces_key(key),
            Mode::Groups => self.groups_key(key),
            Mode::Help => self.mode = Mode::Normal,
            Mode::ConfirmQuit => {
                self.mode = Mode::Normal;
                match key.code {
                    KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.quit = true,
                    KeyCode::Char('d' | 'D') => self.detach(),
                    _ => {}
                }
            }
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        if self.config.prefix.matches(&key) {
            self.mode = Mode::Tab;
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
        if self.config.prefix.matches(&key) {
            // Prefix twice sends it through, like tmux.
            let byte = self.config.prefix.byte();
            if let Some(tab) = self.active_tab() {
                tab.write([byte]);
            }
            self.mode = Mode::Normal;
            return;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // A pending "press x again" only survives an immediate second x.
        if key.code != KeyCode::Char('x') {
            self.confirm_close = None;
        }
        match key.code {
            KeyCode::Char('J') => self.shift_active(1),
            KeyCode::Char('K') => self.shift_active(-1),
            KeyCode::Down if shift => self.shift_active(1),
            KeyCode::Up if shift => self.shift_active(-1),
            KeyCode::Char('j') | KeyCode::Down => self.cycle(1),
            KeyCode::Char('k') | KeyCode::Up => self.cycle(-1),
            KeyCode::Char('l') | KeyCode::Right => self.cycle_workspace(1),
            KeyCode::Char('h') | KeyCode::Left => self.cycle_workspace(-1),
            KeyCode::Char('g') | KeyCode::Home => self.jump('1'),
            KeyCode::Char('G') | KeyCode::End => self.jump('9'),
            KeyCode::Char(c @ '1'..='9') => {
                self.jump(c);
                self.mode = Mode::Normal;
            }
            KeyCode::Tab => {
                self.activate_previous();
                self.mode = Mode::Normal;
            }
            KeyCode::BackTab => {
                self.switch_to_previous_workspace();
                self.mode = Mode::Normal;
            }
            KeyCode::Char('t' | 'n' | 'c') => {
                if let Err(err) = self.new_tab_in_group() {
                    self.set_status(format!("can't start a shell: {err}"));
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Char('x') => self.close_tab_guarded(self.ws().active),
            KeyCode::Char('p') => self.toggle_pin(self.ws().active),
            KeyCode::Char('m') => self.start_groups(),
            KeyCode::Char('u') => self.remove_from_group(),
            KeyCode::Char('z') => {
                if let Some(id) = self.active_group() {
                    self.toggle_group(id);
                }
            }
            KeyCode::Char('e') => {
                if let Some(id) = self.active_group() {
                    self.start_group_rename(id);
                }
            }
            KeyCode::Char('w') => self.start_workspaces(String::new()),
            KeyCode::Char('W') => self.start_workspace_rename(),
            KeyCode::Char('b' | 's') => self.set_collapsed(!self.collapsed),
            KeyCode::Char('a') => self.cycle_activity_bar(),
            KeyCode::Char('<') => self.resize_sidebar(-2),
            KeyCode::Char('>') => self.resize_sidebar(2),
            KeyCode::Char('[' | 'v') | KeyCode::PageUp => self.mode = Mode::Scroll,
            KeyCode::Char('r' | ',') => self.start_rename(),
            KeyCode::Char('/' | 'f') => self.start_search(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('d') => self.detach(),
            KeyCode::Char('q') => self.request_quit(),
            _ => self.mode = Mode::Normal,
        }
    }

    fn scroll_mode_key(&mut self, key: KeyEvent) {
        if self.config.prefix.matches(&key) {
            self.scroll_active(isize::MIN);
            self.mode = Mode::Tab;
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
            KeyCode::Char('q' | 'i') | KeyCode::Esc | KeyCode::Enter => {
                self.scroll_active(isize::MIN);
                self.mode = Mode::Normal;
                return;
            }
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

    pub(super) fn start_rename(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        self.input = tab.prefix();
        self.renaming = Renaming::Tab;
        if self.collapsed {
            self.set_collapsed(false);
        }
        self.mode = Mode::Rename;
    }

    pub(super) fn start_group_rename(&mut self, id: GroupId) {
        let Some(group) = self.ws().group(id) else { return };
        self.input = group.name.clone();
        self.renaming = Renaming::Group(id);
        if self.collapsed {
            self.set_collapsed(false);
        }
        self.mode = Mode::Rename;
    }

    pub(super) fn start_workspace_rename(&mut self) {
        self.input = self.ws().name.clone();
        self.renaming = Renaming::Workspace;
        if self.collapsed {
            self.set_collapsed(false);
        }
        self.mode = Mode::Rename;
    }

    pub(super) fn finish_rename(&mut self) {
        let name = self.input.trim().to_owned();
        self.input.clear();
        self.mode = Mode::Normal;
        match std::mem::replace(&mut self.renaming, Renaming::Tab) {
            Renaming::Group(id) => {
                if let Some(group) = self.ws_mut().group_mut(id) {
                    group.name = name;
                }
                return;
            }
            Renaming::Workspace => {
                self.ws_mut().rename(Some(name));
                self.save();
                return;
            }
            Renaming::Tab => {}
        }
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

    fn rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.finish_rename(),
            KeyCode::Esc => {
                self.input.clear();
                self.renaming = Renaming::Tab;
                self.mode = Mode::Normal;
            }
            KeyCode::Tab => {
                if let Renaming::Group(id) = self.renaming {
                    self.cycle_group_color(id);
                }
            }
            _ => edit_line(&mut self.input, &key),
        }
    }

    pub(super) fn start_search(&mut self) {
        self.input.clear();
        self.picker_selected = 0;
        self.mode = Mode::Search;
    }

    fn search_key(&mut self, key: KeyEvent) {
        let results = self.search_results();
        if self.move_picker_selection(&key, results.len()) {
            return;
        }
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if let Some(&(w, t)) = results.get(self.picker_selected) {
                    self.switch_workspace(w);
                    self.activate(t);
                }
            }
            _ => {
                edit_line(&mut self.input, &key);
                self.picker_selected = 0;
            }
        }
    }

    pub(super) fn start_workspaces(&mut self, query: String) {
        self.input = query;
        self.picker_selected = 0;
        self.confirm_remove = None;
        self.mode = Mode::Workspaces;
    }

    fn workspaces_key(&mut self, key: KeyEvent) {
        let entries = self.picker_entries();
        if self.move_picker_selection(&key, entries.len()) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let selected = entries.get(self.picker_selected).cloned();
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
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
                if let Some(path) = path {
                    self.input = format!("{}/", workspace::pretty_path(&path).trim_end_matches('/'));
                    self.picker_selected = 0;
                }
            }
            KeyCode::Char('d') if ctrl => {
                let Some(PickerEntry::Workspace(index)) = selected else { return };
                let ws = &self.workspaces[index];
                let open = ws.tabs.len();
                // Removing closes the workspace's shells, so ask twice.
                if open > 0 && self.confirm_remove != Some(ws.id) {
                    self.confirm_remove = Some(ws.id);
                    return;
                }
                self.confirm_remove = None;
                self.remove_workspace(index);
                self.picker_selected = self.picker_selected.min(self.picker_entries().len().saturating_sub(1));
            }
            _ => {
                edit_line(&mut self.input, &key);
                self.picker_selected = 0;
                self.confirm_remove = None;
            }
        }
    }

    fn start_groups(&mut self) {
        self.input.clear();
        self.picker_selected = 0;
        self.mode = Mode::Groups;
    }

    fn groups_key(&mut self, key: KeyEvent) {
        let entries = self.group_entries();
        if self.move_picker_selection(&key, entries.len()) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let selected = entries.get(self.picker_selected).cloned();
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                match selected {
                    Some(GroupEntry::New(name)) => self.new_group(name),
                    Some(GroupEntry::Group(id)) => self.add_to_group(id),
                    None => {}
                }
                self.input.clear();
            }
            KeyCode::Char('d') if ctrl => {
                if let Some(GroupEntry::Group(id)) = selected {
                    self.ungroup(id);
                    self.picker_selected = self.picker_selected.min(self.group_entries().len().saturating_sub(1));
                }
            }
            _ => {
                edit_line(&mut self.input, &key);
                self.picker_selected = 0;
            }
        }
    }

    /// Arrow / Ctrl-n / Ctrl-p movement shared by the pickers.
    fn move_picker_selection(&mut self, key: &KeyEvent, len: usize) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = len.saturating_sub(1);
        let up = matches!(key.code, KeyCode::Up) || (ctrl && matches!(key.code, KeyCode::Char('p' | 'k')));
        let down = matches!(key.code, KeyCode::Down) || (ctrl && matches!(key.code, KeyCode::Char('n' | 'j')));
        if up {
            self.picker_selected = self.picker_selected.saturating_sub(1);
        } else if down {
            self.picker_selected = (self.picker_selected + 1).min(last);
        } else {
            return false;
        }
        self.confirm_remove = None;
        true
    }

    fn send_key(&mut self, key: KeyEvent) {
        self.selection = None;
        let Some(tab) = self.ws_mut().active_tab_mut() else { return };
        // Typing always jumps back to the live screen.
        tab.screen_mut().set_scrollback(0);
        let bytes = input::encode_key(&key, tab.screen().application_cursor());
        if !bytes.is_empty() {
            tab.write(bytes);
        }
    }
}

/// Minimal line editing for the rename / search inputs.
fn edit_line(text: &mut String, key: &KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('u') if ctrl => text.clear(),
        KeyCode::Char('w') if ctrl => {
            // Delete the last word, or path component in a path.
            let trimmed = text.trim_end_matches(['/', ' ']).len();
            let start = text[..trimmed].rfind([' ', '/']).map_or(0, |i| i + 1);
            text.truncate(start);
        }
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char(c) if !ctrl => text.push(c),
        _ => {}
    }
}
