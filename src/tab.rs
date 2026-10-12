//! A tab is one shell running in its own pseudo-terminal, plus the vt100
//! emulator state that mirrors what the shell has drawn.

use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::app::AppEvent;
use crate::procinfo;

pub type TabId = u64;

const SCROLLBACK_LINES: usize = 5_000;

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
    /// Name given by the user; overrides every automatic title.
    pub custom_title: Option<String>,
    /// Name of the process in the foreground of this tab's tty.
    fg_name: String,
    fg_pgid: Option<i32>,
    /// Foreground process group at the time the OSC title was set.
    title_owner: Option<i32>,
    cwd: Option<PathBuf>,
    pub last_output: Instant,
    /// Output arrived while the tab was in the background.
    pub unseen: bool,
    pub bell: bool,
}

impl Tab {
    pub fn spawn(
        id: TabId,
        (rows, cols): (u16, u16),
        cwd: Option<&Path>,
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("failed to open a pty")?;

        let mut cmd = CommandBuilder::new_default_prog();
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // We emulate an xterm, not whatever terminal sasha itself runs in.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "sasha");
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
            custom_title: None,
            fg_name: String::new(),
            fg_pgid: shell_pid,
            title_owner: None,
            cwd: cwd.map(Path::to_path_buf),
            last_output: Instant::now(),
            unseen: false,
            bell: false,
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
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }

    /// Feeds child output into the emulator. Returns an OSC 52 clipboard
    /// payload (base64) if the child asked to set the clipboard.
    pub fn process(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.parser.process(bytes);
        self.last_output = Instant::now();

        let cb = self.parser.callbacks_mut();
        if !cb.replies.is_empty() {
            let replies = std::mem::take(&mut cb.replies);
            let _ = self.input.send(replies);
        }
        let bell = std::mem::take(&mut cb.bell);
        let title_changed = std::mem::take(&mut cb.title_changed);
        let clipboard = cb.clipboard.take();
        self.bell |= bell;
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

    pub fn title(&self) -> String {
        if let Some(title) = &self.custom_title {
            return title.clone();
        }
        // Programs like vim or ssh set meaningful titles; only trust one while
        // the program that set it is still in the foreground.
        if let Some(title) = &self.parser.callbacks().title
            && self.is_running_command()
            && self.title_owner == self.fg_pgid
        {
            return title.clone();
        }
        let dir = self.cwd.as_deref().map(short_dir);
        match (self.is_running_command(), dir) {
            (true, Some(dir)) => format!("{} · {dir}", self.fg_name),
            (false, Some(dir)) => dir,
            (_, None) if !self.fg_name.is_empty() => self.fg_name.clone(),
            _ => "shell".to_owned(),
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

fn short_dir(path: &Path) -> String {
    if std::env::home_dir().is_some_and(|home| home == path) {
        return "~".to_owned();
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}
