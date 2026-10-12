use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyEvent, KeyEventKind};
use ratatui::DefaultTerminal;
use ratatui::layout::{Rect, Size};

use crate::input;
use crate::tab::{Tab, TabId};
use crate::ui;

/// Minimum time between two frames (~120 fps).
const FRAME: Duration = Duration::from_millis(8);
/// How often tab titles (foreground process, cwd) are refreshed.
const INFO_REFRESH: Duration = Duration::from_millis(500);

pub enum AppEvent {
    Input(Event),
    Output(TabId, Vec<u8>),
    Exited(TabId),
}

pub struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: TabId,
    events_tx: Sender<AppEvent>,
    events: Receiver<AppEvent>,
    pane: Rect,
    quit: bool,
}

impl App {
    pub fn new(size: Size) -> Result<Self> {
        let (events_tx, events) = mpsc::channel();
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            events_tx,
            events,
            pane: Rect::default(),
            quit: false,
        };
        app.relayout(size);
        app.new_tab()?;
        Ok(app)
    }

    pub fn pane(&self) -> Rect {
        self.pane
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
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
        while !self.quit && !self.tabs.is_empty() {
            let timeout = if dirty { FRAME.saturating_sub(last_draw.elapsed()) } else { INFO_REFRESH };
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
            if dirty && last_draw.elapsed() >= FRAME && !self.tabs.is_empty() {
                self.relayout(terminal.size()?);
                terminal.draw(|frame| ui::draw(frame, self))?;
                last_draw = Instant::now();
                dirty = false;
            }
        }
        Ok(())
    }

    /// Recomputes the pane and keeps every pty sized to it.
    fn relayout(&mut self, size: Size) {
        self.pane = Rect::new(0, 0, size.width, size.height);
        let (rows, cols) = (self.pane.height.max(1), self.pane.width.max(1));
        for tab in &mut self.tabs {
            tab.resize(rows, cols);
        }
    }

    fn new_tab(&mut self) -> Result<()> {
        let cwd = match self.active_tab() {
            Some(tab) => tab.cwd().map(ToOwned::to_owned),
            None => std::env::current_dir().ok(),
        };
        let id = self.next_id;
        self.next_id += 1;
        let size = (self.pane.height.max(1), self.pane.width.max(1));
        let tab = Tab::spawn(id, size, cwd.as_deref(), self.events_tx.clone())?;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    fn remove_tab(&mut self, index: usize) {
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn refresh_tabs(&mut self) {
        let mut index = 0;
        while index < self.tabs.len() {
            if self.tabs[index].has_exited() {
                self.remove_tab(index);
                continue;
            }
            self.tabs[index].refresh_info();
            index += 1;
        }
    }

    fn handle(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => self.handle_key(key),
            AppEvent::Input(Event::Paste(text)) => self.paste(&text),
            AppEvent::Input(_) => {}
            AppEvent::Output(id, bytes) => {
                let active_id = self.active_tab().map(|t| t.id);
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                    let _clipboard = tab.process(&bytes);
                    if Some(id) != active_id {
                        tab.unseen = true;
                    }
                }
            }
            AppEvent::Exited(id) => {
                if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
                    self.remove_tab(index);
                }
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
