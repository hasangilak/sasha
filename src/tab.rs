//! A tab is one shell running in its own pseudo-terminal, plus the vt100
//! emulator state that mirrors what the shell has drawn.

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use unicode_width::UnicodeWidthStr;

use crate::app::AppEvent;
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
        for var in ["TERM_PROGRAM_VERSION", "TERMINFO", "TERMCAP"] {
            cmd.env_remove(var);
        }

        let child = pair.slave.spawn_command(cmd).context("failed to spawn shell")?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let output_events = events.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output_events.send(AppEvent::Output(id, buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    // EIO once the shell is gone.
                    Err(_) => break,
                }
            }
            let _ = output_events.send(AppEvent::Exited(id));
        });

        // Writes go through a thread so a child that stops reading can't freeze the UI.
        let mut writer = pair.master.take_writer()?;
        let (input, input_rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            for bytes in input_rx {
                if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                    break;
                }
            }
        });

        let shell_pid = child.process_id().map(|pid| pid as i32);
        let mut tab = Self {
            id,
            parser: vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK_LINES, Callbacks::default()),
            master: pair.master,
            input,
            child,
            shell_pid,
            custom_prefix: None,
            pinned: false,
            group: None,
            group_before_pin: None,
            fg_name: String::new(),
            fg_pgid: shell_pid,
            title_owner: None,
            cwd: cwd.map(Path::to_path_buf),
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
        };
        tab.refresh_info();
        Ok(tab)
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
