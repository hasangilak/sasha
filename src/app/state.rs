//! The interaction state machine: which mode sasha is in, what each mode
//! holds, and the rules for moving between them. Everything here is plain
//! data and pure functions, so the rules can be tested without terminals;
//! `App::enter` is the only place the mode changes.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::panels::Panel;
use crate::tower::Engine;
use crate::files::Preview;
use crate::tab::TabId;
use crate::workspace::{GroupId, WorkspaceId};

/// Pressing the prefix twice this quickly sends it to the program.
pub const PREFIX_TWICE: Duration = Duration::from_millis(400);
/// After a tab-mode command, tab mode stays this long for the next one
/// (j j k…), then ends by itself.
pub const TAB_MODE_REPEAT: Duration = Duration::from_secs(2);
/// How long a "press it again" question waits for its answer.
pub const CONFIRM_FOR: Duration = Duration::from_secs(5);

/// What sasha does with keys, with whatever that mode is working on.
pub enum Mode {
    /// Keys go to the program in the tab.
    Normal,
    /// Entered with the prefix: single keys manage tabs.
    Tab(TabMode),
    /// Browsing the scrollback with vim-ish keys.
    Scroll,
    /// Typing a name in the sidebar.
    Rename(Rename),
    /// Choosing from a list by typing: tabs, workspaces or groups.
    Picker(Picker),
    /// Pull requests or ports, over the pane.
    Panel(Panel),
    /// Keys move through the files bar; a file can be shown over the pane.
    Files(Browse),
    Help,
    ConfirmQuit,
}

impl Mode {
    pub fn is_tab(&self) -> bool {
        matches!(self, Mode::Tab(_))
    }
}

/// Browsing the files bar.
#[derive(Default)]
pub struct Browse {
    /// The file shown over the pane, if one is.
    pub preview: Option<Preview>,
}

// ── Tab mode ────────────────────────────────────────────────────────────

/// Tab mode's clock: when the prefix was pressed, and when tab mode ends
/// by itself (`None` while it waits for its first key).
#[derive(Clone, Copy, Debug)]
pub struct TabMode {
    pressed: Instant,
    until: Option<Instant>,
}

/// What the prefix does while tab mode is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefix {
    /// A quick double press: send it to the program, like tmux.
    PassThrough,
    /// Tab mode was still on: it starts over, waiting for a key.
    Restart,
}

impl TabMode {
    pub fn new(now: Instant) -> Self {
        Self { pressed: now, until: None }
    }

    pub fn prefix(&mut self, now: Instant) -> Prefix {
        if now.saturating_duration_since(self.pressed) < PREFIX_TWICE {
            return Prefix::PassThrough;
        }
        *self = Self::new(now);
        Prefix::Restart
    }

    /// A command ran and tab mode stays: wait a moment for the next one.
    pub fn repeat(&mut self, now: Instant) {
        self.until = Some(now + TAB_MODE_REPEAT);
    }

    pub fn until(&self) -> Option<Instant> {
        self.until
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now >= until)
    }
}

/// What a key does in tab mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NextTab,
    PrevTab,
    MoveDown,
    MoveUp,
    NextWorkspace,
    PrevWorkspace,
    FirstTab,
    LastTab,
    Jump(char),
    PreviousTab,
    PreviousWorkspace,
    NewTab,
    Close,
    Pin,
    GroupPicker,
    Ungroup,
    ToggleGroup,
    RenameGroup,
    Workspaces,
    RenameWorkspace,
    ToggleSidebar,
    CycleWorkspaceBar,
    ToggleStacked,
    Minimize,
    KeepAwake,
    PullRequests,
    Ports,
    Files,
    /// Goes to the tower, or back from it.
    Tower,
    /// Runs the tower on this subscription.
    TowerOn(Engine),
    TowerRestart,
    TowerHide,
    /// News from its sessions goes straight to the tower, or waits.
    TowerAutopilot,
    /// Starts listening to you, or sends what you said to the tower.
    Talk,
    /// Stops listening without sending.
    TalkCancel,
    /// The tower says the big picture of its answers aloud, or doesn't.
    TowerVoice,
    /// Opens your instructions for the tower in your editor.
    TowerRole,
    SideBars,
    Narrower,
    Wider,
    Scroll,
    Rename,
    Search,
    Help,
    Detach,
    Quit,
    /// Anything else: back to the program.
    Leave,
}

