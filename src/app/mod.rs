mod keys;
mod mouse;

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{Event, KeyEventKind};
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;

use crate::clipboard;
use crate::config::Config;
use crate::layout::{self, Hit, Layout};
use crate::protocol::{self, Hello};
use crate::server::{Attached, ClientId};
use crate::store::{self, SavedTab, SavedWorkspace, State};
use crate::tab::{Tab, TabId};
use crate::workspace::{self, Group, GroupId, Workspace, WorkspaceId};
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
    Output(TabId, Vec<u8>),
    Exited(TabId),
    /// A client wants to show the tabs.
    Attach(ClientId, Box<Hello>, UnixStream),
    Input(ClientId, Event),
    /// A client's connection closed.
    Gone(ClientId),
    /// `sasha --kill-server`.
    Kill,
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
    /// Putting the active tab into a tab group (query in `App::input`).
    Groups,
    Help,
    ConfirmQuit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Tab(TabId),
    Divider,
    ActivityDivider,
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

/// What rename mode is renaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renaming {
    /// The active tab's prefix.
    Tab,
    Group(GroupId),
    /// The current workspace.
    Workspace,
}

/// A row of the tab group picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupEntry {
    /// Start a group with this name (which may be empty).
    New(String),
    Group(GroupId),
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
    /// The client showing the tabs, if any.
    client: Option<Attached>,
    /// This server's `protocol::build_id`.
    build: u64,
    /// Whether a client has attached before (the first one opens a tab).
    attached_before: bool,
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
    last_group_click: Option<(Instant, GroupId)>,
    last_workspace_click: Option<(Instant, usize)>,
    last_name_click: Option<Instant>,
    pub(crate) renaming: Renaming,
    status: Option<(String, Instant)>,
    /// Workspace waiting for a second ^D before its tabs are closed.
    confirm_remove: Option<WorkspaceId>,
    /// Pinned tab waiting for a second `x` before it's closed.
    confirm_close: Option<TabId>,
    sidebar_width: u16,
    collapsed: bool,
    /// Width of the workspace bar; 0 hides it.
    activity_width: u16,
    /// The width it goes back to when it shows names again.
    wide_activity_width: u16,
    drag: Option<Drag>,
    next_tab_id: TabId,
    next_workspace_id: WorkspaceId,
    next_group_id: GroupId,
    events_tx: Sender<AppEvent>,
    events: Receiver<AppEvent>,
    started: Instant,
    cursor_style: u16,
    quit: bool,
}

impl App {
    /// Restores the saved workspaces and pinned tabs; the first `attach`
    /// opens a tab where sasha was started.
    pub fn new(config: Config, size: Size, events_tx: Sender<AppEvent>, events: Receiver<AppEvent>) -> Self {
        let mut app = Self {
            config,
            client: None,
            build: protocol::build_id(),
            attached_before: false,
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
            last_group_click: None,
            last_workspace_click: None,
            last_name_click: None,
            renaming: Renaming::Tab,
            status: None,
            confirm_remove: None,
            confirm_close: None,
            sidebar_width: layout::DEFAULT_WIDTH,
            collapsed: false,
            activity_width: layout::ACTIVITY_COMPACT,
            wide_activity_width: layout::ACTIVITY_WIDE_DEFAULT,
            drag: None,
            next_tab_id: 0,
            next_workspace_id: 0,
            next_group_id: 0,
            events_tx,
            events,
            started: Instant::now(),
            cursor_style: 0,
            quit: false,
        };
        app.relayout(size);

        let (state, warning) = store::load();
        for saved in state.workspaces {
            let index = app.add_workspace(saved.path);
            app.workspaces[index].rename(saved.name);
            for pinned in saved.pinned {
                match app.spawn_tab(&pinned.cwd) {
                    Ok(mut tab) => {
                        tab.pinned = true;
                        tab.custom_prefix = pinned.title;
                        app.workspaces[index].tabs.push(tab);
                    }
                    Err(err) => app.set_status(format!("can't restore a pinned tab: {err}")),
                }
            }
        }
        if let Some(warning) = warning {
            app.set_status(warning);
        }
        app
    }

