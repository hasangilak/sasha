//! A tab is one shell running in its own pseudo-terminal, plus the vt100
//! emulator state that mirrors what the shell has drawn.

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use unicode_width::UnicodeWidthStr;

use crate::agents::{self, Agent, Conversation};
use crate::app::AppEvent;
use crate::handoff::{self, HandedChild, HandedPty, HandedTab};
use crate::mission::Mission;
use crate::{notify, procinfo};
use crate::workspace::GroupId;

pub type TabId = u64;

const SCROLLBACK_LINES: usize = 5_000;
/// Output this far apart belongs to separate stretches of work.
const QUIET: Duration = Duration::from_secs(3);
/// Stretches of work, and commands, at least this long are worth a
/// notification when they end.
const LONG_WORK: Duration = Duration::from_secs(15);
const LONG_COMMAND: Duration = Duration::from_secs(10);
/// How long a tab waits for the agent it's resuming to start.
const RESUME_WAIT: Duration = Duration::from_secs(60);
/// How often Codex's logs are searched for its conversation, until found.
const CODEX_LOOKUP: Duration = Duration::from_secs(30);
/// An agent that's drawn nothing for this long has finished its turn.
const TURN_OVER: Duration = Duration::from_secs(3);
/// How long an agent told to stop has to go before it's made to, and how
/// long after that it's given up on.
const STOP_WAIT: Duration = Duration::from_secs(5);
const KILL_WAIT: Duration = Duration::from_secs(3);
/// How often the agent running is compared with the one installed, and
/// its log read for whether it waits for you.
const VERSION_CHECK: Duration = Duration::from_secs(20);
const WAITS_CHECK: Duration = Duration::from_secs(2);

/// Handles the escape sequences vt100 parses but leaves to the embedder:
/// queries that need a reply, titles, bells, clipboard and cursor shape.
#[derive(Default)]
pub struct Callbacks {
    /// Bytes to send back to the child (answers to DSR / DA queries).
    replies: Vec<u8>,
    title: Option<String>,
    title_changed: bool,
    bell: bool,
    /// Base64 payload of an OSC 52 clipboard write.
    clipboard: Option<Vec<u8>>,
    cursor_style: u16,
    /// A notification the program asked for (OSC 9 or OSC 777).
    notice: Option<String>,
}

impl vt100::Callbacks for Callbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        let title = String::from_utf8_lossy(title).trim().to_owned();
        self.title = (!title.is_empty()).then_some(title);
        self.title_changed = true;
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        self.clipboard = Some(data.to_vec());
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        let text = |parts: &[&[u8]]| {
            let joined = parts.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(";");
            Some(joined.trim().to_owned()).filter(|t| !t.is_empty())
        };
        match params {
            // iTerm2's `OSC 9 ; message`. A number after the 9 is something
            // else (ConEmu's progress bar), not a message.
            [b"9", rest @ ..] if !rest.first().is_some_and(|p| !p.is_empty() && p.iter().all(u8::is_ascii_digit)) => {
                self.notice = text(rest);
            }
            // `OSC 777 ; notify ; title ; body`, from urxvt, Ghostty and others.
            [b"777", b"notify", title, body @ ..] => {
                self.notice = match text(body) {
                    Some(body) => Some(format!("{}: {body}", String::from_utf8_lossy(title))),
                    None => text(&[title]),
                };
            }
            _ => {}
        }
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c) {
            // Device status report: operating status and cursor position.
            (None, 'n') if first == 5 => self.replies.extend_from_slice(b"\x1b[0n"),
            (None | Some(b'?'), 'n') if first == 6 => {
                let (row, col) = screen.cursor_position();
                let marker = if i1.is_some() { "?" } else { "" };
                let reply = format!("\x1b[{marker}{};{}R", row + 1, col + 1);
                self.replies.extend_from_slice(reply.as_bytes());
            }
            // Primary / secondary device attributes.
            (None, 'c') if first == 0 => self.replies.extend_from_slice(b"\x1b[?62;22c"),
            (Some(b'>'), 'c') if first == 0 => self.replies.extend_from_slice(b"\x1b[>1;10;0c"),
            // DECSCUSR: cursor shape (block / underline / bar).
            (Some(b' '), 'q') => self.cursor_style = first,
            _ => {}
        }
    }
}