/// Where a command leaves tab mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// Tab mode stays for the next key (j j k…).
    Stay,
    /// Back to the program.
    Leave,
    /// The command opens a mode of its own.
    Own,
}

impl Command {
    pub fn from_key(key: &KeyEvent) -> Self {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('J') => Command::MoveDown,
            KeyCode::Char('K') => Command::MoveUp,
            KeyCode::Down if shift => Command::MoveDown,
            KeyCode::Up if shift => Command::MoveUp,
            KeyCode::Char('j') | KeyCode::Down => Command::NextTab,
            KeyCode::Char('k') | KeyCode::Up => Command::PrevTab,
            KeyCode::Char('l') | KeyCode::Right => Command::NextWorkspace,
            KeyCode::Char('h') | KeyCode::Left => Command::PrevWorkspace,
            KeyCode::Char('g') | KeyCode::Home => Command::FirstTab,
            KeyCode::Char('G') | KeyCode::End => Command::LastTab,
            KeyCode::Char(c @ '1'..='9') => Command::Jump(c),
            KeyCode::Tab => Command::PreviousTab,
            KeyCode::BackTab => Command::PreviousWorkspace,
            KeyCode::Char('t' | 'n' | 'c') => Command::NewTab,
            KeyCode::Char('x') => Command::Close,
            KeyCode::Char('p') => Command::Pin,
            KeyCode::Char('m') => Command::GroupPicker,
            KeyCode::Char('u') => Command::Ungroup,
            KeyCode::Char('z') => Command::ToggleGroup,
            KeyCode::Char('e') => Command::RenameGroup,
            KeyCode::Char('w') => Command::Workspaces,
            KeyCode::Char('W') => Command::RenameWorkspace,
            KeyCode::Char('b' | 's') => Command::ToggleSidebar,
            KeyCode::Char('a') => Command::CycleWorkspaceBar,
            KeyCode::Char('o') => Command::ToggleStacked,
            KeyCode::Char('-') => Command::Minimize,
            KeyCode::Char('C') => Command::KeepAwake,
            KeyCode::Char('P') => Command::PullRequests,
            KeyCode::Char('O') => Command::Ports,
            KeyCode::Char('F') => Command::Files,
            KeyCode::Char('T') => Command::Tower,
            KeyCode::Char('V') => Command::Talk,
            KeyCode::Char('i') => Command::SideBars,
            KeyCode::Char('<') => Command::Narrower,
            KeyCode::Char('>') => Command::Wider,
            KeyCode::Char('[' | 'v') | KeyCode::PageUp => Command::Scroll,
            KeyCode::Char('r' | ',') => Command::Rename,
            KeyCode::Char('/' | 'f') => Command::Search,
            KeyCode::Char('?') => Command::Help,
            KeyCode::Char('d') => Command::Detach,
            KeyCode::Char('q') => Command::Quit,
            _ => Command::Leave,
        }
    }

    pub fn after(self) -> After {
        match self {
            Command::Jump(_)
            | Command::PreviousTab
            | Command::PreviousWorkspace
            | Command::NewTab
            | Command::Tower
            | Command::TowerOn(_)
            | Command::TowerRestart
            | Command::TowerHide
            | Command::TowerAutopilot
            | Command::Talk
            | Command::TalkCancel
            | Command::TowerVoice
            | Command::TowerRole
            | Command::Leave => {
                After::Leave
            }
            Command::GroupPicker
            | Command::RenameGroup
            | Command::Workspaces
            | Command::RenameWorkspace
            | Command::PullRequests
            | Command::Ports
            | Command::Files
            | Command::Scroll
            | Command::Rename
            | Command::Search
            | Command::Help
            | Command::Detach
            | Command::Quit => After::Own,
            _ => After::Stay,
        }
    }
}

