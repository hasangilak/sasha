use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::DefaultTerminal;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;

use crate::input;
use crate::sidebar::{self, Hit, Layout};
use crate::tab::{Tab, TabId};
use crate::ui;

/// Minimum time between two frames (~120 fps).
const FRAME: Duration = Duration::from_millis(8);
/// How often tab titles (foreground process, cwd) are refreshed.
const INFO_REFRESH: Duration = Duration::from_millis(500);
/// A background tab counts as busy (spinner) this long after its last output.
const BUSY: Duration = Duration::from_millis(800);
const SPINNER_STEP: Duration = Duration::from_millis(80);

pub enum AppEvent {
    Input(Event),
    Output(TabId, Vec<u8>),
    Exited(TabId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Tab(usize),
    Divider,
}

pub struct App {
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) layout: Layout,
    /// First visible row of the tab list.
    pub(crate) scroll: usize,
    pub(crate) hover: Option<Hit>,
    sidebar_width: u16,
    collapsed: bool,
    drag: Option<Drag>,
    next_id: TabId,
    events_tx: Sender<AppEvent>,
    events: Receiver<AppEvent>,
    started: Instant,
    quit: bool,
}

impl App {
    pub fn new(size: Size) -> Result<Self> {
        let (events_tx, events) = mpsc::channel();
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            layout: Layout::default(),
            scroll: 0,
            hover: None,
            sidebar_width: sidebar::DEFAULT_WIDTH,
            collapsed: false,
            drag: None,
            next_id: 0,
            events_tx,
            events,
            started: Instant::now(),
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

    /// Paint the active tab in the accent colour (a modal state is active).
    pub fn highlight_active(&self) -> bool {
        false
    }

    pub fn footer(&self) -> (Option<(&'static str, Color)>, String) {
        let count = self.tabs.len();
        (None, format!("{count} tab{}", if count == 1 { "" } else { "s" }))
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
        self.active = index;
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
        let _clipboard = tab.process(bytes);
        // Shells redraw their prompt after a resize; that isn't news.
        if !tab.recently_resized() {
            tab.last_activity = Instant::now();
            if Some(id) != active_id {
                tab.unseen = true;
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let Some(tab) = self.active_tab_mut() else { return };
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
            (MouseEventKind::Up(_), Some(_)) => {
                self.drag = None;
                return;
            }
            _ => {}
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
        let Some(tab) = self.active_tab_mut() else { return };
        let screen = tab.screen();
        let mode = screen.mouse_protocol_mode();
        // Shift bypasses the program's mouse handling, as in xterm.
        if mode != vt100::MouseProtocolMode::None && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
            let encoding = screen.mouse_protocol_encoding();
            if let Some(bytes) = input::encode_mouse(&mouse, col, row, mode, encoding) {
                tab.write(bytes);
            }
        }
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