pub struct Tab {
    pub id: TabId,
    parser: vt100::Parser<Callbacks>,
    master: Box<dyn MasterPty + Send>,
    input: Sender<Vec<u8>>,
    child: Box<dyn Child + Send + Sync>,
    shell_pid: Option<i32>,
    /// The prefix the user gave the tab, in place of its folder name.
    pub custom_prefix: Option<String>,
    /// Pinned tabs sit at the top of their workspace and are restored on startup.
    pub pinned: bool,
    /// The tab group this tab belongs to (never set on pinned tabs).
    pub group: Option<GroupId>,
    /// The group it was in before it was pinned: unpinning puts it back.
    pub group_before_pin: Option<GroupId>,
    /// What the tower set it to do, if it's one of the tower's sessions.
    pub mission: Option<Mission>,
    /// Messages from other sessions, waiting for its turn to end.
    pub mailbox: Vec<String>,
    /// Name of the process in the foreground of this tab's tty.
    fg_name: String,
    fg_pgid: Option<i32>,
    /// Foreground process group at the time the OSC title was set.
    title_owner: Option<i32>,
    cwd: Option<PathBuf>,
    /// Last time the tab produced output that wasn't just a resize redraw.
    pub last_activity: Instant,
    /// Until then, output is the shell starting up or redrawing after a
    /// resize rather than news.
    settle_until: Instant,
    /// Output arrived while the tab was in the background.
    pub unseen: bool,
    pub bell: bool,
    /// News not yet considered for a notification: a bell, a program's own
    /// notification, a long command finishing, a long stretch of output.
    rang: bool,
    notice: Option<String>,
    finished: Option<(String, Duration)>,
    streak_start: Option<Instant>,
    /// When the program in the foreground started, and what it is.
    command_since: Option<Instant>,
    last_command: String,
    pub last_notified: Option<Instant>,
    /// The Claude Code or Codex conversation in it, to resume if sasha
    /// stops; or the one it's resuming, since then.
    conversation: Option<Conversation>,
    resuming_since: Option<Instant>,
    /// The Codex it last searched the logs for, and when.
    codex_lookup: Option<(i32, Instant)>,
    /// The flags its agent was started with that a restart keeps, for the
    /// process group it runs as.
    agent_flags: Option<(i32, Vec<String>)>,
    /// The version installed in place of the agent it runs, when that's
    /// another; and which run it was looked up for, when.
    newer: Option<String>,
    newer_checked: Option<(i32, Instant)>,
    /// Whether its agent waited for you, as its log said, and when it was
    /// read.
    waits: Option<(Instant, bool)>,
    /// Its agent being restarted on its conversation.
    restart: Option<Restart>,
    /// What came of that, to say.
    restarted: Option<String>,
}

/// Restarting a tab's agent: it's told to stop, once its turn is over,
/// and started again in the same shell on the same conversation.
struct Restart {
    agent: Agent,
    /// The process group it runs as.
    group: i32,
    /// What the shell is given to start it again.
    command: String,
    conversation: Option<Conversation>,
    /// Waiting for its turn to end first.
    wait: bool,
    /// When it was told to stop, and whether it had to be made to.
    stopping: Option<(Instant, bool)>,
}

