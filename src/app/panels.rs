//! Panels over the terminal pane: the repository's open pull requests (via
//! `gh`), and the ports that programs in the tabs listen on.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, AppEvent, Mode};
use crate::github::{self, FileDiff, Pr};
use crate::ports::{self, Port};
use crate::tab::{Tab, TabId};

/// Something fetched in the background.
pub enum Loadable<T> {
    Loading,
    Failed(String),
    Ready(T),
}

impl<T> Loadable<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(value) => Loadable::Ready(value),
            Err(err) => Loadable::Failed(err),
        }
    }

    pub fn ready(&self) -> Option<&T> {
        match self {
            Loadable::Ready(value) => Some(value),
            _ => None,
        }
    }
}

pub enum Panel {
    Prs(PrPanel),
    Ports(PortsPanel),
}

pub struct PrPanel {
    /// Where `gh` runs: the folder of the tab that was on screen.
    pub dir: PathBuf,
    pub prs: Loadable<Vec<Pr>>,
    pub selected: usize,
    /// The pull request being looked at.
    pub open: Option<OpenPr>,
}

pub struct OpenPr {
    pub number: u64,
    pub title: String,
    pub files: Loadable<Vec<FileDiff>>,
    pub selected: usize,
    /// First line shown of the selected file's diff.
    pub scroll: usize,
}

pub struct PortsPanel {
    pub ports: Loadable<Vec<Port>>,
    pub selected: usize,
    /// The program waiting for a second `x`.
    pub confirm: Option<i32>,
}

/// What a key in a panel asks for, done once the panel is no longer
/// borrowed.
enum Action {
    Close,
    /// From a pull request's files back to the list.
    Back,
    LoadFiles(PathBuf, u64),
    Browser(PathBuf, u64),
    ReloadPrs,
    Kill(Port),
    GoTo(TabId),
    RefreshPorts,
}

impl App {
    /// Shows the open pull requests of the repository the tab on screen is
    /// in.
    pub(super) fn open_prs(&mut self) {
        let dir = self.active_tab().and_then(Tab::cwd).map_or_else(|| self.ws().path.clone(), Path::to_path_buf);
        self.panel = Some(Panel::Prs(PrPanel { dir: dir.clone(), prs: Loadable::Loading, selected: 0, open: None }));
        self.mode = Mode::Panel;
        let events = self.events_tx.clone();
        thread::spawn(move || {
            let prs = github::open_prs(&dir);
            let _ = events.send(AppEvent::Prs(dir, prs));
        });
    }

    /// Shows the ports that programs in the tabs listen on.
    pub(super) fn open_ports(&mut self) {
        self.panel = Some(Panel::Ports(PortsPanel { ports: Loadable::Loading, selected: 0, confirm: None }));
        self.mode = Mode::Panel;
        self.refresh_ports(Duration::ZERO);
    }

    fn refresh_ports(&self, after: Duration) {
        let shells: Vec<(i32, TabId)> =
            self.workspaces.iter().flat_map(|ws| &ws.tabs).filter_map(|t| Some((t.shell_pid()?, t.id))).collect();
        let events = self.events_tx.clone();
        thread::spawn(move || {
            thread::sleep(after);
            let _ = events.send(AppEvent::Ports(ports::listening(&shells)));
        });
    }

    pub(super) fn prs_loaded(&mut self, dir: &Path, prs: Result<Vec<Pr>, String>) {
        if let Some(Panel::Prs(panel)) = &mut self.panel
            && panel.dir == dir
        {
            panel.prs = Loadable::from(prs);
            let count = panel.prs.ready().map_or(0, Vec::len);
            panel.selected = panel.selected.min(count.saturating_sub(1));
        }
    }

    pub(super) fn pr_files_loaded(&mut self, number: u64, files: Result<Vec<FileDiff>, String>) {
        if let Some(Panel::Prs(PrPanel { open: Some(open), .. })) = &mut self.panel
            && open.number == number
        {
            open.files = Loadable::from(files);
        }
    }

    pub(super) fn ports_loaded(&mut self, ports: Result<Vec<Port>, String>) {
        if let Some(Panel::Ports(panel)) = &mut self.panel {
            panel.ports = Loadable::from(ports);
            let count = panel.ports.ready().map_or(0, Vec::len);
            panel.selected = panel.selected.min(count.saturating_sub(1));
        }
    }

    /// The mouse wheel over a panel: scrolls a diff, else moves the
    /// highlight.
    pub(super) fn panel_wheel(&mut self, down: bool) {
        let step: isize = if down { 3 } else { -3 };
        match &mut self.panel {
            Some(Panel::Prs(PrPanel { open: Some(open), .. })) => {
                scroll_diff(open, step);
            }
            Some(Panel::Prs(panel)) => {
                let count = panel.prs.ready().map_or(0, Vec::len);
                panel.selected = step_in(panel.selected, step.signum(), count);
            }
            Some(Panel::Ports(panel)) => {
                let count = panel.ports.ready().map_or(0, Vec::len);
                panel.selected = step_in(panel.selected, step.signum(), count);
            }
            None => {}
        }
    }

