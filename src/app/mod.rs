mod keys;
mod mouse;
mod panels;

pub use panels::{Loadable, OpenPr, Panel, PortsPanel, PrList, PrPanel};

use std::collections::HashMap;
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

use crate::config::Config;
use crate::awake::Awake;
use crate::github::{FileDiff, Pr};
use crate::ports::Port;
use crate::layout::{self, Hit, Layout, ListItem, Row};
use crate::links::{self, Link};
use crate::protocol::{self, Hello};
use crate::server::{self, Attached, ClientId};
use crate::store::{self, SavedGroup, SavedLayout, SavedTab, SavedWorkspace, State};
use crate::tab::{Tab, TabId};
use crate::workspace::{self, Group, GroupId, Workspace, WorkspaceId};
use crate::{clipboard, notify, theme, ui};

/// Minimum time between two frames (~120 fps).
const FRAME: Duration = Duration::from_millis(8);
/// How often tab titles (foreground process, cwd) are refreshed.
const INFO_REFRESH: Duration = Duration::from_millis(500);
/// A background tab counts as busy (spinner) this long after its last output.
const BUSY: Duration = Duration::from_millis(800);
const SPINNER_STEP: Duration = Duration::from_millis(80);
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);
/// Pressing the prefix twice this quickly sends it to the program.
const PREFIX_TWICE: Duration = Duration::from_millis(400);
/// After a tab-mode key, tab mode stays this long for the next one (j j k…),
/// then ends by itself.
const TAB_MODE_REPEAT: Duration = Duration::from_secs(2);
/// A click on a link opens it once this passes without a second click.
const LINK_DELAY: Duration = Duration::from_millis(400);
/// At most one notification per tab this often.
const NOTIFY_GAP: Duration = Duration::from_secs(10);

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
    /// Background work for the panels finishing.
    Prs(PathBuf, Result<Vec<Pr>, String>),
    PrFiles(u64, Result<Vec<FileDiff>, String>),
    Ports(Result<Vec<Port>, String>),
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
    /// A panel over the pane: pull requests or ports (`App::panel`).
    Panel,
    Help,
    ConfirmQuit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Tab(TabId),
    Divider,
    ActivityDivider,
    PortsDivider,
    PrsDivider,
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
    /// Set while sasha keeps the computer from sleeping.
    pub(crate) awake: Option<Awake>,
    /// The panel shown in `Mode::Panel`.
    pub(crate) panel: Option<Panel>,
    /// Show the ports and pull request bars (where there's room).
    side_bars: bool,
    ports_width: u16,
    prs_width: u16,
    /// The ports programs in the tabs listen on, as last checked.
    pub(crate) ports: Loadable<Vec<Port>>,
    ports_checking: bool,
    ports_checked: Option<Instant>,
    /// Open pull requests by repository folder, as last fetched.
    pub(crate) prs: HashMap<PathBuf, PrList>,
    /// Whether the client's window has focus (when its terminal says).
    focused: bool,
    /// This server's `protocol::build_id`.
    build: u64,
    /// Whether a client has attached before (the first one opens a tab).
    attached_before: bool,
    /// The state file as last written.
    saved: String,
    pub(crate) mode: Mode,
    pub(crate) workspaces: Vec<Workspace>,
    /// Tab groups, in the order they're listed; their tabs can come from
    /// any workspace.
    pub(crate) groups: Vec<Group>,
    /// Index of the workspace on screen.
    pub(crate) current: usize,
    previous_workspace: Option<WorkspaceId>,
    pub(crate) layout: Layout,
    pub(crate) hover: Option<Hit>,
    pub(crate) selection: Option<Selection>,
    /// The link under the mouse in a tab, underlined.
    pub(crate) hover_link: Option<(TabId, Link)>,
    /// A clicked link, opened once it's clear the click wasn't the start of
    /// a double-click.
    pending_link: Option<(String, Instant)>,
    /// Text being typed in rename / search / workspace mode.
    pub(crate) input: String,
    /// Highlighted row in the search results or workspace picker.
    pub(crate) picker_selected: usize,
    /// First visible item of the activity bar.
    pub(crate) activity_scroll: usize,
    /// First visible row of the tab sidebar.
    pub(crate) list_scroll: usize,
    /// Show only the current workspace's tabs, rather than every
    /// workspace's stacked.
    current_only: bool,
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
    /// Workspace whose tabs wait for a second click on its heading's ×.
    confirm_close_section: Option<WorkspaceId>,
    /// When the prefix was last pressed.
    prefix_at: Instant,
    /// When tab mode ends by itself; `None` while it waits for its first
    /// key.
    tab_mode_until: Option<Instant>,
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
            awake: None,
            panel: None,
            side_bars: true,
            ports_width: layout::PORTS_WIDTH,
            prs_width: layout::PRS_WIDTH,
            ports: Loadable::Loading,
            ports_checking: false,
            ports_checked: None,
            prs: HashMap::new(),
            focused: true,
            build: protocol::build_id(),
            attached_before: false,
            saved: String::new(),
            mode: Mode::Normal,
            workspaces: Vec::new(),
            groups: Vec::new(),
            current: 0,
            previous_workspace: None,
            layout: Layout::default(),
            hover: None,
            selection: None,
            hover_link: None,
            pending_link: None,
            input: String::new(),
            picker_selected: 0,
            activity_scroll: 0,
            list_scroll: 0,
            current_only: false,
            last_click: None,
            last_tab_click: None,
            last_group_click: None,
            last_workspace_click: None,
            last_name_click: None,
            renaming: Renaming::Tab,
            status: None,
            confirm_remove: None,
            confirm_close: None,
            confirm_close_section: None,
            prefix_at: Instant::now(),
            tab_mode_until: None,
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
        app.restore(state);
        if let Some(warning) = warning {
            app.set_status(warning);
        }
        app
    }

    /// Brings back the saved workspaces, groups and layout. Pinned and
    /// grouped tabs reopen right away; a workspace's other tabs wait until
    /// it's first opened (see `wake`).
    fn restore(&mut self, state: State) {
        if let Some(layout) = state.layout {
            self.sidebar_width = layout.sidebar_width.max(layout::MIN_WIDTH);
            self.collapsed = layout.collapsed;
            self.activity_width = layout.activity_width.min(layout::ACTIVITY_MAX);
            self.wide_activity_width = layout.wide_activity_width.clamp(layout::ACTIVITY_WIDE_MIN, layout::ACTIVITY_MAX);
            self.current_only = layout.current_only;
            self.side_bars = !layout.hide_side_bars;
            if layout.ports_width > 0 {
                self.ports_width = layout.ports_width.clamp(layout::PORTS_MIN, layout::PORTS_MAX);
            }
            if layout.prs_width > 0 {
                self.prs_width = layout.prs_width.clamp(layout::PRS_MIN, layout::PRS_MAX);
            }
            self.relayout_now();
        }
        let mut group_ids = Vec::new();
        for group in state.groups {
            let id = self.next_group_id;
            self.next_group_id += 1;
            group_ids.push(id);
            let (name, color, collapsed) = (group.name, group.color, group.collapsed);
            self.groups.push(Group { id, name, color, collapsed, last_active: None });
        }
        for mut saved in state.workspaces {
            let index = self.add_workspace(saved.path.clone());
            self.workspaces[index].rename(saved.name.take());
            let mut later = Vec::new();
            let mut active = None;
            for (i, tab) in saved.take_tabs().into_iter().enumerate() {
                let group = tab.group.and_then(|g| group_ids.get(g).copied());
                if tab.pinned || group.is_some() {
                    let id = self.reopen(index, tab, group);
                    if i == saved.active {
                        active = id;
                    }
                } else {
                    if i == saved.active {
                        saved.active = later.len();
                    }
                    later.push(tab);
                }
            }
            let ws = &mut self.workspaces[index];
            // Pinned tabs first, whatever the file says.
            ws.tabs.sort_by_key(|tab| !tab.pinned);
            ws.active = ws.tabs.iter().position(|tab| Some(tab.id) == active).unwrap_or(0);
            if !later.is_empty() {
                if active.is_some() {
                    saved.active = usize::MAX;
                }
                saved.tabs = later;
                ws.dormant = Some(saved);
            }
        }
        self.prune_groups();
        self.current = state.current.min(self.workspaces.len().saturating_sub(1));
        // Keeping the computer awake survives restarts (and updates).
        if state.keep_awake {
            self.toggle_awake();
        }
    }

    /// Reopens a workspace's remaining saved tabs, the first time it's
    /// opened.
    fn wake(&mut self, index: usize) {
        let Some(saved) = self.workspaces.get_mut(index).and_then(|ws| ws.dormant.take()) else { return };
        let mut active = None;
        for (i, tab) in saved.tabs.into_iter().enumerate() {
            let id = self.reopen(index, tab, None);
            if i == saved.active {
                active = id;
            }
        }
        let ws = &mut self.workspaces[index];
        if let Some(position) = ws.tabs.iter().position(|tab| Some(tab.id) == active) {
            ws.active = position;
        }
    }

    /// Reopens a saved tab as a fresh shell in the folder it was in (or the
    /// workspace's, if that's gone), at the end of workspace `index`.
    fn reopen(&mut self, index: usize, saved: SavedTab, group: Option<GroupId>) -> Option<TabId> {
        let cwd = if saved.cwd.is_dir() { saved.cwd } else { self.workspaces[index].path.clone() };
        match self.spawn_tab(&cwd) {
            Ok(mut tab) => {
                tab.pinned = saved.pinned;
                tab.custom_prefix = saved.title;
                tab.group = group.filter(|_| !tab.pinned);
                let id = tab.id;
                self.workspaces[index].tabs.push(tab);
                Some(id)
            }
            Err(err) => {
                self.set_status(format!("can't reopen a tab: {err}"));
                None
            }
        }
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
        self.focused = true;
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

    /// Shows the workspace for the folder sasha was started in, adding it if
    /// it's new; the very first time, with nothing to reopen, that means a
    /// tab right there. Started from the home folder, where new terminal
    /// windows open, it carries on where you left off instead.
    fn open_launch_folder(&mut self, launch: &Path) -> Result<()> {
        let saved = self.workspaces.iter().any(|ws| !ws.tabs.is_empty() || ws.dormant.is_some());
        let fresh = !self.attached_before && !saved;
        self.attached_before = true;
        let at_home = std::env::home_dir().is_some_and(|home| home == launch);
        if at_home && !fresh {
            let current = self.current;
            self.switch_workspace(current);
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
        if fresh {
            self.current = index;
            let tab = self.spawn_tab(launch)?;
            self.ws_mut().tabs.push(tab);
            let last = self.ws().tabs.len() - 1;
            self.activate(last);
        } else {
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
        self.panel = None;
        self.hover = None;
        self.drag = None;
        self.selection = None;
        self.hover_link = None;
        self.pending_link = None;
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
    pub(crate) fn find_tab(&self, id: TabId) -> Option<(usize, usize)> {
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

    /// Whether a bar's edge is hovered or being dragged, to light it up.
    pub fn edge_hot(&self, edge: Hit) -> bool {
        let dragging = match edge {
            Hit::PortsDivider => self.drag == Some(Drag::PortsDivider),
            Hit::PrsDivider => self.drag == Some(Drag::PrsDivider),
            _ => false,
        };
        dragging || self.hover == Some(edge)
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
            Mode::Panel => match self.panel {
                Some(Panel::Ports(_)) => (Some((if short { "PRT" } else { "PORTS" }, p.cyan)), "esc closes".to_owned()),
                _ => (Some((if short { "PR" } else { "PULLS" }, p.purple)), "esc closes".to_owned()),
            },
            Mode::ConfirmQuit => (Some((if short { "Q?" } else { "QUIT?" }, p.red)), "y / n".to_owned()),
        }
    }

    pub fn run(&mut self) -> Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        let mut last_refresh = Instant::now();
        let mut was_animating = false;
        while !self.quit {
            // Logging out or shutting down: keep the tabs for next time.
            if server::stop_requested() {
                break;
            }
            let mut timeout = if dirty {
                FRAME.saturating_sub(last_draw.elapsed())
            } else if was_animating {
                SPINNER_STEP
            } else {
                INFO_REFRESH
            };
            if let Some((_, at)) = &self.pending_link {
                timeout = timeout.min(LINK_DELAY.saturating_sub(at.elapsed()));
            }
            if let Some(until) = self.tab_mode_until {
                timeout = timeout.min(until.saturating_duration_since(Instant::now()));
            }
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

            // Tab mode ends a moment after its last key.
            if self.mode == Mode::Tab && self.tab_mode_until.is_some_and(|until| Instant::now() >= until) {
                self.mode = Mode::Normal;
                self.tab_mode_until = None;
                dirty = true;
            }
            if self.pending_link.as_ref().is_some_and(|(_, at)| at.elapsed() >= LINK_DELAY)
                && let Some((url, _)) = self.pending_link.take()
            {
                links::open(&url);
                self.set_status("opening link");
                dirty = true;
            }
            if last_refresh.elapsed() >= INFO_REFRESH {
                last_refresh = Instant::now();
                self.refresh_tabs();
                self.refresh_bars();
                self.check_awake();
                self.notify();
                self.save();
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
        self.save();
        Ok(())
    }

    /// Tells the client sasha has stopped (once nothing can attach anymore).
    pub fn dismiss_client(&mut self) {
        if let Some(client) = self.client.take() {
            client.exit("[sasha stopped · run sasha to reopen your tabs]");
        }
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
        let groups = &self.groups;
        let state = State {
            workspaces: self.workspaces.iter().map(|ws| save_workspace(ws, groups)).collect(),
            groups: groups
                .iter()
                .map(|g| SavedGroup { name: g.name.clone(), color: g.color, collapsed: g.collapsed })
                .collect(),
            current: self.current,
            keep_awake: self.awake.is_some(),
            layout: Some(SavedLayout {
                sidebar_width: self.sidebar_width,
                collapsed: self.collapsed,
                activity_width: self.activity_width,
                wide_activity_width: self.wide_activity_width,
                current_only: self.current_only,
                hide_side_bars: !self.side_bars,
                ports_width: self.ports_width,
                prs_width: self.prs_width,
            }),
        };
        let result = store::encode(&state).and_then(|json| {
            // Only touch the disk when something changed.
            if json != self.saved {
                store::write(&json)?;
                self.saved = json;
            }
            Ok(())
        });
        if let Err(err) = result {
            self.set_status(format!("couldn't save workspaces: {err:#}"));
        }
    }

    /// Recomputes the layout and keeps every pty sized to the pane.
    fn relayout(&mut self, size: Size) {
        let area = Rect::new(0, 0, size.width, size.height);
        let bars = if self.side_bars { (self.ports_width, self.prs_width) } else { (0, 0) };
        self.layout = Layout::new(area, self.activity_width, self.sidebar_width, self.collapsed, bars);
        self.clamp_bar_scroll(false);
        self.clamp_list_scroll(false);
        let (rows, cols) = self.pty_size();
        for ws in &mut self.workspaces {
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
        // Going to a workspace shows its section again.
        self.workspaces[index].section_hidden = false;
        self.clamp_bar_scroll(true);
        self.wake(index);
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

    /// Opens a tab in the current workspace.
    fn new_tab(&mut self) -> Result<()> {
        self.open_tab(None)
    }

    /// Opens a tab in the current workspace, in the active tab's group if
    /// it's in one.
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
        ws.section_hidden = false;
        ws.tabs.push(tab);
        let last = ws.tabs.len() - 1;
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
        let (id, group) = (tab.id, tab.group);
        if let Some(group) = group.and_then(|g| self.group_mut(g)) {
            group.last_active = Some(id);
        }
        self.selection = None;
        self.hover_link = None;
        self.clamp_list_scroll(true);
    }

    /// Shows tab `t` of workspace `w`.
    fn go_to(&mut self, w: usize, t: usize) {
        if w != self.current {
            self.switch_workspace(w);
        }
        self.activate(t);
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
        if index < ws.active || ws.active >= ws.tabs.len() {
            ws.active = ws.active.saturating_sub(1);
        }
        self.prune_groups();
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

    /// Rows of the tab sidebar: the groups first, each with its tabs from
    /// any workspace, then each workspace's other tabs, under a heading
    /// when there's more than one such workspace. Only the current
    /// workspace's section has a "+", and other workspaces show up only
    /// with tabs outside groups. `current_only` leaves out every other
    /// workspace's tabs.
    pub fn sidebar_rows(&self) -> Vec<Row> {
        let Some(current) = self.workspaces.get(self.current) else { return Vec::new() };
        let on_screen = (self.current, current.active);
        let mut rows = Vec::new();
        for group in &self.groups {
            let members = self.shown_members(group.id);
            if members.is_empty() {
                continue;
            }
            rows.push(Row::Group(group.id));
            let visible = members.into_iter().filter(|&tab| !group.collapsed || tab == on_screen);
            rows.extend(visible.map(|(w, t)| Row::Item(w, ListItem::Tab(t))));
        }
        // The current workspace (unless its section was closed), and others
        // with tabs of their own, outside groups.
        let sections: Vec<usize> = (0..self.workspaces.len())
            .filter(|&w| {
                let ws = &self.workspaces[w];
                let own = !ws.section_hidden && ws.has_loose_tabs();
                if w == self.current { !ws.section_hidden } else { !self.current_only && own }
            })
            .collect();
        // With groups above, a heading keeps the tabs outside them from
        // looking like part of the last group.
        let headings = sections.len() > 1 || !rows.is_empty();
        for &w in &sections {
            if !rows.is_empty() {
                rows.push(Row::Gap);
            }
            if headings {
                rows.push(Row::Workspace(w));
            }
            let items = self.workspaces[w].list_items().into_iter();
            rows.extend(items.filter(|item| w == self.current || *item != ListItem::NewTab).map(|item| Row::Item(w, item)));
        }
        rows
    }

    /// Takes a workspace's section out of the stacked sidebar, its tabs
    /// still running; going to the workspace (its badge) brings it back.
    fn minimize_section(&mut self, w: usize) {
        let Some(ws) = self.workspaces.get_mut(w) else { return };
        ws.section_hidden = true;
        let name = ws.name.clone();
        self.clamp_list_scroll(false);
        self.set_status(format!("minimized {name} · click its badge to bring it back"));
    }

    /// Closes a workspace's section of the stacked sidebar, and the tabs
    /// listed in it (those outside groups), asking for a second click when
    /// that's more than one tab. Going to the workspace shows it again.
    fn close_section(&mut self, w: usize) {
        let Some(ws) = self.workspaces.get(w) else { return };
        let (id, name) = (ws.id, ws.name.clone());
        let tabs: Vec<TabId> = ws.tabs.iter().filter(|t| t.group.is_none()).map(|t| t.id).collect();
        if tabs.len() > 1 && self.confirm_close_section != Some(id) {
            self.confirm_close_section = Some(id);
            self.set_status(format!("click × again to close {} tabs in {name}", tabs.len()));
            return;
        }
        self.confirm_close_section = None;
        if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == id) {
            ws.section_hidden = true;
        }
        for tab in tabs {
            if let Some((w, t)) = self.find_tab(tab) {
                self.remove_tab(w, t);
            }
        }
        self.clamp_list_scroll(false);
        self.set_status(format!("closed {name} · click its badge to bring it back"));
    }

    /// Where keyboard navigation stops, top to bottom: every tab in the
    /// sidebar, and one standing in for each collapsed group.
    fn stops(&self) -> Vec<(usize, usize)> {
        let on_screen = (self.current, self.ws().active);
        let mut stops = Vec::new();
        for row in self.sidebar_rows() {
            match row {
                Row::Item(w, ListItem::Tab(t)) => stops.push((w, t)),
                Row::Group(id) => {
                    let Some(group) = self.group(id).filter(|g| g.collapsed) else { continue };
                    let members = self.shown_members(id);
                    if members.contains(&on_screen) {
                        continue;
                    }
                    let last = members.iter().find(|&&(w, t)| Some(self.workspaces[w].tabs[t].id) == group.last_active);
                    stops.extend(last.or(members.first()));
                }
                _ => {}
            }
        }
        stops
    }

    /// Keeps the tab sidebar scrolled into range; with `reveal`, it shows
    /// the tab on screen.
    fn clamp_list_scroll(&mut self, reveal: bool) {
        let Some(ws) = self.workspaces.get(self.current) else { return };
        let rows = self.sidebar_rows();
        let target = Row::Item(self.current, ListItem::Tab(ws.active));
        let row = reveal.then(|| rows.iter().position(|r| *r == target)).flatten();
        self.list_scroll = self.layout.clamp_scroll(self.list_scroll, rows.len(), row);
    }

    /// Keeps the workspace bar scrolled into range, showing the current
    /// workspace with `reveal`.
    fn clamp_bar_scroll(&mut self, reveal: bool) {
        let reveal = reveal.then_some(self.current);
        self.activity_scroll = self.layout.clamp_activity_scroll(self.activity_scroll, self.workspaces.len(), reveal);
    }

    /// Makes sure keeping the computer awake still holds while it's on.
    fn check_awake(&mut self) {
        if let Some(awake) = &mut self.awake
            && let Err(err) = awake.check()
        {
            self.awake = None;
            self.set_status(format!("stopped keeping the computer awake: {err:#}"));
        }
    }

    /// Keeps the computer from sleeping, or lets it sleep again.
    fn toggle_awake(&mut self) {
        if self.awake.take().is_some() {
            self.set_status("the computer can sleep again");
            return;
        }
        match Awake::start() {
            Ok(awake) => {
                self.awake = Some(awake);
                self.set_status("keeping the computer awake");
            }
            Err(err) => self.set_status(format!("can't keep the computer awake: {err:#}")),
        }
    }

    /// Shows or hides the ports and pull request bars.
    fn toggle_side_bars(&mut self) {
        self.side_bars = !self.side_bars;
        self.hover = None;
        self.relayout_now();
        if self.side_bars && self.layout.ports_bar.width == 0 {
            self.set_status("the window is too narrow for the ports and pull request bars");
        }
    }

    /// Every workspace's tabs, or only the current workspace's.
    fn toggle_stacked(&mut self) {
        self.current_only = !self.current_only;
        self.clamp_list_scroll(true);
        self.set_status(if self.current_only { "showing this workspace's tabs" } else { "showing every workspace's tabs" });
    }

    /// Activates the tab `delta` steps away in the sidebar, wrapping
    /// around, across groups and workspaces. A collapsed group counts as one
    /// step.
    fn cycle(&mut self, delta: isize) {
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let here = (self.current, self.ws().active);
        // The tab on screen may not be listed (its section is minimized):
        // start from the top, or the bottom going up.
        let next = match stops.iter().position(|&stop| stop == here) {
            Some(position) => (position as isize + delta).rem_euclid(stops.len() as isize) as usize,
            None if delta > 0 => 0,
            None => stops.len() - 1,
        };
        let (w, t) = stops[next];
        self.go_to(w, t);
    }

    /// Chrome numbering for the current workspace's tabs: 1-8 pick a tab,
    /// 9 is always the last one. A collapsed group counts as one tab.
    fn jump(&mut self, digit: char) {
        let stops: Vec<usize> = self.stops().into_iter().filter(|&(w, _)| w == self.current).map(|(_, t)| t).collect();
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

    /// Moves the active tab one place up or down its list: the pinned tabs,
    /// its group's tabs from the same workspace, or the workspace's tabs
    /// outside groups.
    fn shift_active(&mut self, delta: isize) {
        let ws = self.ws();
        let index = ws.active;
        let Some(tab) = ws.tabs.get(index) else { return };
        let same_list = |other: &Tab| other.pinned == tab.pinned && other.group == tab.group;
        let neighbour = if delta > 0 {
            (index + 1..ws.tabs.len()).find(|&i| same_list(&ws.tabs[i]))
        } else {
            (0..index).rev().find(|&i| same_list(&ws.tabs[i]))
        };
        if let Some(to) = neighbour {
            self.move_within(index, to);
        }
    }

    /// Moves tab `from` of the current workspace to where tab `to` is.
    fn move_within(&mut self, from: usize, to: usize) {
        let ws = &mut self.workspaces[self.current];
        if from == to || from >= ws.tabs.len() || to >= ws.tabs.len() {
            return;
        }
        let active_id = ws.tabs[ws.active].id;
        let tab = ws.tabs.remove(from);
        let pinned = tab.pinned;
        ws.tabs.insert(to, tab);
        ws.active = ws.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
        self.clamp_list_scroll(true);
        if pinned {
            self.save();
        }
    }

    /// Drops tab `from` of the current workspace onto tab `to`, taking on
    /// its place: pinned, in its group or in neither, like dragging tabs in
    /// Chrome.
    fn move_tab(&mut self, from: usize, to: usize) {
        let ws = self.ws();
        let (Some(tab), Some(target)) = (ws.tabs.get(from), ws.tabs.get(to)) else { return };
        if from == to {
            return;
        }
        let (id, target_id, pinned, group) = (tab.id, target.id, target.pinned, target.group);
        self.set_membership(self.current, from, pinned, group);
        let ws = self.ws();
        let position = |id| ws.tabs.iter().position(|t| t.id == id);
        if let (Some(from), Some(to)) = (position(id), position(target_id)) {
            self.move_within(from, to);
        }
    }

    /// Pins tab `t` of workspace `w`, puts it in `group`, or neither.
    /// Pinning takes it out of its group to the end of the pinned tabs;
    /// unpinning puts it first among the others.
    fn set_membership(&mut self, w: usize, t: usize, pinned: bool, group: Option<GroupId>) {
        let Some(ws) = self.workspaces.get_mut(w) else { return };
        let Some(tab) = ws.tabs.get_mut(t) else { return };
        let group = if pinned { None } else { group };
        let pin_changed = tab.pinned != pinned;
        tab.group = group;
        if pin_changed {
            let active_id = ws.tabs.get(ws.active).map(|tab| tab.id);
            let mut tab = ws.tabs.remove(t);
            tab.pinned = pinned;
            let at = ws.pinned_count();
            ws.tabs.insert(at, tab);
            ws.active = ws.tabs.iter().position(|tab| Some(tab.id) == active_id).unwrap_or(0);
        }
        self.prune_groups();
        self.clamp_list_scroll(true);
        if pin_changed {
            self.save();
        }
    }

    /// Pins a tab, taking it out of its group, or unpins it, back into the
    /// group it was in.
    fn toggle_pin(&mut self, index: usize) {
        let Some(tab) = self.ws().tabs.get(index) else { return };
        let pin = !tab.pinned;
        let message = format!("{} {}", if pin { "pinned" } else { "unpinned" }, tab.title());
        let (group, before) = (tab.group, tab.group_before_pin);
        let back = before.filter(|&id| !pin && self.group(id).is_some());
        if let Some(tab) = self.ws_mut().tabs.get_mut(index) {
            tab.group_before_pin = if pin { group } else { None };
        }
        self.set_membership(self.current, index, pin, back);
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

    /// Posts a notification for each tab with news you aren't looking at:
    /// any tab but the one on screen, or every tab while the window is in the
    /// background or no window is open.
    fn notify(&mut self) {
        let watching = self.client.is_some() && self.focused;
        let on_screen = (self.current, self.workspaces.get(self.current).map_or(0, |ws| ws.active));
        for (w, ws) in self.workspaces.iter_mut().enumerate() {
            for (t, tab) in ws.tabs.iter_mut().enumerate() {
                let Some(news) = tab.take_news() else { continue };
                let seen = watching && (w, t) == on_screen;
                if seen || tab.last_notified.is_some_and(|at| at.elapsed() < NOTIFY_GAP) {
                    continue;
                }
                tab.last_notified = Some(Instant::now());
                notify::send(&tab.title(), &news);
            }
        }
    }

    // ── Tab groups ──────────────────────────────────────────────────────

    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    fn group_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// The tabs (workspace, tab) in group `id`, by workspace and then in
    /// each workspace's order.
    pub fn members(&self, id: GroupId) -> Vec<(usize, usize)> {
        let mut members = Vec::new();
        for (w, ws) in self.workspaces.iter().enumerate() {
            for (t, tab) in ws.tabs.iter().enumerate() {
                if tab.group == Some(id) {
                    members.push((w, t));
                }
            }
        }
        members
    }

    /// `members`, minus other workspaces' with `current_only`.
    fn shown_members(&self, id: GroupId) -> Vec<(usize, usize)> {
        let mut members = self.members(id);
        if self.current_only {
            members.retain(|&(w, _)| w == self.current);
        }
        members
    }

    /// Forgets groups whose last tab has gone.
    fn prune_groups(&mut self) {
        let workspaces = &self.workspaces;
        self.groups.retain(|g| workspaces.iter().flat_map(|ws| &ws.tabs).any(|tab| tab.group == Some(g.id)));
    }

    /// Starts a group in the first colour no other group has. It takes the
    /// active tab, unless that tab is already in a group: then it starts with
    /// a new terminal instead, so the other group keeps its tabs.
    fn new_group(&mut self, name: String) {
        let id = self.next_group_id;
        self.next_group_id += 1;
        let colors = theme::palette().group_colors().len();
        let color = (1..colors)
            .chain([0])
            .find(|c| !self.groups.iter().any(|g| g.color == *c))
            .unwrap_or(self.groups.len() % colors);
        self.groups.push(Group { id, name, color, collapsed: false, last_active: None });
        if self.active_tab().is_some_and(|t| t.group.is_some()) {
            if let Err(err) = self.open_tab(Some(id)) {
                self.set_status(format!("can't start a shell: {err}"));
                self.prune_groups();
            }
            return;
        }
        let active = self.ws().active;
        self.set_membership(self.current, active, false, Some(id));
    }

    /// Puts the active tab in group `id`, whichever workspaces its other
    /// tabs are from.
    fn add_to_group(&mut self, id: GroupId) {
        let active = self.ws().active;
        self.set_membership(self.current, active, false, Some(id));
    }

    /// Takes the active tab out of its group, back among its workspace's.
    fn remove_from_group(&mut self) {
        if self.active_tab().and_then(|t| t.group).is_none() {
            self.set_status("this tab isn't in a group");
            return;
        }
        let active = self.ws().active;
        self.set_membership(self.current, active, false, None);
    }

    /// Dissolves a group; its tabs go back among their workspaces'.
    fn ungroup(&mut self, id: GroupId) {
        for tab in self.workspaces.iter_mut().flat_map(|ws| &mut ws.tabs) {
            if tab.group == Some(id) {
                tab.group = None;
            }
        }
        self.prune_groups();
        self.clamp_list_scroll(false);
    }

    fn toggle_group(&mut self, id: GroupId) {
        if let Some(group) = self.group_mut(id) {
            group.collapsed = !group.collapsed;
        }
        self.clamp_list_scroll(false);
    }

    fn cycle_group_color(&mut self, id: GroupId) {
        let colors = theme::palette().group_colors().len();
        if let Some(group) = self.group_mut(id) {
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

    /// Rows of the group picker: every group matching the query, and a new
    /// group named after it (first when nothing is typed yet).
    pub fn group_entries(&self) -> Vec<GroupEntry> {
        let query = self.input.trim();
        let lower = query.to_lowercase();
        let groups = &self.groups;
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
                    tab.group.and_then(|id| self.group(id)).map_or("", |g| g.name.as_str()),
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
                Event::FocusGained => self.focused = true,
                Event::FocusLost => self.focused = false,
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
            AppEvent::Prs(dir, prs) => self.prs_loaded(dir, prs),
            AppEvent::PrFiles(number, files) => self.pr_files_loaded(number, files),
            AppEvent::Ports(ports) => self.ports_loaded(ports),
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
            tab.note_output();
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

fn save_workspace(ws: &Workspace, groups: &[Group]) -> SavedWorkspace {
    // Not opened since the last start: its tabs stay as they were saved.
    if let Some(dormant) = &ws.dormant {
        return SavedWorkspace {
            path: ws.path.clone(),
            name: ws.custom_name.clone(),
            tabs: dormant.tabs.clone(),
            groups: Vec::new(),
            active: dormant.active,
            pinned: Vec::new(),
        };
    }
    let tabs = ws
        .tabs
        .iter()
        .map(|tab| SavedTab {
            cwd: tab.cwd().map_or_else(|| ws.path.clone(), Path::to_path_buf),
            title: tab.custom_prefix.clone(),
            pinned: tab.pinned,
            group: tab.group.and_then(|id| groups.iter().position(|g| g.id == id)),
        })
        .collect();
    SavedWorkspace {
        path: ws.path.clone(),
        name: ws.custom_name.clone(),
        tabs,
        groups: Vec::new(),
        active: ws.active,
        pinned: Vec::new(),
    }
}