impl Tab {
    /// Starts the user's shell in a new tab, or `program` (its arguments)
    /// in its place.
    pub fn spawn(
        id: TabId,
        (rows, cols): (u16, u16),
        cwd: Option<&Path>,
        program: Option<Vec<OsString>>,
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("failed to open a pty")?;

        let mut cmd = program.map_or_else(CommandBuilder::new_default_prog, CommandBuilder::from_argv);
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // We emulate an xterm, not whatever terminal sasha itself runs in.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "sasha");
        // Lets sasha refuse to attach from inside itself.
        if let Ok(socket) = crate::protocol::socket_path() {
            cmd.env("SASHA", socket);
        }
        // So hooks of the agents in it can say which terminal they're in.
        cmd.env("SASHA_TAB", id.to_string());
        for var in ["TERM_PROGRAM_VERSION", "TERMINFO", "TERMCAP"] {
            cmd.env_remove(var);
        }

        let child = pair.slave.spawn_command(cmd).context("failed to spawn shell")?;
        drop(pair.slave);
        let input = start_io(id, pair.master.as_ref(), events)?;
        let shell_pid = child.process_id().map(|pid| pid as i32);
        let parser = vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK_LINES, Callbacks::default());
        let mut tab = Self::with(id, parser, pair.master, input, child, shell_pid, cwd.map(Path::to_path_buf));
        tab.refresh_info();
        Ok(tab)
    }

    /// Takes over a terminal the sasha before handed over, still running,
    /// showing again what `screen` (from `dump`) draws.
    pub fn adopt(handed: HandedTab, screen: &[u8], groups: &[GroupId], events: Sender<AppEvent>) -> Result<Self> {
        // SAFETY: the descriptor was handed over for this terminal alone.
        let fd = unsafe { OwnedFd::from_raw_fd(handed.fd) };
        // Not into the programs this sasha starts.
        handoff::keep_on_exec(handed.fd, false)?;
        let master: Box<dyn MasterPty + Send> = Box::new(HandedPty(fd));
        let input = start_io(handed.id, master.as_ref(), events)?;
        let (rows, cols) = handed.size;
        let callbacks = Callbacks { title: handed.title, cursor_style: handed.cursor_style, ..Callbacks::default() };
        let mut parser = vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK_LINES, callbacks);
        parser.process(screen);
        // Drawing it again isn't news, nor a question to answer.
        let callbacks = parser.callbacks_mut();
        callbacks.replies.clear();
        (callbacks.bell, callbacks.title_changed, callbacks.clipboard, callbacks.notice) = (false, false, None, None);
        let child = Box::new(HandedChild::new(handed.pid));
        let mut tab = Self::with(handed.id, parser, master, input, child, handed.shell_pid, handed.cwd);
        tab.custom_prefix = handed.custom_prefix;
        tab.pinned = handed.pinned;
        tab.group = handed.group.and_then(|g| groups.get(g).copied());
        tab.group_before_pin = handed.group_before_pin.and_then(|g| groups.get(g).copied());
        tab.mission = handed.mission;
        tab.mailbox = handed.mailbox;
        tab.conversation = handed.conversation;
        tab.refresh_info();
        tab.title_owner = handed.title_owner;
        Ok(tab)
    }

    /// The terminal, for the next sasha to take over: `None` without a pty
    /// to hand. Groups by their place in `groups`.
    pub fn handed(&self, groups: &[GroupId]) -> Option<HandedTab> {
        let place = |group: Option<GroupId>| group.and_then(|id| groups.iter().position(|&g| g == id));
        Some(HandedTab {
            id: self.id,
            fd: self.master.as_raw_fd()?,
            pid: self.child.process_id()?,
            shell_pid: self.shell_pid,
            size: self.size(),
            title: self.parser.callbacks().title.clone(),
            title_owner: self.title_owner,
            cursor_style: self.cursor_style(),
            custom_prefix: self.custom_prefix.clone(),
            pinned: self.pinned,
            group: place(self.group),
            group_before_pin: place(self.group_before_pin),
            mission: self.mission.clone(),
            mailbox: self.mailbox.clone(),
            cwd: self.cwd.clone(),
            conversation: self.conversation.clone(),
        })
    }

    /// What the terminal shows, and what has scrolled off its top, as the
    /// bytes that draw it all again in a new emulator of its size.
    pub fn dump(&mut self) -> Vec<u8> {
        // A full-screen program draws on the alternate screen: that, after
        // the screen underneath.
        let alternate = self.screen().alternate_screen().then(|| self.screen().state_formatted());
        if alternate.is_some() {
            self.parser.process(b"\x1b[?1049l");
        }
        let screen = self.parser.screen_mut();
        let (rows, cols) = screen.size();
        let mut out = Vec::new();
        // Each line that scrolled off, oldest first, pushed up by the next.
        screen.set_scrollback(usize::MAX);
        let mut offset = screen.scrollback();
        let lines = offset;
        while offset > 0 {
            screen.set_scrollback(offset);
            for row in screen.rows_formatted(0, cols).take(offset.min(usize::from(rows))) {
                out.extend_from_slice(&row);
                out.extend_from_slice(b"\x1b[m\r\n");
            }
            offset = offset.saturating_sub(usize::from(rows));
        }
        screen.set_scrollback(0);
        // Enough more to push the last of them off the screen.
        if lines > 0 {
            for _ in 1..rows {
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(&screen.state_formatted());
        if let Some(alternate) = alternate {
            out.extend_from_slice(b"\x1b[?1049h");
            out.extend_from_slice(&alternate);
            // And back, should the update not happen.
            self.parser.process(b"\x1b[?1049h");
            self.parser.process(&alternate);
        }
        out
    }

    /// The Claude Code or Codex conversation in it, to resume if sasha
    /// stops.
    pub fn conversation(&self) -> Option<&Conversation> {
        self.conversation.as_ref()
    }

    /// Picks `conversation` up again: types the command that resumes it.
    /// Returns whether it could.
    pub fn resume(&mut self, conversation: Conversation) -> bool {
        let Some(command) = conversation.resume_command() else { return false };
        // The shell reads it once it's up.
        self.write(format!("{command}\r"));
        self.conversation = Some(conversation);
        self.resuming_since = Some(Instant::now());
        true
    }

    fn with(
        id: TabId,
        parser: vt100::Parser<Callbacks>,
        master: Box<dyn MasterPty + Send>,
        input: Sender<Vec<u8>>,
        child: Box<dyn Child + Send + Sync>,
        shell_pid: Option<i32>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            id,
            parser,
            master,
            input,
            child,
            shell_pid,
            custom_prefix: None,
            pinned: false,
            group: None,
            group_before_pin: None,
            mission: None,
            mailbox: Vec::new(),
            fg_name: String::new(),
            fg_pgid: shell_pid,
            title_owner: None,
            cwd,
            last_activity: Instant::now(),
            settle_until: Instant::now() + Duration::from_secs(2),
            unseen: false,
            bell: false,
            rang: false,
            notice: None,
            finished: None,
            streak_start: None,
            command_since: None,
            last_command: String::new(),
            last_notified: None,
            conversation: None,
            resuming_since: None,
            codex_lookup: None,
            agent_flags: None,
            newer: None,
            newer_checked: None,
            waits: None,
            restart: None,
            restarted: None,
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn screen_mut(&mut self) -> &mut vt100::Screen {
        self.parser.screen_mut()
    }

    pub fn size(&self) -> (u16, u16) {
        self.screen().size()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if self.size() == (rows, cols) {
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.settle_until = self.settle_until.max(Instant::now() + Duration::from_millis(500));
    }

    /// Output right after startup or a resize is just the program drawing.
    pub fn settling(&self) -> bool {
        Instant::now() < self.settle_until
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }

    /// Types `text` the way a terminal sends a paste: line breaks as
    /// carriage returns, bracketed when the program asked for that.
    pub fn paste(&self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.screen().bracketed_paste() {
            self.write(format!("\x1b[200~{text}\x1b[201~"));
        } else {
            self.write(text);
        }
    }

    /// The last `lines` lines of text: the screen, and the scrollback above
    /// it when that's asked for. Rows a long line wrapped onto join it
    /// again, and trailing blank lines are left out.
    pub fn text(&mut self, lines: usize) -> String {
        let screen = self.parser.screen_mut();
        let (rows, cols) = screen.size();
        let viewed = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let mut offset = lines.saturating_sub(usize::from(rows)).min(screen.scrollback());
        // Each row, and whether it runs on into the next.
        let mut text: Vec<(String, bool)> = Vec::new();
        let mut take_rows = |screen: &vt100::Screen, take: usize| {
            for (row, line) in (0..).zip(screen.rows(0, cols).take(take)) {
                text.push((line, screen.row_wrapped(row)));
            }
        };
        // The view `offset` lines up starts at the line wanted next.
        while offset > 0 {
            screen.set_scrollback(offset);
            let take = offset.min(usize::from(rows));
            take_rows(screen, take);
            offset -= take;
        }
        screen.set_scrollback(0);
        take_rows(screen, usize::from(rows));
        screen.set_scrollback(viewed);
        let mut joined = String::new();
        for (line, wrapped) in text {
            if wrapped {
                joined.push_str(&line);
            } else {
                joined.push_str(line.trim_end());
                joined.push('\n');
            }
        }
        joined.trim_end().to_owned()
    }

    /// Feeds child output into the emulator. Returns an OSC 52 clipboard
    /// payload (base64) if the child asked to set the clipboard.
    pub fn process(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.parser.process(bytes);

        let cb = self.parser.callbacks_mut();
        if !cb.replies.is_empty() {
            let replies = std::mem::take(&mut cb.replies);
            let _ = self.input.send(replies);
        }
        let bell = std::mem::take(&mut cb.bell);
        let title_changed = std::mem::take(&mut cb.title_changed);
        let clipboard = cb.clipboard.take();
        if let Some(notice) = cb.notice.take() {
            self.notice = Some(notice);
        }
        self.bell |= bell;
        self.rang |= bell;
        if title_changed {
            self.title_owner = self.master.process_group_leader();
        }
        clipboard
    }

    pub fn cursor_style(&self) -> u16 {
        self.parser.callbacks().cursor_style
    }

    /// Re-reads the foreground process and the shell's working directory.
    pub fn refresh_info(&mut self) {
        self.fg_pgid = self.master.process_group_leader().or(self.shell_pid);
        if let Some(name) = self.fg_pgid.and_then(procinfo::name) {
            self.fg_name = name;
        }
        if let Some(cwd) = self.shell_pid.and_then(procinfo::cwd) {
            self.cwd = Some(cwd);
        }
        // Notice long commands finishing.
        match (self.is_running_command(), self.command_since) {
            (true, None) => self.command_since = Some(Instant::now()),
            (false, Some(since)) => {
                self.command_since = None;
                if since.elapsed() >= LONG_COMMAND {
                    self.finished = Some((self.last_command.clone(), since.elapsed()));
                }
            }
            _ => {}
        }
        if self.is_running_command() {
            self.last_command.clone_from(&self.fg_name);
        }
        self.drive_restart();
        self.refresh_conversation();
    }

    /// Follows the conversation of the agent in the foreground. Claude Code
    /// says which it's having, so that's read each time (`/resume` switches
    /// it); Codex's logs are searched once a run, and again now and then
    /// until it's found.
    fn refresh_conversation(&mut self) {
        let (Some(agent), Some(group)) = (self.agent(), self.fg_pgid) else {
            self.newer = None;
            // The agent has left, unless it's still to start.
            if self.resuming_since.is_none_or(|since| since.elapsed() >= RESUME_WAIT) {
                self.conversation = None;
                self.resuming_since = None;
            }
            return;
        };
        self.resuming_since = None;
        if self.newer_checked.is_none_or(|(checked, at)| checked != group || at.elapsed() >= VERSION_CHECK) {
            self.newer = agents::newer_version(agent, group);
            self.newer_checked = Some((group, Instant::now()));
        }
        if agent == Agent::Codex {
            let searched = self.codex_lookup.filter(|&(searched, _)| searched == group);
            if searched.is_some_and(|(_, at)| self.conversation.is_some() || at.elapsed() < CODEX_LOOKUP) {
                return;
            }
            self.codex_lookup = Some((group, Instant::now()));
        }
        let cwd = self.program_cwd().unwrap_or_default();
        if let Some(mut conversation) = Conversation::of(agent, group, &cwd) {
            conversation.flags = self.flags_of(agent, group);
            self.conversation = Some(conversation);
        }
    }

    /// The flags the agent running as `group` keeps when it's started
    /// again; its arguments are read once a run.
    fn flags_of(&mut self, agent: Agent, group: i32) -> Vec<String> {
        if let Some((read, flags)) = &self.agent_flags
            && *read == group
        {
            return flags.clone();
        }
        let flags = procinfo::args(group).map(|args| agents::kept_flags(agent, &args)).unwrap_or_default();
        self.agent_flags = Some((group, flags.clone()));
        flags
    }

    /// The agent in the foreground, if one is.
    pub fn agent(&self) -> Option<Agent> {
        if self.is_running_command() { Agent::of(&self.fg_name) } else { None }
    }

    /// The version installed in place of the agent it runs, when that's
    /// another (it's been updated since it started).
    pub fn newer_agent(&self) -> Option<&str> {
        self.newer.as_deref()
    }

    /// Whether its agent is in the middle of a turn, or waits for you to
    /// allow something. Claude Code's title starts ◐ or ◑ while it works
    /// (a braille spinner, before) and ✳ once it's done; otherwise an agent
    /// is working while it draws.
    pub fn agent_busy(&mut self) -> bool {
        let Some(agent) = self.agent() else { return false };
        let title = self.running();
        let working = match title.as_deref().and_then(|title| split_glyph(title).0) {
            Some(glyph) if agent == Agent::Claude => {
                glyph.starts_with(|c| matches!(c, '◐' | '◑') || ('\u{2800}'..='\u{28ff}').contains(&c))
            }
            _ => self.last_activity.elapsed() < TURN_OVER,
        };
        if working {
            return true;
        }
        if let Some((read, waits)) = self.waits
            && read.elapsed() < WAITS_CHECK
        {
            return waits;
        }
        let waits = agents::waits_for_you(agent, self.conversation.as_ref(), &self.program_cwd().unwrap_or_default());
        self.waits = Some((Instant::now(), waits));
        waits
    }

    /// Restarts the agent in the foreground, on its conversation and with
    /// the flags it was started with: at once, or once its turn is over
    /// (`wait`). False when there's no agent, or no way to start it again.
    pub fn restart_agent(&mut self, wait: bool) -> bool {
        let (Some(agent), Some(group)) = (self.agent(), self.fg_pgid) else { return false };
        if let Some(restart) = &mut self.restart
            && restart.group == group
        {
            restart.wait &= wait;
        } else {
            let flags = self.flags_of(agent, group);
            let conversation = self.conversation.clone().map(|conversation| Conversation { flags: flags.clone(), ..conversation });
            let command = match &conversation {
                Some(conversation) => conversation.resume_command(),
                None => agents::start_command(agent, &flags),
            };
            let Some(command) = command else { return false };
            self.restart = Some(Restart { agent, group, command, conversation, wait, stopping: None });
        }
        self.drive_restart();
        true
    }

    /// Whether its agent is to restart once its turn is over.
    pub fn restart_waiting(&self) -> bool {
        self.restart.as_ref().is_some_and(|r| r.wait && r.stopping.is_none())
    }

    /// Forgets a restart that's still waiting for the turn to end.
    pub fn cancel_restart(&mut self) {
        if self.restart_waiting() {
            self.restart = None;
        }
    }

    /// What came of a restart since the last call.
    pub fn take_restarted(&mut self) -> Option<String> {
        self.restarted.take()
    }

    /// Moves a restart along: tells the agent to stop once it may, makes it
    /// if it doesn't, and once the shell has the terminal again, starts it.
    fn drive_restart(&mut self) {
        let Some(restart) = &self.restart else { return };
        let (agent, group, wait, stopping) = (restart.agent, restart.group, restart.wait, restart.stopping);
        let name = agent.name();
        let here = self.fg_pgid == Some(group);
        match stopping {
            // It left by itself.
            None if !here => self.restart = None,
            None if wait && self.agent_busy() => {}
            None => {
                if self.signal_group(group, libc::SIGTERM) {
                    self.set_stopping(Instant::now(), false);
                } else {
                    self.restart = None;
                    self.restarted = Some(format!("can't stop {name}"));
                }
            }
            // The shell's back: it starts the agent again.
            Some(_) if !self.is_running_command() => {
                let Some(restart) = self.restart.take() else { return };
                self.write(format!("{}\r", restart.command));
                self.restarted = Some(match restart.conversation {
                    Some(conversation) => {
                        self.conversation = Some(conversation);
                        self.resuming_since = Some(Instant::now());
                        format!("restarted {name} on the same conversation")
                    }
                    None => format!("restarted {name}"),
                });
            }
            Some((since, false)) if here && since.elapsed() >= STOP_WAIT => {
                self.set_stopping(since, true);
                self.signal_group(group, libc::SIGKILL);
            }
            Some((since, _)) if since.elapsed() >= STOP_WAIT + KILL_WAIT => {
                self.restart = None;
                self.restarted = Some(format!("{name} didn't stop, so it wasn't restarted"));
            }
            Some(_) => {}
        }
    }

    fn set_stopping(&mut self, since: Instant, made_to: bool) {
        if let Some(restart) = &mut self.restart {
            restart.stopping = Some((since, made_to));
        }
    }

    /// Sends `signal` to process group `group`: never the shell's, nor
    /// sasha's own.
    fn signal_group(&self, group: i32, signal: libc::c_int) -> bool {
        // SAFETY: getpgrp has no preconditions.
        let own = unsafe { libc::getpgrp() };
        if group <= 1 || Some(group) == self.shell_pid || group == own {
            return false;
        }
        // SAFETY: kill has no memory-safety preconditions.
        unsafe { libc::kill(-group, signal) == 0 }
    }

    /// Records output that is news (not the shell starting or redrawing).
    pub fn note_output(&mut self) {
        let now = Instant::now();
        if self.streak_start.is_none() || now - self.last_activity > QUIET {
            self.streak_start = Some(now);
        }
        self.last_activity = now;
    }

    /// What's worth a notification since the last call, if anything:
    /// the program's own message, a long command finishing, a bell, or a
    /// long stretch of output coming to an end.
    pub fn take_news(&mut self) -> Option<String> {
        let notice = self.notice.take();
        let finished = self.finished.take();
        let rang = std::mem::take(&mut self.rang);
        let mut worked = None;
        if let Some(start) = self.streak_start
            && self.last_activity.elapsed() >= QUIET
        {
            self.streak_start = None;
            worked = Some(self.last_activity - start).filter(|w| *w >= LONG_WORK);
        }
        notice
            .or_else(|| finished.map(|(name, took)| format!("{name} finished after {}", notify::duration(took))))
            .or_else(|| rang.then(|| "wants your attention".to_owned()))
            .or_else(|| worked.map(|w| format!("went quiet after {} of output", notify::duration(w))))
    }

    pub fn shell_pid(&self) -> Option<i32> {
        self.shell_pid
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Where the program in the foreground runs, else the shell's folder.
    pub fn program_cwd(&self) -> Option<PathBuf> {
        self.fg_pgid.and_then(procinfo::cwd).or_else(|| self.cwd.clone())
    }

    pub fn process_name(&self) -> &str {
        &self.fg_name
    }

    /// True while something other than the shell owns the terminal.
    pub fn is_running_command(&self) -> bool {
        self.fg_pgid.is_some() && self.fg_pgid != self.shell_pid
    }

    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// "prefix · what's running", or just the prefix while the shell is idle.
    pub fn title(&self) -> String {
        let prefix = self.prefix();
        match self.running() {
            Some(running) => format!("{prefix} · {running}"),
            None => prefix,
        }
    }

    /// The name the user gave the tab, else the folder it's in.
    pub fn prefix(&self) -> String {
        if let Some(prefix) = &self.custom_prefix {
            return prefix.clone();
        }
        match &self.cwd {
            Some(cwd) => short_dir(cwd),
            None if !self.fg_name.is_empty() => self.fg_name.clone(),
            None => "shell".to_owned(),
        }
    }

    /// The program in the foreground, when it isn't the shell: its own title
    /// (vim, ssh and claude set good ones) or else its name.
    pub fn running(&self) -> Option<String> {
        if !self.is_running_command() {
            return None;
        }
        // Only trust a title while the program that set it is still in the
        // foreground.
        let title = self.parser.callbacks().title.as_ref();
        match title.filter(|t| !t.trim().is_empty() && self.title_owner == self.fg_pgid) {
            Some(title) => Some(title.clone()),
            None => Some(self.fg_name.clone()),
        }
    }
}

/// Reads what the program writes, as events, until it's gone; returns
/// where to send what's typed, which a thread writes so a program that
/// stops reading can't freeze sasha.
fn start_io(id: TabId, master: &(dyn MasterPty + Send), events: Sender<AppEvent>) -> Result<Sender<Vec<u8>>> {
    let mut reader = master.try_clone_reader()?;
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if events.send(AppEvent::Output(id, buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // EIO once the shell is gone.
                Err(_) => break,
            }
        }
        let _ = events.send(AppEvent::Exited(id));
    });
    let mut writer = master.take_writer()?;
    let (input, input_rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        for bytes in input_rx {
            if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                break;
            }
        }
    });
    Ok(input)
}