// ── Renaming and pickers ────────────────────────────────────────────────

/// What rename mode is renaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renaming {
    /// The active tab's prefix.
    Tab,
    Group(GroupId),
    /// The current workspace.
    Workspace,
}

pub struct Rename {
    pub target: Renaming,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    /// Tabs in every workspace.
    Search,
    /// Switching, adding and removing workspaces.
    Workspaces,
    /// Putting the active tab into a group.
    Groups,
    /// Commands, from the header.
    Menu(Menu),
}

/// The header's menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    /// What can be done with the terminal on screen: its title's menu.
    Terminal,
    /// Everything else: the ⋮ button's.
    More,
    /// The tower's: its name's, over its panel.
    Tower,
}

pub struct Picker {
    pub kind: PickerKind,
    pub query: String,
    /// The highlighted row.
    pub selected: usize,
}

impl Picker {
    pub fn new(kind: PickerKind, query: String) -> Self {
        Self { kind, query, selected: 0 }
    }

    /// Moves the highlight with ↑ ↓ or ^p ^n (^k ^j); false for other keys.
    pub fn step(&mut self, key: &KeyEvent, rows: usize) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let up = key.code == KeyCode::Up || (ctrl && matches!(key.code, KeyCode::Char('p' | 'k')));
        let down = key.code == KeyCode::Down || (ctrl && matches!(key.code, KeyCode::Char('n' | 'j')));
        if up {
            self.selected = self.selected.saturating_sub(1);
        } else if down {
            self.selected = (self.selected + 1).min(rows.saturating_sub(1));
        } else {
            return false;
        }
        true
    }

    /// Types into the query; a new query starts at the top of the list.
    pub fn edit(&mut self, key: &KeyEvent) {
        edit_line(&mut self.query, key);
        self.selected = 0;
    }
}

/// Minimal line editing for names and queries.
pub fn edit_line(text: &mut String, key: &KeyEvent) {
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

// ── Asking twice ────────────────────────────────────────────────────────

/// Something that only happens when asked for twice in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// A pinned tab: `x` twice.
    CloseTab(TabId),
    /// A workspace's section of the sidebar and its tabs: × twice.
    CloseSection(WorkspaceId),
    /// A workspace with open tabs: `^D` twice in the picker.
    RemoveWorkspace(WorkspaceId),
    /// A program listening on a port: `x` twice.
    KillPort(i32),
}

/// The one question waiting for a second press, if any. Any other key or
/// click forgets it, and so does time.
#[derive(Debug, Default)]
pub struct Pending {
    question: Option<(Confirm, Instant)>,
    /// Whether the key or click being handled asked or answered.
    touched: bool,
}

impl Pending {
    /// True when `what` was just asked for: go ahead. Otherwise it's asked
    /// now, and the caller says "press again".
    pub fn confirm(&mut self, what: Confirm, now: Instant) -> bool {
        self.touched = true;
        let asked = self.question.is_some_and(|(q, at)| q == what && now.saturating_duration_since(at) < CONFIRM_FOR);
        self.question = if asked { None } else { Some((what, now)) };
        asked
    }

    /// The question waiting, if it's still fresh.
    pub fn waiting(&self, now: Instant) -> Option<Confirm> {
        self.question.filter(|(_, at)| now.saturating_duration_since(*at) < CONFIRM_FOR).map(|(q, _)| q)
    }

    /// When the question waiting runs out.
    pub fn deadline(&self) -> Option<Instant> {
        self.question.map(|(_, at)| at + CONFIRM_FOR)
    }

    /// Forgets a question that has run out; true if there was one.
    pub fn expire(&mut self, now: Instant) -> bool {
        let stale = self.question.is_some() && self.waiting(now).is_none();
        if stale {
            self.question = None;
        }
        stale
    }

    /// Keeps the question across a key that's only a step towards
    /// answering it (the prefix in ^B x … ^B x).
    pub fn keep(&mut self) {
        self.touched = true;
    }