    pub(super) fn panel_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = isize::try_from(self.layout.pane.height.saturating_sub(6).max(2)).unwrap_or(2);
        let action = match &mut self.panel {
            None => Some(Action::Close),
            // A pull request's files and the selected one's diff.
            Some(Panel::Prs(PrPanel { dir, open: Some(open), .. })) => {
                let count = open.files.ready().map_or(0, Vec::len);
                match key.code {
                    KeyCode::Esc | KeyCode::Left | KeyCode::Char('q' | 'h') => Some(Action::Back),
                    KeyCode::Up | KeyCode::Char('k') => {
                        open.selected = step_in(open.selected, -1, count);
                        open.scroll = 0;
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        open.selected = step_in(open.selected, 1, count);
                        open.scroll = 0;
                        None
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => scroll_diff(open, page),
                    KeyCode::PageUp => scroll_diff(open, -page),
                    KeyCode::Char('d') if ctrl => scroll_diff(open, page / 2),
                    KeyCode::Char('u') if ctrl => scroll_diff(open, -page / 2),
                    KeyCode::Char('g') => scroll_diff(open, isize::MIN / 2),
                    KeyCode::Char('G') => scroll_diff(open, isize::MAX / 2),
                    KeyCode::Char('o') => Some(Action::Browser(dir.clone(), open.number)),
                    _ => None,
                }
            }
            // The list of open pull requests.
            Some(Panel::Prs(panel)) => {
                let selected = panel.prs.ready().and_then(|prs| prs.get(panel.selected)).cloned();
                let count = panel.prs.ready().map_or(0, Vec::len);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => Some(Action::Close),
                    KeyCode::Up | KeyCode::Char('k') => {
                        panel.selected = step_in(panel.selected, -1, count);
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        panel.selected = step_in(panel.selected, 1, count);
                        None
                    }
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => selected.map(|pr| {
                        panel.open = Some(OpenPr {
                            number: pr.number,
                            title: pr.title,
                            files: Loadable::Loading,
                            selected: 0,
                            scroll: 0,
                        });
                        Action::LoadFiles(panel.dir.clone(), pr.number)
                    }),
                    KeyCode::Char('o') => selected.map(|pr| Action::Browser(panel.dir.clone(), pr.number)),
                    KeyCode::Char('r') => Some(Action::ReloadPrs),
                    _ => None,
                }
            }
            Some(Panel::Ports(panel)) => {
                let selected = panel.ports.ready().and_then(|ports| ports.get(panel.selected)).cloned();
                let count = panel.ports.ready().map_or(0, Vec::len);
                if key.code != KeyCode::Char('x') {
                    panel.confirm = None;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => Some(Action::Close),
                    KeyCode::Up | KeyCode::Char('k') => {
                        panel.selected = step_in(panel.selected, -1, count);
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        panel.selected = step_in(panel.selected, 1, count);
                        None
                    }
                    // Force-killing needs a second press.
                    KeyCode::Char('x') => selected.and_then(|port| {
                        if panel.confirm == Some(port.pid) {
                            panel.confirm = None;
                            Some(Action::Kill(port))
                        } else {
                            panel.confirm = Some(port.pid);
                            None
                        }
                    }),
                    KeyCode::Enter => selected.map(|port| Action::GoTo(port.tab)),
                    KeyCode::Char('r') => Some(Action::RefreshPorts),
                    _ => None,
                }
            }
        };
        match action {
            None => {}
            Some(Action::Close) => {
                self.panel = None;
                self.mode = Mode::Normal;
            }
            Some(Action::Back) => {
                if let Some(Panel::Prs(panel)) = &mut self.panel {
                    panel.open = None;
                }
            }
            Some(Action::LoadFiles(dir, number)) => {
                let events = self.events_tx.clone();
                thread::spawn(move || {
                    let files = github::pr_files(&dir, number);
                    let _ = events.send(AppEvent::PrFiles(number, files));
                });
            }
            Some(Action::Browser(dir, number)) => {
                thread::spawn(move || {
                    let _ = github::open_in_browser(&dir, number);
                });
                self.set_status(format!("opening #{number} in the browser"));
            }
            Some(Action::ReloadPrs) => self.open_prs(),
            Some(Action::Kill(port)) => {
                match ports::force_kill(port.pid) {
                    Ok(()) => self.set_status(format!("killed {} ({}) on :{}", port.command, port.pid, port.port)),
                    Err(err) => self.set_status(format!("can't kill {}: {err}", port.pid)),
                }
                // Give the port a moment to close before looking again.
                self.refresh_ports(Duration::from_millis(300));
            }
            Some(Action::GoTo(tab)) => {
                self.panel = None;
                self.mode = Mode::Normal;
                if let Some((w, t)) = self.find_tab(tab) {
                    self.go_to(w, t);
                }
            }
            Some(Action::RefreshPorts) => self.refresh_ports(Duration::ZERO),
        }
    }
}

/// Moves the diff `delta` lines, keeping at least its last line in view.
fn scroll_diff(open: &mut OpenPr, delta: isize) -> Option<Action> {
    let lines = open.files.ready().and_then(|files| files.get(open.selected)).map_or(0, |f| f.lines.len());
    open.scroll = open.scroll.saturating_add_signed(delta).min(lines.saturating_sub(1));
    None
}

/// `index` moved by `delta`, within `0..count`.
fn step_in(index: usize, delta: isize, count: usize) -> usize {
    index.saturating_add_signed(delta).min(count.saturating_sub(1))
}