impl Drop for Tab {
    fn drop(&mut self) {
        // Hang up like a real terminal does when its window closes.
        for pid in [self.fg_pgid.map(|pgid| -pgid), self.shell_pid].into_iter().flatten() {
            // SAFETY: kill has no memory-safety preconditions.
            unsafe {
                libc::kill(pid, libc::SIGHUP);
            }
        }
    }
}

/// Splits off the status glyph a program puts at the start of its title,
/// like claude's ✳ and spinner or codex's ◐: one symbol, then a space.
pub fn split_glyph(title: &str) -> (Option<&str>, &str) {
    let Some((glyph, rest)) = title.split_once(' ') else { return (None, title) };
    let mut chars = glyph.chars();
    let symbol = chars.next().is_some_and(|c| c == '*' || (!c.is_ascii() && !c.is_alphanumeric()));
    // A symbol, maybe with a variation selector, one or two columns wide.
    let fits = chars.count() <= 1 && (1..=2).contains(&glyph.width());
    if symbol && fits { (Some(glyph), rest.trim_start()) } else { (None, title) }
}

fn short_dir(path: &Path) -> String {
    if std::env::home_dir().is_some_and(|home| home == path) {
        return "~".to_owned();
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::split_glyph;

    #[test]
    fn splits_a_status_glyph_off_titles() {
        assert_eq!(split_glyph("✳ Fix the build"), (Some("✳"), "Fix the build"));
        assert_eq!(split_glyph("⠐ Fix the build"), (Some("⠐"), "Fix the build"));
        assert_eq!(split_glyph("◐ Docker services startup"), (Some("◐"), "Docker services startup"));
        assert_eq!(split_glyph("* Claude Code"), (Some("*"), "Claude Code"));
        assert_eq!(split_glyph("✳️ Task"), (Some("✳️"), "Task"));
        // Titles that start with a word, a path or ASCII punctuation stay.
        assert_eq!(split_glyph("main.rs (~/src) - NVIM"), (None, "main.rs (~/src) - NVIM"));
        assert_eq!(split_glyph("~/Work: zsh"), (None, "~/Work: zsh"));
        assert_eq!(split_glyph("- dashes"), (None, "- dashes"));
        assert_eq!(split_glyph("日本 text"), (None, "日本 text"));
        assert_eq!(split_glyph("htop"), (None, "htop"));
    }
}
