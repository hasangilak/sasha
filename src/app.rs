use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::DefaultTerminal;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;

use crate::clipboard;
use crate::config::Config;
use crate::input;
use crate::sidebar::{self, Hit, Layout};
use crate::tab::{Tab, TabId};
use crate::{theme, ui};

/// Minimum time between two frames (~120 fps).
const FRAME: Duration = Duration::from_millis(8);
/// How often tab titles (foreground process, cwd) are refreshed.
const INFO_REFRESH: Duration = Duration::from_millis(500);
/// A background tab counts as busy (spinner) this long after its last output.
const BUSY: Duration = Duration::from_millis(800);
const SPINNER_STEP: Duration = Duration::from_millis(80);
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const MULTI_CLICK: Duration = Duration::from_millis(400);
const WHEEL_LINES: usize = 3;

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

pub struct App {
    pub(crate) config: Config,
    pub(crate) mode: Mode,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    /// The tab that was active before the current one.
    previous: Option<TabId>,
    pub(crate) layout: Layout,
    /// First visible row of the tab list.
    pub(crate) scroll: usize,
    pub(crate) hover: Option<Hit>,
    pub(crate) selection: Option<Selection>,
    /// Time, position and count of the last click in the pane.
    last_click: Option<(Instant, (u16, u16), u8)>,
    status: Option<(String, Instant)>,
    sidebar_width: u16,
    collapsed: bool,
    drag: Option<Drag>,
    next_id: TabId,
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
            tabs: Vec::new(),
            active: 0,
            previous: None,
            layout: Layout::default(),
            scroll: 0,
            hover: None,
            selection: None,
            last_click: None,
            status: None,
            sidebar_width: sidebar::DEFAULT_WIDTH,
            collapsed: false,
            drag: None,
            next_id: 0,
            events_tx,
            events,
            started: Instant::now(),
            cursor_style: 0,
            quit: false,
        };
        app.relayout(size);
        app.new_tab()?;
        Ok(app)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// Whether a background tab is streaming output right now.
    pub fn is_busy(&self, index: usize) -> bool {
        index != self.active && self.tabs[index].last_activity.elapsed() < BUSY
    }

    fn animating(&self) -> bool {
        (0..self.tabs.len()).any(|i| self.is_busy(i))
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

    /// Badge and hint text for the bottom of the sidebar.
    pub fn footer(&self) -> (Option<(&'static str, Color)>, String) {
        let short = self.layout.collapsed;
        let prefix = self.config.prefix.label();
        match self.mode {
            Mode::Normal => match &self.status {
                Some((message, at)) if at.elapsed() < STATUS_TIMEOUT => (None, message.clone()),
                _ => (None, format!("{prefix} tab keys · {prefix} ? help")),
            },
            Mode::Scroll => (
                Some((if short { "SCR" } else { "SCROLL" }, theme::YELLOW)),
                "j/k ^u/^d g/G · q exit".to_owned(),
            ),
            Mode::Tab => (
                Some((if short { "TAB" } else { "TABS" }, theme::ACCENT)),
                "j/k move · t new · x close · ? help".to_owned(),
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
        while !self.quit && !self.tabs.is_empty() {
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

            if dirty && last_draw.elapsed() >= FRAME && !self.tabs.is_empty() {
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
        self.layout = Layout::new(area, self.sidebar_width, self.collapsed);
        self.scroll = self.layout.clamp_scroll(self.scroll, self.tabs.len(), None);
        let (rows, cols) = self.pty_size();
        for tab in &mut self.tabs {
            tab.resize(rows, cols);
        }
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
        let area = self.layout.sidebar.union(self.layout.pane);
        self.relayout(area.as_size());
    }

    fn new_tab(&mut self) -> Result<()> {
        let cwd = match self.active_tab() {
            Some(tab) => tab.cwd().map(ToOwned::to_owned),
            None => std::env::current_dir().ok(),
        };
        let id = self.next_id;
        self.next_id += 1;
        let tab = Tab::spawn(id, self.pty_size(), cwd.as_deref(), self.events_tx.clone())?;
        self.tabs.push(tab);
        self.activate(self.tabs.len() - 1);
        Ok(())
    }

    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.active
            && let Some(current) = self.tabs.get(self.active)
        {
            self.previous = Some(current.id);
        }
        self.active = index;
        self.selection = None;
        let tab = &mut self.tabs[index];
        tab.unseen = false;
        tab.bell = false;
        self.scroll = self.layout.clamp_scroll(self.scroll, self.tabs.len(), Some(index));
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        // Dropping the tab hangs up its shell.
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if !self.tabs.is_empty() {
            self.activate(self.active);
        }
    }

    /// Activates the tab `delta` steps away, wrapping around.
    fn cycle(&mut self, delta: isize) {
        let len = self.tabs.len() as isize;
        if len > 0 {
            self.activate((self.active as isize + delta).rem_euclid(len) as usize);
        }
    }

    /// Chrome numbering: 1-8 pick a tab, 9 is always the last one.
    fn jump(&mut self, digit: char) {
        match digit {
            '9' => self.activate(self.tabs.len().saturating_sub(1)),
            '1'..='8' => self.activate(digit as usize - '1' as usize),
            _ => {}
        }
    }

    fn activate_previous(&mut self) {
        if let Some(index) = self.tabs.iter().position(|t| Some(t.id) == self.previous) {
            self.activate(index);
        }
    }

    fn shift_active(&mut self, delta: isize) {
        let to = self.active as isize + delta;
        if (0..self.tabs.len() as isize).contains(&to) {
            self.move_tab(self.active, to as usize);
        }
    }

    fn resize_sidebar(&mut self, delta: i16) {
        self.collapsed = false;
        self.sidebar_width = self.sidebar_width.saturating_add_signed(delta).max(sidebar::MIN_WIDTH);
        let area = self.layout.sidebar.union(self.layout.pane);
        self.relayout(area.as_size());
        // Layout clamps to the screen; keep the stored width in sync with it.
        self.sidebar_width = self.layout.sidebar.width.max(sidebar::MIN_WIDTH);
    }

    fn request_quit(&mut self) {
        if self.tabs.len() > 1 {
            self.mode = Mode::ConfirmQuit;
        } else {
            self.quit = true;
        }
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
        self.scroll = self.layout.clamp_scroll(self.scroll, self.tabs.len(), Some(to));
    }

    fn refresh_tabs(&mut self) {
        let exited: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].has_exited()).collect();
        for index in exited.into_iter().rev() {
            self.close_tab(index);
        }
        for tab in &mut self.tabs {
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
                if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
                    self.close_tab(index);
                }
            }
        }
    }

    fn handle_output(&mut self, id: TabId, bytes: &[u8]) {
        let active_id = self.active_tab().map(|t| t.id);
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        if let Some(data) = tab.process(bytes) {
            clipboard::copy_base64(&data);
        }
        // Shells redraw their prompt after a resize; that isn't news.
        if !tab.recently_resized() {
            tab.last_activity = Instant::now();
            if Some(id) != active_id {
                tab.unseen = true;
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Tab => self.tab_mode_key(key),
            Mode::Scroll => self.scroll_mode_key(key),
            Mode::Help => self.mode = Mode::Normal,
            Mode::ConfirmQuit => {
                self.quit = matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter);
                self.mode = Mode::Normal;
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
        match key.code {
            KeyCode::Char('J') => self.shift_active(1),
            KeyCode::Char('K') => self.shift_active(-1),
            KeyCode::Down if shift => self.shift_active(1),
            KeyCode::Up if shift => self.shift_active(-1),
            KeyCode::Char('j' | 'n') | KeyCode::Down => self.cycle(1),
            KeyCode::Char('k' | 'p') | KeyCode::Up => self.cycle(-1),
            KeyCode::Char('g') | KeyCode::Home => self.activate(0),
            KeyCode::Char('G') | KeyCode::End => self.activate(self.tabs.len().saturating_sub(1)),
            KeyCode::Char(c @ '1'..='9') => {
                self.jump(c);
                self.mode = Mode::Normal;
            }
            KeyCode::Tab => {
                self.activate_previous();
                self.mode = Mode::Normal;
            }
            KeyCode::Char('t' | 'c') => {
                let _ = self.new_tab();
                self.mode = Mode::Normal;
            }
            KeyCode::Char('x' | 'w') => self.close_tab(self.active),
            KeyCode::Char('b' | 's') => self.set_collapsed(!self.collapsed),
            KeyCode::Char('h' | '<') | KeyCode::Left => self.resize_sidebar(-2),
            KeyCode::Char('l' | '>') | KeyCode::Right => self.resize_sidebar(2),
            KeyCode::Char('[' | 'v') | KeyCode::PageUp => self.mode = Mode::Scroll,
            KeyCode::Char('?') => self.mode = Mode::Help,
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
    fn scroll_active(&mut self, delta: isize) {
        self.selection = None;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let offset = tab.screen().scrollback().saturating_add_signed(delta);
            tab.screen_mut().set_scrollback(offset);
        }
    }

    fn send_key(&mut self, key: KeyEvent) {
        self.selection = None;
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        // Typing always jumps back to the live screen.
        tab.screen_mut().set_scrollback(0);
        let bytes = input::encode_key(&key, tab.screen().application_cursor());
        if !bytes.is_empty() {
            tab.write(bytes);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let hit = self.layout.hit(mouse.column, mouse.row, self.tabs.len(), self.scroll);
        self.hover = hit.filter(|h| *h != Hit::Pane);

        match (mouse.kind, self.drag) {
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::Divider)) => {
                let width = mouse.column.saturating_sub(self.layout.sidebar.x) + 1;
                self.sidebar_width = width.max(sidebar::MIN_WIDTH);
                if self.collapsed {
                    self.collapsed = false;
                }
                let area = self.layout.sidebar.union(self.layout.pane);
                self.relayout(area.as_size());
                return;
            }
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::Tab(from))) => {
                if let Some(Hit::Tab(to) | Hit::Close(to)) = hit {
                    self.move_tab(from, to);
                    self.drag = Some(Drag::Tab(to));
                }
                return;
            }
            (MouseEventKind::Drag(MouseButton::Left), Some(Drag::Select)) => {
                let pane = self.layout.pane;
                let col = mouse.column.clamp(pane.left(), pane.right().saturating_sub(1)) - pane.x;
                let row = mouse.row.clamp(pane.top(), pane.bottom().saturating_sub(1)) - pane.y;
                if let Some(selection) = &mut self.selection {
                    selection.head = (row, col);
                }
                return;
            }
            (MouseEventKind::Up(_), Some(Drag::Select)) => {
                self.drag = None;
                self.copy_selection();
                return;
            }
            (MouseEventKind::Up(_), Some(_)) => {
                self.drag = None;
                return;
            }
            _ => {}
        }

        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            match self.mode {
                Mode::Help | Mode::ConfirmQuit => {
                    self.mode = Mode::Normal;
                    return;
                }
                Mode::Tab if hit == Some(Hit::Pane) => self.mode = Mode::Normal,
                _ => {}
            }
        }

        match (mouse.kind, hit) {
            (_, Some(Hit::Pane)) => self.mouse_in_pane(mouse),
            (MouseEventKind::Down(MouseButton::Left), Some(hit)) => match hit {
                Hit::Toggle => self.set_collapsed(!self.collapsed),
                Hit::Search | Hit::Pane => {}
                Hit::Tab(index) => {
                    self.activate(index);
                    self.drag = Some(Drag::Tab(index));
                }
                Hit::Close(index) => self.close_tab(index),
                Hit::NewTab => {
                    let _ = self.new_tab();
                }
                Hit::Divider => self.drag = Some(Drag::Divider),
            },
            // Middle-click closes, like in a browser.
            (MouseEventKind::Down(MouseButton::Middle), Some(Hit::Tab(index) | Hit::Close(index))) => {
                self.close_tab(index);
            }
            (MouseEventKind::ScrollDown, Some(_)) => {
                self.scroll = self.layout.clamp_scroll(self.scroll + 1, self.tabs.len(), None);
            }
            (MouseEventKind::ScrollUp, Some(_)) => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn mouse_in_pane(&mut self, mouse: MouseEvent) {
        let pane = self.layout.pane;
        let (col, row) = (mouse.column - pane.x, mouse.row - pane.y);
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let screen = tab.screen();
        let mode = screen.mouse_protocol_mode();
        // Shift bypasses the program's mouse handling, as in xterm.
        if mode != vt100::MouseProtocolMode::None
            && !mouse.modifiers.contains(KeyModifiers::SHIFT)
            && screen.scrollback() == 0
        {
            let encoding = screen.mouse_protocol_encoding();
            if let Some(bytes) = input::encode_mouse(&mouse, col, row, mode, encoding) {
                tab.write(bytes);
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if screen.alternate_screen() {
                    // Full-screen apps without mouse support (less, man) get arrow keys.
                    let key = KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down });
                    let bytes = input::encode_key(&key, screen.application_cursor()).repeat(WHEEL_LINES);
                    tab.write(bytes);
                } else {
                    let lines = WHEEL_LINES as isize;
                    self.scroll_active(if up { lines } else { -lines });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let tab_id = tab.id;
                let clicks = match self.last_click {
                    Some((at, pos, n)) if pos == (row, col) && at.elapsed() < MULTI_CLICK => n % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((Instant::now(), (row, col), clicks));
                let (anchor, head) = match clicks {
                    2 => word_bounds(self.tabs[self.active].screen(), row, col),
                    3 => ((row, 0), (row, pane.width.saturating_sub(1))),
                    _ => ((row, col), (row, col)),
                };
                self.selection = Some(Selection { tab: tab_id, anchor, head });
                self.drag = Some(Drag::Select);
            }
            _ => {}
        }
    }

    fn copy_selection(&mut self) {
        let Some(selection) = self.selection else { return };
        if selection.anchor == selection.head {
            self.selection = None;
            return;
        }
        let Some(tab) = self.tabs.iter().find(|t| t.id == selection.tab) else { return };
        let ((start_row, start_col), (end_row, end_col)) = selection.ordered();
        let text = tab.screen().contents_between(start_row, start_col, end_row, end_col + 1);
        if text.is_empty() {
            return;
        }
        clipboard::copy(&text);
        let count = text.chars().count();
        self.status = Some((format!("copied {count} character{}", if count == 1 { "" } else { "s" }), Instant::now()));
    }

    fn paste(&mut self, text: &str) {
        let Some(tab) = self.active_tab_mut() else { return };
        // Terminals send pasted line breaks as carriage returns.
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if tab.screen().bracketed_paste() {
            tab.write(format!("\x1b[200~{text}\x1b[201~"));
        } else {
            tab.write(text);
        }
    }
}

/// The run of non-separator characters around (row, col), for double-click.
fn word_bounds(screen: &vt100::Screen, row: u16, col: u16) -> ((u16, u16), (u16, u16)) {
    let is_word = |c: u16| {
        screen.cell(row, c).is_some_and(|cell| {
            let text = cell.contents();
            !text.is_empty() && !text.chars().any(|ch| ch.is_whitespace() || "\"'`()[]{}<>|,;".contains(ch))
        })
    };
    if !is_word(col) {
        return ((row, col), (row, col));
    }
    let (_, cols) = screen.size();
    let mut start = col;
    while start > 0 && is_word(start - 1) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cols && is_word(end + 1) {
        end += 1;
    }
    ((row, start), (row, end))
}