    // ── Clients ─────────────────────────────────────────────────────────

    /// Shows the tabs on a client's terminal. One client at a time: a new
    /// one takes over from the last, like `tmux attach -d`.
    pub fn attach(&mut self, id: ClientId, hello: Hello, mut stream: UnixStream) {
        if hello.version != protocol::VERSION {
            let message = "[this sasha doesn't match the one running · stop it with sasha --kill-server]";
            let _ = protocol::write_frame(&mut stream, protocol::EXIT, message.as_bytes());
            return;
        }
        let size = Size::new(hello.size.0, hello.size.1);
        let fallback = stream.try_clone();
        let client = match Attached::new(id, stream, size) {
            Ok(client) => client,
            Err(err) => {
                if let Ok(mut stream) = fallback {
                    let message = format!("[can't draw for this window: {err}]");
                    let _ = protocol::write_frame(&mut stream, protocol::EXIT, message.as_bytes());
                }
                // Nothing to keep running for if this was the first client.
                self.quit = self.tab_count() == 0;
                return;
            }
        };
        if let Some(old) = self.client.replace(client) {
            old.exit("[sasha opened in another window]");
        }
        self.reset_modes();
        theme::init(hello.colors.0, hello.colors.1);
        self.config = hello.config;
        // A fresh terminal has the default cursor.
        self.cursor_style = 0;
        self.relayout(size);
        if let Err(err) = self.open_launch_folder(&hello.cwd) {
            if let Some(client) = self.client.take() {
                client.exit(&format!("can't start a shell: {err}"));
            }
            self.quit = self.tab_count() == 0;
            return;
        }
        if hello.build != self.build {
            let prefix = self.config.prefix.label();
            self.set_status(format!("sasha was rebuilt · {prefix} q and reopen to update"));
        }
    }

    /// The first time, opens a tab in the folder sasha was started in. Later
    /// it shows that folder's workspace, unless sasha was started in the home
    /// folder, where every new terminal window starts: then it carries on
    /// where you left off.
    fn open_launch_folder(&mut self, launch: &Path) -> Result<()> {
        let first = !self.attached_before;
        self.attached_before = true;
        if !first && std::env::home_dir().is_some_and(|home| home == launch) {
            return Ok(());
        }
        let index = match self.workspace_for(launch) {
            Some(index) => index,
            None => {
                let index = self.add_workspace(launch.to_path_buf());
                self.save();
                index
            }
        };
        if first {
            self.current = index;
            let tab = self.spawn_tab(launch)?;
            self.ws_mut().tabs.push(tab);
            let last = self.ws().tabs.len() - 1;
            self.activate(last);
        } else if index != self.current {
            self.switch_workspace(index);
        }
        Ok(())
    }

    /// Lets the client go; the tabs keep running.
    fn detach(&mut self) {
        if let Some(client) = self.client.take() {
            client.exit("[detached · run sasha to come back]");
        }
        self.reset_modes();
    }

    /// Leaves half-done interactions behind when a client goes.
    fn reset_modes(&mut self) {
        self.mode = Mode::Normal;
        self.input.clear();
        self.renaming = Renaming::Tab;
        self.hover = None;
        self.drag = None;
        self.selection = None;
    }

    fn resize_client(&mut self, size: Size) {
        let Some(client) = &mut self.client else { return };
        if client.resize(size).is_err() {
            self.detach();
            return;
        }
        self.relayout(size);
    }

    /// Bytes for the client's terminal itself (cursor shape, clipboard).
    fn send_to_terminal(&mut self, bytes: &[u8]) {
        if let Some(client) = &mut self.client
            && client.send(bytes).is_err()
        {
            self.detach();
        }
    }

