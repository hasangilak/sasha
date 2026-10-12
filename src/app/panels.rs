//! The ports that programs in the tabs listen on and the repository's open
//! pull requests (via `gh`): kept fresh for the bars beside the sidebar and
//! the pane, and shown in full in panels over the pane.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Position;

use super::{App, AppEvent, Mode};
use crate::github::{self, FileDiff, Pr};
use crate::panel_view;
use crate::ports::{self, Port};
use crate::tab::{Tab, TabId};

/// How often the ports bar looks again.
const PORTS_EVERY: Duration = Duration::from_secs(3);
/// How long a repository's list of pull requests is trusted.
const PRS_EVERY: Duration = Duration::from_secs(60);

static NOT_YET: Loadable<Vec<Pr>> = Loadable::Loading;

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

/// A repository's open pull requests, as last fetched.
pub struct PrList {
    pub prs: Loadable<Vec<Pr>>,
    fetched: Option<Instant>,
    fetching: bool,
}

pub enum Panel {
    Prs(PrPanel),
    Ports(PortsPanel),
}

pub struct PrPanel {
    /// The repository: where `gh` runs, and the key into `App::prs`.
    pub dir: PathBuf,
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
    ReloadPrs(PathBuf),
    Kill(Port),
    GoTo(TabId),
    CheckPorts,
}

impl App {
    // ── The bars ────────────────────────────────────────────────────────

    /// The repository the tab on screen is in: the nearest folder up from
    /// it with a `.git`.
    pub fn active_repo(&self) -> Option<PathBuf> {
        let cwd = self.active_tab().and_then(Tab::cwd)?;
        cwd.ancestors().find(|dir| dir.join(".git").exists()).map(Path::to_path_buf)
    }

    /// The pull requests of `dir`, as far as they're known.
    pub fn prs_of(&self, dir: &Path) -> &Loadable<Vec<Pr>> {
        self.prs.get(dir).map_or(&NOT_YET, |list| &list.prs)
    }

    /// The pull requests listed in the bar, once they've loaded.
    pub fn bar_prs(&self) -> Option<&[Pr]> {
        let dir = self.active_repo()?;
        self.prs_of(&dir).ready().map(Vec::as_slice)
    }

    pub fn port_count(&self) -> usize {
        self.ports.ready().map_or(0, Vec::len)
    }

    /// Keeps the bars' ports and pull requests fresh while a window shows
    /// them.
    pub(super) fn refresh_bars(&mut self) {
        if self.client.is_none() {
            return;
        }
        let ports_shown = self.layout.ports_bar.width > 0 || matches!(self.panel, Some(Panel::Ports(_)));
        if ports_shown && self.ports_checked.is_none_or(|at| at.elapsed() >= PORTS_EVERY) {
            self.check_ports(Duration::ZERO);
        }
        if self.layout.pr_bar.width > 0
            && let Some(dir) = self.active_repo()
            && self.prs_stale(&dir)
        {
            self.fetch_prs(dir);
        }
    }

    /// Whether `dir`'s pull requests are worth fetching (again).
    fn prs_stale(&self, dir: &Path) -> bool {
        self.prs.get(dir).is_none_or(|l| !l.fetching && l.fetched.is_none_or(|at| at.elapsed() >= PRS_EVERY))
    }

    fn fetch_prs(&mut self, dir: PathBuf) {
        let list = self.prs.entry(dir.clone()).or_insert(PrList { prs: Loadable::Loading, fetched: None, fetching: false });
        if list.fetching {
            return;
        }
        list.fetching = true;
        let events = self.events_tx.clone();
        thread::spawn(move || {
            let prs = github::open_prs(&dir);
            let _ = events.send(AppEvent::Prs(dir, prs));
        });
    }

    fn check_ports(&mut self, after: Duration) {
        if self.ports_checking {
            return;
        }
        self.ports_checking = true;
        let shells: Vec<(i32, TabId)> =
            self.workspaces.iter().flat_map(|ws| &ws.tabs).filter_map(|t| Some((t.shell_pid()?, t.id))).collect();
        let events = self.events_tx.clone();
        thread::spawn(move || {
            thread::sleep(after);
            let _ = events.send(AppEvent::Ports(ports::listening(&shells)));
        });
    }