    /// Before handling a key or a click.
    pub fn begin(&mut self) {
        self.touched = false;
    }

    /// After it: one that neither asked nor answered forgets the question.
    pub fn end(&mut self) {
        if !self.touched {
            self.question = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn a_quick_second_prefix_passes_through_a_late_one_restarts() {
        let start = Instant::now();
        let mut tab = TabMode::new(start);
        assert_eq!(tab.prefix(start + Duration::from_millis(100)), Prefix::PassThrough);
        // ^B j, then ^B a second later: tab mode just starts over (the bug
        // where it sent ^B to the shell and the q of ^B q went with it).
        tab.repeat(start);
        assert_eq!(tab.prefix(start + Duration::from_secs(1)), Prefix::Restart);
        assert_eq!(tab.until(), None);
        assert_eq!(Command::from_key(&key(KeyCode::Char('q'))), Command::Quit);
    }

    #[test]
    fn tab_mode_waits_for_its_first_key_then_ends_after_a_pause() {
        let start = Instant::now();
        let mut tab = TabMode::new(start);
        assert!(!tab.expired(start + Duration::from_secs(60)));
        tab.repeat(start);
        assert!(!tab.expired(start + Duration::from_secs(1)));
        assert!(tab.expired(start + TAB_MODE_REPEAT));
    }

    #[test]
    fn commands_and_where_they_leave_tab_mode() {
        let after = |code| Command::from_key(&key(code)).after();
        assert_eq!(after(KeyCode::Char('j')), After::Stay);
        assert_eq!(after(KeyCode::Char('x')), After::Stay);
        assert_eq!(after(KeyCode::Char('t')), After::Leave);
        assert_eq!(after(KeyCode::Char('3')), After::Leave);
        assert_eq!(after(KeyCode::Char('m')), After::Own);
        assert_eq!(after(KeyCode::Esc), After::Leave);
        let shift_down = KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(Command::from_key(&shift_down), Command::MoveDown);
    }

    #[test]
    fn a_question_needs_the_same_answer_straight_away() {
        let start = Instant::now();
        let mut pending = Pending::default();
        let pinned = Confirm::CloseTab(7);
        pending.begin();
        assert!(!pending.confirm(pinned, start));
        pending.end();
        assert_eq!(pending.waiting(start), Some(pinned));
        // The same request again: yes.
        pending.begin();
        assert!(pending.confirm(pinned, start));
        pending.end();
        assert_eq!(pending.waiting(start), None);
        // Another key in between forgets it.
        pending.begin();
        pending.confirm(pinned, start);
        pending.end();
        pending.begin();
        pending.end();
        assert!(!pending.confirm(pinned, start));
        // So does time.
        let later = start + CONFIRM_FOR;
        assert!(!pending.confirm(pinned, later + CONFIRM_FOR));
        assert_eq!(pending.deadline(), Some(later + CONFIRM_FOR + CONFIRM_FOR));
        assert!(!pending.expire(later + CONFIRM_FOR));
        assert!(pending.expire(later + CONFIRM_FOR + CONFIRM_FOR));
        assert_eq!(pending.deadline(), None);
        // A different question replaces it.
        pending.confirm(Confirm::KillPort(1), later);
        assert!(!pending.confirm(pinned, later));
    }

    #[test]
    fn pickers_move_and_type() {
        let mut picker = Picker::new(PickerKind::Search, String::new());
        assert!(picker.step(&key(KeyCode::Down), 3));
        assert!(picker.step(&key(KeyCode::Down), 3));
        assert!(picker.step(&key(KeyCode::Down), 3));
        assert_eq!(picker.selected, 2);
        assert!(!picker.step(&key(KeyCode::Char('a')), 3));
        picker.edit(&key(KeyCode::Char('a')));
        assert_eq!((picker.query.as_str(), picker.selected), ("a", 0));
        let mut path = "~/Work/sasha/".to_owned();
        edit_line(&mut path, &KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(path, "~/Work/");
    }
}
