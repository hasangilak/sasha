mod keys;
mod mouse;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{Event, KeyEventKind};
use ratatui::DefaultTerminal;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;

use crate::clipboard;
use crate::config::Config;
use crate::layout::{self, Hit, Layout};
use crate::tab::{Tab, TabId};
use crate::workspace::{self, Workspace, WorkspaceId};
use crate::{theme, ui};

/// Minimum time between two frames (~120 fps).
const FRAME: Duration = Duration::from_millis(8);
/// How often tab titles (foreground process, cwd) are refreshed.
const INFO_REFRESH: Duration = Duration::from_millis(500);
/// A background tab counts as busy (spinner) this long after its last output.
const BUSY: Duration = Duration::from_millis(800);
const SPINNER_STEP: Duration = Duration::from_millis(80);
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

pub enum AppEvent {
    Input(Event),
    Output(TabId, Vec<u8>),
    Exited(TabId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keys go to the shell.
    Normal,
    /// Entered with the prefix key: single keys manage tabs.
    Tab,
    /// Browsing the scrollback with vim-ish keys.
    Scroll,
    /// Editing the active tab's name in the sidebar (text in `App::input`).
    Rename,
    /// Finding a tab in any workspace (query in `App::input`).
    Search,
    /// Switching, adding and removing workspaces (query in `App::input`).
    Workspaces,
    Help,
    ConfirmQuit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Tab(usize),
    Divider,
    Select,
}

/// A mouse selection in screen coordinates (row, col) of one tab.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub tab: TabId,
    anchor: (u16, u16),
    head: (u16, u16),
}

impl Selection {
    /// Start and end (inclusive) in reading order.
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (start, end) = self.ordered();
        (start..=end).contains(&(row, col))
    }
}

/// A row of the workspace picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerEntry {
    Workspace(usize),
    /// A folder that isn't a workspace yet.
    AddFolder(PathBuf),
}

/// Activity of a workspace that isn't on screen, for its badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Busy,
    Bell,
    Unseen,
}

pub struct App {
    pub(crate) config: Config,
    pub(crate) mode: Mode,
    pub(crate) workspaces: Vec<Workspace>,
    /// Index of the workspace on screen.
    pub(crate) current: usize,
    previous_workspace: Option<WorkspaceId>,
    pub(crate) layout: Layout,
    pub(crate) hover: Option<Hit>,
    pub(crate) selection: Option<Selection>,
    /// Text being typed in rename / search / workspace mode.
    pub(crate) input: String,
    /// Highlighted row in the search results or workspace picker.
    pub(crate) picker_selected: usize,
    /// First visible item of the activity bar.
    pub(crate) activity_scroll: usize,
    /// Time, position and count of the last click in the pane.
    last_click: Option<(Instant, (u16, u16), u8)>,
    last_tab_click: Option<(Instant, usize)>,
    status: Option<(String, Instant)>,
    /// Workspace waiting for a second ^D before its tabs are closed.
    confirm_remove: Option<WorkspaceId>,
    sidebar_width: u16,
    collapsed: bool,
    show_activity: bool,
    drag: Option<Drag>,
    next_tab_id: TabId,
    next_workspace_id: WorkspaceId,
    events_tx: Sender<AppEvent>,
    events: Receiver<AppEvent>,
    started: Instant,
    cursor_style: u16,
    quit: bool,
}

impl App {
    pub fn new(config: Config, size: Size) -> Result<Self> {
        let (events_tx, events) = mpsc::channel();
        let mut app = Self {
            config,
            mode: Mode::Normal,
            workspaces: Vec::new(),
            current: 0,
            previous_workspace: None,
            layout: Layout::default(),
            hover: None,
            selection: None,
            input: String::new(),
            picker_selected: 0,
            activity_scroll: 0,
            last_click: None,
            last_tab_click: None,
            status: None,
            confirm_remove: None,
            sidebar_width: layout::DEFAULT_WIDTH,
            collapsed: false,
            show_activity: true,
            drag: None,
            next_tab_id: 0,
            next_workspace_id: 0,
            events_tx,
            events,
            started: Instant::now(),
            cursor_style: 0,
            quit: false,
        };
        app.relayout(size);

        // Open a terminal where sasha was started, in the workspace that
        // contains that folder (or a new one for it).
        let launch = std::env::current_dir()
            .ok()
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let index = match app.workspace_for(&launch) {
            Some(index) => index,
            None => app.add_workspace(launch.clone()),
        };
        app.current = index;
        let tab = app.spawn_tab(&launch)?;
        app.workspaces[index].tabs.push(tab);
        let last = app.ws().tabs.len() - 1;
        app.activate(last);
        Ok(app)
    }