    /// Copies to the clipboard, through the client's terminal if need be.
    fn copy_to_clipboard(&mut self, text: &str) {
        if let Some(osc) = clipboard::copy(text) {
            self.send_to_terminal(osc.as_bytes());
        }
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

    pub fn dragging_activity_divider(&self) -> bool {
        self.drag == Some(Drag::ActivityDivider)
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
        let p = theme::palette();
        let short = self.layout.collapsed;
        let prefix = self.config.prefix.label();
        if let Some((message, at)) = &self.status
            && at.elapsed() < STATUS_TIMEOUT
            && matches!(self.mode, Mode::Normal | Mode::Tab)
        {
            let badge = (self.mode == Mode::Tab).then_some((if short { "TAB" } else { "TABS" }, p.accent));
            return (badge, message.clone());
        }
        match self.mode {
            Mode::Normal => (None, format!("{prefix} tab keys · {prefix} ? help")),
            Mode::Scroll => (
                Some((if short { "SCR" } else { "SCROLL" }, p.yellow)),
                "j/k ^u/^d g/G · q exit".to_owned(),
            ),
            Mode::Tab => (
                Some((if short { "TAB" } else { "TABS" }, p.accent)),
                "j/k tabs · h/l spaces · p pin".to_owned(),
            ),
            Mode::Rename if matches!(self.renaming, Renaming::Group(_)) => (
                Some((if short { "GRP" } else { "GROUP" }, p.green)),
                "⏎ save · ⇥ colour · esc".to_owned(),
            ),
            Mode::Rename if self.renaming == Renaming::Workspace => (
                Some((if short { "WS" } else { "SPACE" }, p.green)),
                "⏎ save · empty = folder".to_owned(),
            ),
            Mode::Rename => (
                Some((if short { "REN" } else { "RENAME" }, p.green)),
                "⏎ save · esc".to_owned(),
            ),
            Mode::Search => (
                Some((if short { "/" } else { "SEARCH" }, p.purple)),
                "↑↓ ⏎ go · esc".to_owned(),
            ),
            Mode::Workspaces => (
                Some((if short { "WS" } else { "SPACES" }, p.orange)),
                "⏎ open · ⇥ complete".to_owned(),
            ),
            Mode::Groups => (
                Some((if short { "GRP" } else { "GROUP" }, p.cyan)),
                "⏎ add tab · ^D ungroup".to_owned(),
            ),
            Mode::Help => (Some(("?", p.accent)), "any key to close".to_owned()),
            Mode::ConfirmQuit => (Some((if short { "Q?" } else { "QUIT?" }, p.red)), "y / n".to_owned()),
        }
    }

    pub fn run(&mut self) -> Result<()> {
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
                self.draw();
                last_draw = Instant::now();
                dirty = false;
            }
        }
        if let Some(client) = self.client.take() {
            client.exit("");
        }
        self.save();
        Ok(())
    }

    fn draw(&mut self) {
        let Some(mut client) = self.client.take() else { return };
        let drawn = client.terminal.draw(|frame| ui::draw(frame, self)).map(|_| ());
        self.client = Some(client);
        // A client that can't be written to has gone.
        if drawn.and_then(|()| self.sync_cursor_style()).is_err() {
            self.detach();
        }
    }

    /// Writes the workspaces and pinned tabs to disk; failures show up in
    /// the footer.
    fn save(&mut self) {
        let state = State {
            workspaces: self
                .workspaces
                .iter()
                .map(|ws| SavedWorkspace {
                    path: ws.path.clone(),
                    name: ws.custom_name.clone(),
                    pinned: ws
                        .tabs
                        .iter()
                        .filter(|tab| tab.pinned)
                        .map(|tab| SavedTab {
                            cwd: tab.cwd().map_or_else(|| ws.path.clone(), Path::to_path_buf),
                            title: tab.custom_prefix.clone(),
                        })
                        .collect(),
                })
                .collect(),
        };
        if let Err(err) = store::save(&state) {
            self.set_status(format!("couldn't save workspaces: {err:#}"));
        }
    }

    /// Recomputes the layout and keeps every pty sized to the pane.
    fn relayout(&mut self, size: Size) {
        let area = Rect::new(0, 0, size.width, size.height);
        self.layout = Layout::new(area, self.activity_width, self.sidebar_width, self.collapsed);
        self.activity_scroll = self.layout.clamp_activity_scroll(self.activity_scroll, self.workspaces.len(), None);
        let (rows, cols) = self.pty_size();
        for ws in &mut self.workspaces {
            ws.clamp_scroll(&self.layout, None);
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
    fn sync_cursor_style(&mut self) -> io::Result<()> {
        let style = self.active_tab().map_or(0, Tab::cursor_style);
        let Some(client) = &mut self.client else { return Ok(()) };
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
        crossterm::execute!(client.terminal.backend_mut(), style)
    }

    fn pty_size(&self) -> (u16, u16) {
        (self.layout.pane.height.max(1), self.layout.pane.width.max(1))
    }

    fn set_collapsed(&mut self, collapsed: bool) {
        self.collapsed = collapsed;
        self.relayout_now();
    }

    /// Badges only → badges and names → hidden → badges only.
    fn cycle_activity_bar(&mut self) {
        self.activity_width = match self.activity_width {
            0 => layout::ACTIVITY_COMPACT,
            width if width < layout::ACTIVITY_WIDE_MIN => self.wide_activity_width,
            _ => 0,
        };
        self.relayout_now();
    }

    /// Sets the workspace bar's width while its edge is dragged; too narrow
    /// for names snaps back to badges only.
    fn resize_activity_bar(&mut self, width: u16) {
        self.activity_width = if width < layout::ACTIVITY_WIDE_MIN {
            layout::ACTIVITY_COMPACT
        } else {
            width.min(layout::ACTIVITY_MAX)
        };
        self.relayout_now();
        if self.layout.activity_wide() {
            self.wide_activity_width = self.layout.activity.width;
        }
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
        self.save();
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

    /// Opens a tab at the end of the current workspace.
    fn new_tab(&mut self) -> Result<()> {
        self.open_tab(None)
    }

    /// Opens a tab at the end of the active tab's group, or of the
    /// workspace when it isn't in one.
    fn new_tab_in_group(&mut self) -> Result<()> {
        let group = self.active_tab().and_then(|t| t.group);
        self.open_tab(group)
    }

    /// Opens a tab in the active tab's folder if that's inside the
    /// workspace, else in the workspace root.
    fn open_tab(&mut self, group: Option<GroupId>) -> Result<()> {
        let ws = self.ws();
        let cwd = ws
            .active_tab()
            .and_then(Tab::cwd)
            .filter(|cwd| cwd.starts_with(&ws.path))
            .map_or_else(|| ws.path.clone(), Path::to_path_buf);
        let mut tab = self.spawn_tab(&cwd)?;
        tab.group = group;
        let ws = self.ws_mut();
        let index = group.map_or(ws.tabs.len(), |id| ws.group_range(id).end);
        ws.tabs.insert(index, tab);
        self.activate(index);
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
        let (id, group) = (tab.id, tab.group);
        if let Some(group) = group.and_then(|g| ws.group_mut(g)) {
            group.last_active = Some(id);
        }
        ws.clamp_scroll(&self.layout, Some(index));
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
        let was_pinned = ws.tabs.remove(index).pinned;
        ws.prune_groups();
        if index < ws.active || ws.active >= ws.tabs.len() {
            ws.active = ws.active.saturating_sub(1);
        }
        if was_pinned {
            self.save();
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

    /// Activates the tab `delta` steps away, wrapping around. A collapsed
    /// group counts as one step.
    fn cycle(&mut self, delta: isize) {
        let stops = self.ws().stops();
        let Some(position) = stops.iter().position(|&i| i == self.ws().active) else { return };
        let next = (position as isize + delta).rem_euclid(stops.len() as isize) as usize;
        self.activate(stops[next]);
    }

    /// Chrome numbering: 1-8 pick a tab, 9 is always the last one. A
    /// collapsed group counts as one tab.
    fn jump(&mut self, digit: char) {
        let stops = self.ws().stops();
        let stop = match digit {
            '9' => stops.last(),
            '1'..='8' => stops.get(digit as usize - '1' as usize),
            _ => None,
        };
        if let Some(&index) = stop {
            self.activate(index);
        }
    }

    fn activate_previous(&mut self) {
        let ws = self.ws();
        if let Some(index) = ws.tabs.iter().position(|t| Some(t.id) == ws.previous) {
            self.activate(index);
        }
    }

    /// Moves the active tab one step, as Chrome does: through its group,
    /// out of it at either end, into a group it meets, or past a collapsed
    /// one. Keyboard moves never cross into or out of the pinned section.
    fn shift_active(&mut self, delta: isize) {
        let ws = self.ws();
        let index = ws.active;
        let Some(tab) = ws.tabs.get(index) else { return };
        let pinned = ws.pinned_count();
        let down = delta > 0;
        let past = |range: std::ops::Range<usize>| if down { range.end - 1 } else { range.start };

        if tab.pinned {
            if let Some(to) = index.checked_add_signed(delta).filter(|&to| to < pinned) {
                self.place_tab(index, to, true, None);
            }
        } else if let Some(id) = tab.group {
            let range = ws.group_range(id);
            let collapsed = ws.group(id).is_some_and(|g| g.collapsed);
            let at_edge = if down { index + 1 == range.end } else { index == range.start };
            if collapsed || at_edge {
                self.place_tab(index, past(range), false, None);
            } else {
                self.place_tab(index, index.saturating_add_signed(delta), false, Some(id));
            }
        } else {
            let Some(neighbour) = index.checked_add_signed(delta).filter(|&i| i >= pinned && i < ws.tabs.len())
            else {
                return;
            };
            match ws.tabs[neighbour].group {
                Some(id) if ws.group(id).is_some_and(|g| g.collapsed) => {
                    self.place_tab(index, past(ws.group_range(id)), false, None);
                }
                // Join the group at the near end.
                Some(id) => self.place_tab(index, index, false, Some(id)),
                None => self.place_tab(index, neighbour, false, None),
            }
        }
    }

    /// Drops tab `from` where tab `to` is, taking on that tab's place: in
    /// the pinned section, in its group or in neither, like dragging tabs
    /// in Chrome.
    fn move_tab(&mut self, from: usize, to: usize) {
        let Some(target) = self.ws().tabs.get(to) else { return };
        let (pinned, group) = (target.pinned, target.group);
        self.place_tab(from, to, pinned, group);
    }

    /// Drops tab `from` on a group's header: from above it joins the group
    /// (or skips past it when collapsed); from inside or below it ends up
    /// above the group, outside it.
    fn drop_on_group(&mut self, from: usize, id: GroupId) {
        let ws = self.ws();
        let Some(group) = ws.group(id) else { return };
        let range = ws.group_range(id);
        if from < range.start {
            // Taking the tab out shifts the group up by one.
            if group.collapsed {
                self.place_tab(from, range.end - 1, false, None);
            } else {
                self.place_tab(from, range.start - 1, false, Some(id));
            }
        } else {
            self.place_tab(from, range.start, false, None);
        }
    }

    /// Takes tab `from` out and puts it back at index `to` (counted without
    /// it), pinned, in `group`, or neither.
    fn place_tab(&mut self, from: usize, to: usize, pinned: bool, group: Option<GroupId>) {
        let ws = &mut self.workspaces[self.current];
        let Some(tab) = ws.tabs.get(from) else { return };
        let group = if pinned { None } else { group };
        // Pinned tabs stay above the others.
        let pinned_rest = ws.pinned_count() - usize::from(tab.pinned);
        let last = ws.tabs.len() - 1;
        let to = if pinned { to.min(pinned_rest) } else { to.clamp(pinned_rest, last) };
        if to == from && tab.pinned == pinned && tab.group == group {
            return;
        }
        let affects_pins = tab.pinned || pinned;
        let active_id = ws.tabs[ws.active].id;
        let mut tab = ws.tabs.remove(from);
        tab.pinned = pinned;
        tab.group = group;
        ws.tabs.insert(to, tab);
        ws.active = ws.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
        ws.prune_groups();
        ws.clamp_scroll(&self.layout, Some(to));
        if affects_pins {
            self.save();
        }
    }

    /// Pins a tab (to the end of the pinned section, out of its group) or
    /// unpins it (to the top of the regular tabs).
    fn toggle_pin(&mut self, index: usize) {
        let ws = self.ws();
        let Some(tab) = ws.tabs.get(index) else { return };
        let pin = !tab.pinned;
        let message = format!("{} {}", if pin { "pinned" } else { "unpinned" }, tab.title());
        let to = ws.pinned_count() - usize::from(tab.pinned);
        self.place_tab(index, to, pin, None);
        self.set_status(message);
    }

    /// Closes a tab, except that pinned tabs need a second press.
    fn close_tab_guarded(&mut self, index: usize) {
        let Some(tab) = self.ws().tabs.get(index) else { return };
        if tab.pinned && self.confirm_close != Some(tab.id) {
            self.confirm_close = Some(tab.id);
            self.set_status("pinned tab · press x again to close");
            return;
        }
        self.confirm_close = None;
        self.close_tab(index);
    }

    // ── Tab groups ──────────────────────────────────────────────────────

    /// Starts a group holding the active tab, in the first colour no other
    /// group in the workspace has.
    fn new_group(&mut self, name: String) {
        let id = self.next_group_id;
        self.next_group_id += 1;
        let colors = theme::palette().group_colors().len();
        let ws = self.ws_mut();
        let color = (1..colors)
            .chain([0])
            .find(|c| !ws.groups.iter().any(|g| g.color == *c))
            .unwrap_or(ws.groups.len() % colors);
        ws.groups.push(Group { id, name, color, collapsed: false, last_active: None });
        let index = ws.active;
        let Some(tab) = ws.tabs.get(index) else { return };
        // A pinned tab moves to the top of the regular tabs; one from another
        // group moves out past the end of that group.
        let to = match (tab.pinned, tab.group) {
            (true, _) => ws.pinned_count() - 1,
            (false, Some(old)) => ws.group_range(old).end - 1,
            (false, None) => index,
        };
        self.place_tab(index, to, false, Some(id));
    }

    /// Moves the active tab to the end of group `id`.
    fn add_to_group(&mut self, id: GroupId) {
        let ws = self.ws();
        let index = ws.active;
        if ws.tabs.get(index).is_none_or(|t| t.group == Some(id)) {
            return;
        }
        let range = ws.group_range(id);
        let to = if index < range.start { range.end - 1 } else { range.end };
        self.place_tab(index, to, false, Some(id));
    }

    /// Takes the active tab out of its group, at the group's nearer end.
    fn remove_from_group(&mut self) {
        let ws = self.ws();
        let index = ws.active;
        let Some(id) = ws.tabs.get(index).and_then(|t| t.group) else {
            self.set_status("this tab isn't in a group");
            return;
        };
        let range = ws.group_range(id);
        let to = if index == range.start { index } else { range.end - 1 };
        self.place_tab(index, to, false, None);
    }

    /// Dissolves a group; its tabs stay where they are.
    fn ungroup(&mut self, id: GroupId) {
        let ws = &mut self.workspaces[self.current];
        for tab in ws.tabs.iter_mut().filter(|t| t.group == Some(id)) {
            tab.group = None;
        }
        ws.prune_groups();
        ws.clamp_scroll(&self.layout, None);
    }

    fn toggle_group(&mut self, id: GroupId) {
        let ws = &mut self.workspaces[self.current];
        if let Some(group) = ws.group_mut(id) {
            group.collapsed = !group.collapsed;
        }
        let active = ws.active;
        ws.clamp_scroll(&self.layout, Some(active));
    }

    fn cycle_group_color(&mut self, id: GroupId) {
        let colors = theme::palette().group_colors().len();
        if let Some(group) = self.ws_mut().group_mut(id) {
            group.color = (group.color + 1) % colors;
        }
    }

    /// The active tab's group, or a hint about how to make one.
    fn active_group(&mut self) -> Option<GroupId> {
        let group = self.active_tab().and_then(|t| t.group);
        if group.is_none() {
            let prefix = self.config.prefix.label();
            self.set_status(format!("not in a group · {prefix} m adds it to one"));
        }
        group
    }

    /// Rows of the group picker: existing groups matching the query, and a
    /// new group named after it (first when nothing is typed yet).
    pub fn group_entries(&self) -> Vec<GroupEntry> {
        let query = self.input.trim();
        let lower = query.to_lowercase();
        let groups = self.ws().groups_in_order();
        let matching = groups.iter().filter(|g| g.name.to_lowercase().contains(&lower)).map(|g| GroupEntry::Group(g.id));
        let new = GroupEntry::New(query.to_owned());
        if query.is_empty() {
            return std::iter::once(new).chain(matching).collect();
        }
        let mut entries: Vec<GroupEntry> = matching.collect();
        if !groups.iter().any(|g| g.name.to_lowercase() == lower) {
            entries.push(new);
        }
        entries
    }

    /// Indices (workspace, tab) of the tabs matching the search query.
    pub fn search_results(&self) -> Vec<(usize, usize)> {
        let query = self.input.to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        let mut results = Vec::new();
        for (w, ws) in self.workspaces.iter().enumerate() {
            for (t, tab) in ws.tabs.iter().enumerate() {
                let haystack = format!(
                    "{} {} {} {} {}",
                    tab.title(),
                    tab.process_name(),
                    tab.cwd().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                    ws.name,
                    tab.group.and_then(|id| ws.group(id)).map_or("", |g| g.name.as_str()),
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
            AppEvent::Attach(id, hello, stream) => self.attach(id, *hello, stream),
            // Input from a client that has since been replaced is dropped.
            AppEvent::Input(id, event) if self.client.as_ref().is_some_and(|c| c.id == id) => match event {
                Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
                Event::Mouse(mouse) => self.handle_mouse(mouse),
                Event::Paste(text) => self.paste(&text),
                Event::Resize(cols, rows) => self.resize_client(Size::new(cols, rows)),
                _ => {}
            },
            AppEvent::Input(..) => {}
            AppEvent::Gone(id) => {
                if self.client.as_ref().is_some_and(|c| c.id == id) {
                    self.client = None;
                    self.reset_modes();
                }
            }
            AppEvent::Kill => self.quit = true,
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
        let clipboard = tab.process(bytes).and_then(|data| clipboard::copy_base64(&data));
        // A shell starting up or redrawing after a resize isn't news.
        if !tab.settling() {
            tab.last_activity = Instant::now();
            if !on_screen {
                tab.unseen = true;
            }
        }
        if let Some(osc) = clipboard {
            self.send_to_terminal(osc.as_bytes());
        }
    }

    fn paste(&mut self, text: &str) {
        match self.mode {
            Mode::Rename | Mode::Search | Mode::Workspaces | Mode::Groups => {
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