    pub(super) fn prs_loaded(&mut self, dir: PathBuf, prs: Result<Vec<Pr>, String>) {
        let count = prs.as_ref().map_or(0, Vec::len);
        self.prs.insert(dir.clone(), PrList { prs: Loadable::from(prs), fetched: Some(Instant::now()), fetching: false });
        if let Some(Panel::Prs(panel)) = &mut self.panel
            && panel.dir == dir
        {
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
        self.ports = Loadable::from(ports);
        self.ports_checking = false;
        self.ports_checked = Some(Instant::now());
        let count = self.port_count();
        if let Some(Panel::Ports(panel)) = &mut self.panel {
            panel.selected = panel.selected.min(count.saturating_sub(1));
        }
    }

    // ── The panels ──────────────────────────────────────────────────────

    /// Shows the open pull requests of the repository the tab on screen is
    /// in.
    pub(super) fn open_prs(&mut self) {
        let dir = self.pr_dir();
        self.show_prs(dir, 0);
    }

    /// Opens the bar's `index`th pull request, straight to its files.
    pub(super) fn open_bar_pr(&mut self, index: usize) {
        let Some(dir) = self.active_repo() else { return };
        let Some(pr) = self.prs_of(&dir).ready().and_then(|prs| prs.get(index)).cloned() else { return };
        self.show_prs(dir.clone(), index);
        if let Some(Panel::Prs(panel)) = &mut self.panel {
            panel.open = Some(OpenPr { number: pr.number, title: pr.title, files: Loadable::Loading, selected: 0, scroll: 0 });
        }
        self.load_files(dir, pr.number);
    }

    /// The repository to ask about: the tab's, else its folder (and `gh`
    /// says what's wrong).
    fn pr_dir(&self) -> PathBuf {
        self.active_repo()
            .or_else(|| self.active_tab().and_then(Tab::cwd).map(Path::to_path_buf))
            .unwrap_or_else(|| self.ws().path.clone())
    }

    fn show_prs(&mut self, dir: PathBuf, selected: usize) {
        if self.prs_stale(&dir) {
            self.fetch_prs(dir.clone());
        }
        self.panel = Some(Panel::Prs(PrPanel { dir, selected, open: None }));
        self.mode = Mode::Panel;
    }

    fn load_files(&self, dir: PathBuf, number: u64) {
        let events = self.events_tx.clone();
        thread::spawn(move || {
            let files = github::pr_files(&dir, number);
            let _ = events.send(AppEvent::PrFiles(number, files));
        });
    }

    /// Shows the ports that programs in the tabs listen on, with `index`
    /// selected.
    pub(super) fn open_ports(&mut self, index: usize) {
        self.panel = Some(Panel::Ports(PortsPanel { selected: index, confirm: None }));
        self.mode = Mode::Panel;
        self.check_ports(Duration::ZERO);
    }

    /// Refetches the bar's pull requests.
    pub(super) fn reload_bar_prs(&mut self) {
        if let Some(dir) = self.active_repo() {
            self.fetch_prs(dir);
        }
    }

    /// A click in a panel: picks a pull request (and opens it), a file, or a
    /// port.
    pub(super) fn panel_click(&mut self, x: u16, y: u16) {
        let body = panel_view::body(self.layout.pane);
        let pos = Position::new(x, y);
        if !body.contains(pos) {
            return;
        }
        let row = usize::from(y - body.y);
        let ports = self.port_count();
        let mut load = None;
        match &mut self.panel {
            Some(Panel::Prs(PrPanel { open: Some(open), .. })) => {
                let (list, _) = panel_view::split(body);
                let count = open.files.ready().map_or(0, Vec::len);
                let index = panel_view::first_shown(open.selected, list.height) + row;
                if list.contains(pos) && index < count {
                    open.selected = index;
                    open.scroll = 0;
                }
            }
            Some(Panel::Prs(panel)) => {
                let prs = self.prs.get(&panel.dir).and_then(|l| l.prs.ready());
                let index = panel_view::first_shown(panel.selected, body.height) + row;
                if let Some(pr) = prs.and_then(|prs| prs.get(index)) {
                    panel.selected = index;
                    let (number, title) = (pr.number, pr.title.clone());
                    panel.open = Some(OpenPr { number, title, files: Loadable::Loading, selected: 0, scroll: 0 });
                    load = Some((panel.dir.clone(), number));
                }
            }
            Some(Panel::Ports(panel)) => {
                let index = panel_view::first_shown(panel.selected, body.height) + row;
                if index < ports {
                    panel.selected = index;
                    panel.confirm = None;
                }
            }
            None => {}
        }
        if let Some((dir, number)) = load {
            self.load_files(dir, number);
        }
    }

    /// The mouse wheel over a panel: over a pull request's files it moves
    /// through them, over its diff it scrolls; elsewhere it moves the
    /// highlight.
    pub(super) fn panel_wheel(&mut self, down: bool, x: u16, y: u16) {
        let step: isize = if down { 3 } else { -3 };
        let ports = self.port_count();
        let (list, _) = panel_view::split(panel_view::body(self.layout.pane));
        let over_files = list.contains(Position::new(x, y));
        match &mut self.panel {
            Some(Panel::Prs(PrPanel { open: Some(open), .. })) if over_files => {
                let count = open.files.ready().map_or(0, Vec::len);
                open.selected = step_in(open.selected, step.signum(), count);
                open.scroll = 0;
            }
            Some(Panel::Prs(PrPanel { open: Some(open), .. })) => scroll_diff(open, step),
            Some(Panel::Prs(panel)) => {
                let count = self.prs.get(&panel.dir).and_then(|l| l.prs.ready()).map_or(0, Vec::len);
                panel.selected = step_in(panel.selected, step.signum(), count);
            }
            Some(Panel::Ports(panel)) => panel.selected = step_in(panel.selected, step.signum(), ports),
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
                    KeyCode::PageDown | KeyCode::Char(' ') => scroll(open, page),
                    KeyCode::PageUp => scroll(open, -page),
                    KeyCode::Char('d') if ctrl => scroll(open, page / 2),
                    KeyCode::Char('u') if ctrl => scroll(open, -page / 2),
                    KeyCode::Char('g') => scroll(open, isize::MIN / 2),
                    KeyCode::Char('G') => scroll(open, isize::MAX / 2),
                    KeyCode::Char('o') => Some(Action::Browser(dir.clone(), open.number)),
                    _ => None,
                }
            }
            // The list of open pull requests.
            Some(Panel::Prs(panel)) => {
                let prs = self.prs.get(&panel.dir).and_then(|l| l.prs.ready());
                let selected = prs.and_then(|prs| prs.get(panel.selected)).cloned();
                let count = prs.map_or(0, Vec::len);
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
                    KeyCode::Char('r') => Some(Action::ReloadPrs(panel.dir.clone())),
                    _ => None,
                }
            }
            Some(Panel::Ports(panel)) => {
                let ports = self.ports.ready();
                let selected = ports.and_then(|ports| ports.get(panel.selected)).cloned();
                let count = ports.map_or(0, Vec::len);
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
                    KeyCode::Char('r') => Some(Action::CheckPorts),
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
            Some(Action::LoadFiles(dir, number)) => self.load_files(dir, number),
            Some(Action::Browser(dir, number)) => {
                thread::spawn(move || {
                    let _ = github::open_in_browser(&dir, number);
                });
                self.set_status(format!("opening #{number} in the browser"));
            }
            Some(Action::ReloadPrs(dir)) => self.fetch_prs(dir),
            Some(Action::Kill(port)) => {
                match ports::force_kill(port.pid) {
                    Ok(()) => self.set_status(format!("killed {} ({}) on :{}", port.command, port.pid, port.port)),
                    Err(err) => self.set_status(format!("can't kill {}: {err}", port.pid)),
                }
                // Give the port a moment to close before looking again.
                self.check_ports(Duration::from_millis(300));
            }
            Some(Action::GoTo(tab)) => {
                self.panel = None;
                self.mode = Mode::Normal;
                if let Some((w, t)) = self.find_tab(tab) {
                    self.go_to(w, t);
                }
            }
            Some(Action::CheckPorts) => self.check_ports(Duration::ZERO),
        }
    }
}

/// Moves the diff `delta` lines, keeping at least its last line in view.
fn scroll_diff(open: &mut OpenPr, delta: isize) {
    let lines = open.files.ready().and_then(|files| files.get(open.selected)).map_or(0, |f| f.lines.len());
    open.scroll = open.scroll.saturating_add_signed(delta).min(lines.saturating_sub(1));
}

fn scroll(open: &mut OpenPr, delta: isize) -> Option<Action> {
    scroll_diff(open, delta);
    None
}

/// `index` moved by `delta`, within `0..count`.
fn step_in(index: usize, delta: isize, count: usize) -> usize {
    index.saturating_add_signed(delta).min(count.saturating_sub(1))
}