    /// The workspace on screen.
    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.current]
    }

    fn ws_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.current]
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.ws().active_tab()
    }

    pub fn tab_count(&self) -> usize {
        self.workspaces.iter().map(|w| w.tabs.len()).sum()
    }

    /// Workspace and index of the tab with this id.
    fn find_tab(&self, id: TabId) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(w, ws)| ws.tabs.iter().position(|t| t.id == id).map(|t| (w, t)))
    }

    /// Whether a tab that isn't on screen is streaming output right now.
    pub fn is_busy(&self, tab: &Tab) -> bool {
        Some(tab.id) != self.active_tab().map(|t| t.id) && tab.last_activity.elapsed() < BUSY
    }

    /// The most noteworthy thing happening in a workspace's tabs.
    pub fn workspace_activity(&self, index: usize) -> Option<Activity> {
        let tabs = &self.workspaces[index].tabs;
        if tabs.iter().any(|t| self.is_busy(t)) {
            Some(Activity::Busy)
        } else if tabs.iter().any(|t| t.bell) {
            Some(Activity::Bell)
        } else if tabs.iter().any(|t| t.unseen) {
            Some(Activity::Unseen)
        } else {
            None
        }
    }

    fn animating(&self) -> bool {
        self.workspaces.iter().flat_map(|w| &w.tabs).any(|t| self.is_busy(t))
    }

    pub fn spinner_frame(&self) -> usize {
        (self.started.elapsed().as_millis() / SPINNER_STEP.as_millis()) as usize
    }

    pub fn dragging_divider(&self) -> bool {
        self.drag == Some(Drag::Divider)
    }

    /// Paint the active tab in the accent colour while keys drive the tabs.
    pub fn highlight_active(&self) -> bool {
        self.mode == Mode::Tab
    }

    fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    /// Badge and hint text for the bottom of the sidebar.
    pub fn footer(&self) -> (Option<(&'static str, Color)>, String) {
        let short = self.layout.collapsed;
        let prefix = self.config.prefix.label();
        if let Some((message, at)) = &self.status
            && at.elapsed() < STATUS_TIMEOUT
            && matches!(self.mode, Mode::Normal | Mode::Tab)
        {
            let badge = (self.mode == Mode::Tab).then_some((if short { "TAB" } else { "TABS" }, theme::ACCENT));
            return (badge, message.clone());
        }
        match self.mode {
            Mode::Normal => (None, format!("{prefix} tab keys · {prefix} ? help")),
            Mode::Scroll => (
                Some((if short { "SCR" } else { "SCROLL" }, theme::YELLOW)),
                "j/k ^u/^d g/G · q exit".to_owned(),
            ),
            Mode::Tab => (
                Some((if short { "TAB" } else { "TABS" }, theme::ACCENT)),
                "j/k tabs · h/l spaces · ? help".to_owned(),
            ),
            Mode::Rename => (
                Some((if short { "REN" } else { "RENAME" }, theme::GREEN)),
                "⏎ save · esc".to_owned(),
            ),
            Mode::Search => (
                Some((if short { "/" } else { "SEARCH" }, theme::PURPLE)),
                "↑↓ ⏎ go · esc".to_owned(),
            ),
            Mode::Workspaces => (
                Some((if short { "WS" } else { "SPACES" }, theme::ORANGE)),
                "⏎ open · ⇥ complete".to_owned(),
            ),
            Mode::Help => (Some(("?", theme::ACCENT)), "any key to close".to_owned()),
            Mode::ConfirmQuit => (Some((if short { "Q?" } else { "QUIT?" }, theme::RED)), "y / n".to_owned()),
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let input_tx = self.events_tx.clone();
        thread::spawn(move || {
            while let Ok(event) = crossterm::event::read() {
                if input_tx.send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
        });

        let mut dirty = true;
        let mut last_draw = Instant::now();
        let mut last_refresh = Instant::now();
        let mut was_animating = false;
        while !self.quit {
            let timeout = if dirty {
                FRAME.saturating_sub(last_draw.elapsed())
            } else if was_animating {
                SPINNER_STEP
            } else {
                INFO_REFRESH
            };
            match self.events.recv_timeout(timeout) {
                Ok(event) => {
                    self.handle(event);
                    // Drain what's queued, but never starve the renderer.
                    let started = Instant::now();
                    while started.elapsed() < FRAME {
                        let Ok(event) = self.events.try_recv() else { break };
                        self.handle(event);
                    }
                    dirty = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if last_refresh.elapsed() >= INFO_REFRESH {
                last_refresh = Instant::now();
                self.refresh_tabs();
                dirty = true;
            }
            // Keep spinners moving, and redraw once more when they stop.
            let animating = self.animating();
            dirty |= animating || was_animating;
            was_animating = animating;

            if dirty && last_draw.elapsed() >= FRAME && !self.quit {
                self.relayout(terminal.size()?);
                terminal.draw(|frame| ui::draw(frame, self))?;
                self.sync_cursor_style()?;
                last_draw = Instant::now();
                dirty = false;
            }
        }
        Ok(())
    }

    /// Recomputes the layout and keeps every pty sized to the pane.
    fn relayout(&mut self, size: Size) {
        let area = Rect::new(0, 0, size.width, size.height);
        self.layout = Layout::new(area, self.show_activity, self.sidebar_width, self.collapsed);
        self.activity_scroll = self.layout.clamp_activity_scroll(self.activity_scroll, self.workspaces.len(), None);
        let (rows, cols) = self.pty_size();
        for ws in &mut self.workspaces {
            ws.scroll = self.layout.clamp_scroll(ws.scroll, ws.tabs.len(), ws.pinned_count(), None);
            for tab in &mut ws.tabs {
                tab.resize(rows, cols);
            }
        }
    }

    fn relayout_now(&mut self) {
        let size = self.layout.area.as_size();
        self.relayout(size);
    }

    /// Mirrors the active program's cursor shape (e.g. vim's insert bar).
    fn sync_cursor_style(&mut self) -> Result<()> {
        let style = self.active_tab().map_or(0, Tab::cursor_style);
        if style == self.cursor_style {
            return Ok(());
        }
        self.cursor_style = style;
        let style = match style {
            1 => SetCursorStyle::BlinkingBlock,
            2 => SetCursorStyle::SteadyBlock,
            3 => SetCursorStyle::BlinkingUnderScore,
            4 => SetCursorStyle::SteadyUnderScore,
            5 => SetCursorStyle::BlinkingBar,
            6 => SetCursorStyle::SteadyBar,
            _ => SetCursorStyle::DefaultUserShape,
        };
        crossterm::execute!(std::io::stdout(), style)?;
        Ok(())
    }

    fn pty_size(&self) -> (u16, u16) {
        (self.layout.pane.height.max(1), self.layout.pane.width.max(1))
    }

    fn set_collapsed(&mut self, collapsed: bool) {
        self.collapsed = collapsed;
        self.relayout_now();
    }

    fn toggle_activity_bar(&mut self) {
        self.show_activity = !self.show_activity;
        self.relayout_now();
    }

    fn resize_sidebar(&mut self, delta: i16) {
        self.collapsed = false;
        self.sidebar_width = self.sidebar_width.saturating_add_signed(delta).max(layout::MIN_WIDTH);
        self.relayout_now();
        // Layout clamps to the screen; keep the stored width in sync with it.
        self.sidebar_width = self.layout.sidebar.width.max(layout::MIN_WIDTH);
    }

    fn request_quit(&mut self) {
        if self.tab_count() > 1 {
            self.mode = Mode::ConfirmQuit;
        } else {
            self.quit = true;
        }
    }

    // ── Workspaces ──────────────────────────────────────────────────────

    /// The workspace whose folder contains `path` (the deepest one wins).
    fn workspace_for(&self, path: &Path) -> Option<usize> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.workspaces
            .iter()
            .enumerate()
            .filter(|(_, ws)| path.starts_with(&ws.path))
            .max_by_key(|(_, ws)| ws.path.components().count())
            .map(|(index, _)| index)
    }

    /// Adds a workspace for `path`, or returns the existing one.
    fn add_workspace(&mut self, path: PathBuf) -> usize {
        let path = path.canonicalize().unwrap_or(path);
        if let Some(index) = self.workspaces.iter().position(|w| w.path == path) {
            return index;
        }
        let id = self.next_workspace_id;
        self.next_workspace_id += 1;
        self.workspaces.push(Workspace::new(id, path));
        self.workspaces.len() - 1
    }

    fn remove_workspace(&mut self, index: usize) {
        if self.workspaces.len() <= 1 {
            self.set_status("can't remove the only workspace");
            return;
        }
        // Dropping the workspace hangs up its shells.
        let removed = self.workspaces.remove(index);
        if index < self.current || self.current >= self.workspaces.len() {
            self.current = self.current.saturating_sub(1);
        }
        let current = self.current;
        self.switch_workspace(current);
        self.set_status(format!("removed workspace {}", removed.name));
    }

    fn switch_workspace(&mut self, index: usize) {
        if index >= self.workspaces.len() {
            return;
        }
        if index != self.current {
            self.previous_workspace = Some(self.ws().id);
        }
        self.current = index;
        self.selection = None;
        self.activity_scroll =
            self.layout.clamp_activity_scroll(self.activity_scroll, self.workspaces.len(), Some(index));
        // A workspace never shows an empty pane.
        if self.ws().tabs.is_empty() {
            if let Err(err) = self.new_tab() {
                self.set_status(format!("can't start a shell: {err}"));
            }
        } else {
            let active = self.ws().active;
            self.activate(active);
        }
    }

    fn cycle_workspace(&mut self, delta: isize) {
        let len = self.workspaces.len() as isize;
        self.switch_workspace((self.current as isize + delta).rem_euclid(len) as usize);
    }

    fn switch_to_previous_workspace(&mut self) {
        if let Some(index) = self.workspaces.iter().position(|w| Some(w.id) == self.previous_workspace) {
            self.switch_workspace(index);
        }
    }

    /// The current workspace ran out of terminals: go to the most recent
    /// one that still has some, or quit when none do.
    fn leave_empty_workspace(&mut self) {
        let has_tabs = |w: &Workspace| !w.tabs.is_empty();
        let target = self
            .workspaces
            .iter()
            .position(|w| Some(w.id) == self.previous_workspace && has_tabs(w))
            .or_else(|| self.workspaces.iter().position(has_tabs));
        match target {
            Some(index) => self.switch_workspace(index),
            None => self.quit = true,
        }
    }

    /// Shown in the picker after one ^D on a workspace that has open tabs.
    pub fn remove_warning(&self) -> Option<String> {
        let ws = self.workspaces.iter().find(|w| Some(w.id) == self.confirm_remove)?;
        let count = ws.tabs.len();
        Some(format!("^D again closes {count} tab{} in {}", if count == 1 { "" } else { "s" }, ws.name))
    }

    /// Rows of the workspace picker for the current query: matching
    /// workspaces, or folders to add when the query looks like a path.
    pub fn picker_entries(&self) -> Vec<PickerEntry> {
        let query = self.input.trim();
        let base = self
            .active_tab()
            .and_then(Tab::cwd)
            .map_or_else(|| self.ws().path.clone(), Path::to_path_buf);
        let as_entry = |path: PathBuf| match self.workspaces.iter().position(|w| w.path == path) {
            Some(index) => PickerEntry::Workspace(index),
            None => PickerEntry::AddFolder(path),
        };

        if query.starts_with(['/', '~', '.']) {
            // Path mode: complete the last component against sub-folders.
            let full = workspace::expand_path(query, &base);
            let (dir, partial) = if query.ends_with('/') || query == "~" {
                (full.clone(), String::new())
            } else {
                let partial = full.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                (full.parent().map_or_else(|| full.clone(), Path::to_path_buf), partial)
            };
            let mut entries = Vec::new();
            if full.is_dir() {
                entries.push(as_entry(full.canonicalize().unwrap_or(full)));
            }
            let mut children: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    name.starts_with(&partial) && (partial.starts_with('.') || !name.starts_with('.'))
                })
                .collect();
            children.sort();
            for child in children.into_iter().take(200) {
                let entry = as_entry(child.canonicalize().unwrap_or(child));
                if !entries.contains(&entry) {
                    entries.push(entry);
                }
            }
            return entries;
        }

        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut entries: Vec<PickerEntry> = (0..self.workspaces.len())
            .filter(|&i| {
                let ws = &self.workspaces[i];
                let haystack = format!("{} {}", ws.name, ws.path.display()).to_lowercase();
                terms.iter().all(|t| haystack.contains(t))
            })
            .map(PickerEntry::Workspace)
            .collect();
        // Offer the folder you're in when it isn't a workspace yet.
        if terms.is_empty() && !self.workspaces.iter().any(|w| w.path == base) {
            entries.push(PickerEntry::AddFolder(base));
        }
        entries
    }

    // ── Tabs ────────────────────────────────────────────────────────────

    fn spawn_tab(&mut self, cwd: &Path) -> Result<Tab> {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        Tab::spawn(id, self.pty_size(), Some(cwd), self.events_tx.clone())
    }

    /// Opens a tab in the current workspace, in the active tab's folder if
    /// that's inside the workspace, else in the workspace root.
    fn new_tab(&mut self) -> Result<()> {
        let ws = self.ws();
        let cwd = ws
            .active_tab()
            .and_then(Tab::cwd)
            .filter(|cwd| cwd.starts_with(&ws.path))
            .map_or_else(|| ws.path.clone(), Path::to_path_buf);
        let tab = self.spawn_tab(&cwd)?;
        self.ws_mut().tabs.push(tab);
        let last = self.ws().tabs.len() - 1;
        self.activate(last);
        Ok(())
    }

    fn activate(&mut self, index: usize) {
        let ws = &mut self.workspaces[self.current];
        if index >= ws.tabs.len() {
            return;
        }
        if index != ws.active
            && let Some(current) = ws.tabs.get(ws.active)
        {
            ws.previous = Some(current.id);
        }
        ws.active = index;
        let tab = &mut ws.tabs[index];
        tab.unseen = false;
        tab.bell = false;
        ws.scroll = self.layout.clamp_scroll(ws.scroll, ws.tabs.len(), ws.pinned_count(), Some(index));
        self.selection = None;
    }

    fn close_tab(&mut self, index: usize) {
        self.remove_tab(self.current, index);
    }

    fn remove_tab(&mut self, ws_index: usize, index: usize) {
        let Some(ws) = self.workspaces.get_mut(ws_index) else { return };
        if index >= ws.tabs.len() {
            return;
        }
        // Dropping the tab hangs up its shell.
        ws.tabs.remove(index);
        if index < ws.active || ws.active >= ws.tabs.len() {
            ws.active = ws.active.saturating_sub(1);
        }
        if ws_index != self.current {
            return;
        }
        if self.ws().tabs.is_empty() {
            self.leave_empty_workspace();
        } else {
            let active = self.ws().active;
            self.activate(active);
        }
    }

    /// Activates the tab `delta` steps away, wrapping around.
    fn cycle(&mut self, delta: isize) {
        let len = self.ws().tabs.len() as isize;
        if len > 0 {
            self.activate((self.ws().active as isize + delta).rem_euclid(len) as usize);
        }
    }

    /// Chrome numbering: 1-8 pick a tab, 9 is always the last one.
    fn jump(&mut self, digit: char) {
        match digit {
            '9' => self.activate(self.ws().tabs.len().saturating_sub(1)),
            '1'..='8' => self.activate(digit as usize - '1' as usize),
            _ => {}
        }
    }

    fn activate_previous(&mut self) {
        let ws = self.ws();
        if let Some(index) = ws.tabs.iter().position(|t| Some(t.id) == ws.previous) {
            self.activate(index);
        }
    }

    /// Moves the active tab by `delta`, staying within its section
    /// (pinned or not).
    fn shift_active(&mut self, delta: isize) {
        let ws = self.ws();
        let pinned = ws.pinned_count();
        let section = if ws.active < pinned { 0..pinned } else { pinned..ws.tabs.len() };
        let to = ws.active as isize + delta;
        if to >= 0 && section.contains(&(to as usize)) {
            self.move_tab(ws.active, to as usize);
        }
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        let ws = &mut self.workspaces[self.current];
        if from == to || from >= ws.tabs.len() || to >= ws.tabs.len() {
            return;
        }
        let active_id = ws.tabs[ws.active].id;
        let tab = ws.tabs.remove(from);
        ws.tabs.insert(to, tab);
        ws.active = ws.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
        ws.scroll = self.layout.clamp_scroll(ws.scroll, ws.tabs.len(), ws.pinned_count(), Some(to));
    }

    /// Indices (workspace, tab) of the tabs matching the search query.
    pub fn search_results(&self) -> Vec<(usize, usize)> {
        let query = self.input.to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        let mut results = Vec::new();
        for (w, ws) in self.workspaces.iter().enumerate() {
            for (t, tab) in ws.tabs.iter().enumerate() {
                let haystack = format!(
                    "{} {} {} {}",
                    tab.title(),
                    tab.process_name(),
                    tab.cwd().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                    ws.name,
                )
                .to_lowercase();
                if terms.iter().all(|term| haystack.contains(term)) {
                    results.push((w, t));
                }
            }
        }
        results
    }

    fn refresh_tabs(&mut self) {
        let mut exited = Vec::new();
        for (w, ws) in self.workspaces.iter_mut().enumerate() {
            for (t, tab) in ws.tabs.iter_mut().enumerate() {
                if tab.has_exited() {
                    exited.push((w, t));
                }
            }
        }
        for (w, t) in exited.into_iter().rev() {
            self.remove_tab(w, t);
        }
        for tab in self.workspaces.iter_mut().flat_map(|w| &mut w.tabs) {
            tab.refresh_info();
        }
    }

    fn handle(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => self.handle_key(key),
            AppEvent::Input(Event::Mouse(mouse)) => self.handle_mouse(mouse),
            AppEvent::Input(Event::Paste(text)) => self.paste(&text),
            AppEvent::Input(_) => {}
            AppEvent::Output(id, bytes) => self.handle_output(id, &bytes),
            AppEvent::Exited(id) => {
                if let Some((w, t)) = self.find_tab(id) {
                    self.remove_tab(w, t);
                }
            }
        }
    }

    fn handle_output(&mut self, id: TabId, bytes: &[u8]) {
        let Some((w, t)) = self.find_tab(id) else { return };
        let on_screen = w == self.current && t == self.ws().active;
        let tab = &mut self.workspaces[w].tabs[t];
        if let Some(data) = tab.process(bytes) {
            clipboard::copy_base64(&data);
        }
        // Shells redraw their prompt after a resize; that isn't news.
        if !tab.recently_resized() {
            tab.last_activity = Instant::now();
            if !on_screen {
                tab.unseen = true;
            }
        }
    }

    fn paste(&mut self, text: &str) {
        match self.mode {
            Mode::Rename | Mode::Search | Mode::Workspaces => {
                self.input.push_str(text.lines().next().unwrap_or_default());
                return;
            }
            _ => {}
        }
        let Some(tab) = self.ws_mut().active_tab_mut() else { return };
        // Terminals send pasted line breaks as carriage returns.
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if tab.screen().bracketed_paste() {
            tab.write(format!("\x1b[200~{text}\x1b[201~"));
        } else {
            tab.write(text);
        }
    }
}
