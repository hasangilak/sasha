//! The header's behaviour: the title of the terminal on screen, which opens
//! that terminal's menu, and the buttons on the right for the bars, a new
//! terminal and a menu of everything else.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use super::App;
use super::state::{Command, Menu, Mode, Picker, PickerKind};
use crate::layout::HeaderButton;
use crate::tab::split_glyph;
use crate::tower::Engine;
use crate::ui;

/// A row of a menu: what it says, its key after the prefix, and what it
/// does.
#[derive(Clone, Debug)]
pub struct MenuEntry {
    pub label: String,
    pub key: &'static str,
    pub command: Command,
}

impl App {
    /// The terminal on screen's title: what runs in it (its status glyph
    /// left to the icon), else its name.
    pub fn header_title(&self) -> String {
        let Some(tab) = self.active_tab() else { return String::new() };
        match tab.running() {
            Some(running) => split_glyph(&running).1.to_owned(),
            None => tab.prefix(),
        }
    }

    /// Whether what a header button shows is showing, to light it up.
    pub fn header_on(&self, button: HeaderButton) -> bool {
        match button {
            HeaderButton::Sidebar => !self.collapsed && !self.layout.sidebar.is_empty(),
            HeaderButton::NewTerminal => false,
            HeaderButton::Tower => self.tower_on_screen(),
            HeaderButton::PullRequests => !self.layout.pr_bar.is_empty(),
            HeaderButton::Ports => !self.layout.ports_bar.is_empty(),
            HeaderButton::Files => !self.layout.files_bar.is_empty(),
            HeaderButton::More => self.menu() == Some(Menu::More),
        }
    }

    /// The header menu that's open, if one is.
    pub fn menu(&self) -> Option<Menu> {
        match self.picker()?.kind {
            PickerKind::Menu(menu) => Some(menu),
            _ => None,
        }
    }

    pub(super) fn header_click(&mut self, button: HeaderButton) {
        match button {
            HeaderButton::Sidebar => self.set_collapsed(!self.collapsed),
            HeaderButton::NewTerminal => self.run_command(Command::NewTab),
            HeaderButton::Tower => self.run_command(Command::Tower),
            HeaderButton::PullRequests => self.toggle_prs_bar(),
            HeaderButton::Ports => self.toggle_ports_bar(),
            HeaderButton::Files => self.toggle_files_bar(),
            HeaderButton::More => self.open_menu(Menu::More),
        }
    }

    pub(super) fn open_menu(&mut self, menu: Menu) {
        self.enter(Mode::Picker(Picker::new(PickerKind::Menu(menu), String::new())));
    }

    /// A menu's rows that match what's typed.
    pub fn menu_entries(&self, menu: Menu) -> Vec<MenuEntry> {
        let entry = |label: &str, key, command| MenuEntry { label: label.to_owned(), key, command };
        let entries = match menu {
            Menu::Terminal => {
                let tab = self.active_tab();
                let pinned = tab.is_some_and(|t| t.pinned);
                let grouped = tab.is_some_and(|t| t.group.is_some());
                let mut entries = vec![
                    entry("Rename", "r", Command::Rename),
                    entry(if pinned { "Unpin" } else { "Pin" }, "p", Command::Pin),
                    entry(if grouped { "Move to another group" } else { "Add to a group" }, "m", Command::GroupPicker),
                ];
                if grouped {
                    entries.push(entry("Take out of its group", "u", Command::Ungroup));
                }
                if tab.is_some_and(|t| t.tower) {
                    let other = match self.tower_engine() {
                        Engine::Claude => entry("Run on Codex (ChatGPT subscription)", "", Command::TowerOn(Engine::Codex)),
                        Engine::Codex => entry("Run on Claude (Claude subscription)", "", Command::TowerOn(Engine::Claude)),
                    };
                    entries.insert(0, other);
                }
                entries.push(entry("Scroll back", "[", Command::Scroll));
                entries.push(entry("Close", "x", Command::Close));
                entries
            }
            Menu::More => vec![
                entry("New terminal", "t", Command::NewTab),
                entry(if self.tower_on_screen() { "Back from the tower" } else { "Tower: steer every terminal" }, "T", Command::Tower),
                entry("Search terminals", "/", Command::Search),
                entry("Workspaces", "w", Command::Workspaces),
                entry("Browse files", "F", Command::Files),
                entry("Pull requests", "P", Command::PullRequests),
                entry("Ports", "O", Command::Ports),
                entry(
                    if self.awake.is_some() { "Let the computer sleep" } else { "Keep the computer awake" },
                    "C",
                    Command::KeepAwake,
                ),
                entry(
                    if self.current_only { "Show every workspace's terminals" } else { "Show this workspace's only" },
                    "o",
                    Command::ToggleStacked,
                ),
                entry("Workspace bar: names or badges", "a", Command::CycleWorkspaceBar),
                entry("Keys", "?", Command::Help),
                entry("Detach: terminals keep running", "d", Command::Detach),
                entry("Quit", "q", Command::Quit),
            ],
        };
        let query = self.typed().to_lowercase();
        entries.into_iter().filter(|e| query.split_whitespace().all(|word| e.label.to_lowercase().contains(word))).collect()
    }

    pub(super) fn menu_key(&mut self, key: KeyEvent, menu: Menu) {
        let entries = self.menu_entries(menu);
        match key.code {
            KeyCode::Esc => self.leave(),
            KeyCode::Enter => {
                let command = self.picker().and_then(|p| entries.get(p.selected)).map(|e| e.command);
                self.leave();
                if let Some(command) = command {
                    self.run_command(command);
                }
            }
            _ => self.picker_key(&key, entries.len()),
        }
    }

    /// How many rows the picker that's open has.
    pub fn picker_len(&self) -> usize {
        match self.picker().map(|p| p.kind) {
            Some(PickerKind::Search) => self.search_results().len(),
            Some(PickerKind::Workspaces) => self.picker_entries().len(),
            Some(PickerKind::Groups) => self.group_entries().len(),
            Some(PickerKind::Menu(menu)) => self.menu_entries(menu).len(),
            None => 0,
        }
    }

    /// The mouse over an open picker: moving highlights a row, a click
    /// picks it, the wheel moves through them. True when it was over it.
    pub(super) fn picker_mouse(&mut self, mouse: &MouseEvent) -> bool {
        let Some(frame) = ui::picker_frame(self) else { return false };
        let pos = Position::new(mouse.column, mouse.row);
        if !frame.area.contains(pos) {
            return false;
        }
        let rows = self.picker_len();
        let row = frame.list.contains(pos).then(|| frame.first + usize::from(mouse.row - frame.list.y)).filter(|&r| r < rows);
        let Mode::Picker(picker) = &mut self.mode else { return true };
        match mouse.kind {
            MouseEventKind::Moved => {
                if let Some(row) = row {
                    picker.selected = row;
                }
            }
            MouseEventKind::ScrollDown => picker.selected = (picker.selected + 1).min(rows.saturating_sub(1)),
            MouseEventKind::ScrollUp => picker.selected = picker.selected.saturating_sub(1),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(row) = row {
                    picker.selected = row;
                    self.handle_key(KeyEvent::from(KeyCode::Enter));
                }
            }
            _ => {}
        }
        true
    }
}
