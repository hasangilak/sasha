//! Updating sasha without closing anything. The server hands every
//! terminal, the window showing them and its socket to the new sasha,
//! which takes over its process (exec): the same process, so the programs
//! in the terminals, its children, never notice. What's on each screen
//! goes along, scrollback and all.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

use crate::agents::Conversation;
use crate::config::Config;
use crate::mission::{Ended, Mission, Update};
use crate::server::ClientId;
use crate::store::{SavedLayout, SavedWorkspace};
use crate::tab::TabId;
use crate::theme::Rgb;

/// Bumped when the new sasha couldn't read what an older one hands over.
pub const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct Handoff {
    pub version: u32,
    /// The socket clients connect to, still listening.
    pub listener: RawFd,
    /// The window showing the tabs, if one is.
    pub client: Option<HandedClient>,
    pub layout: SavedLayout,
    pub groups: Vec<HandedGroup>,
    pub workspaces: Vec<HandedWorkspace>,
    pub current: usize,
    pub tower: Option<HandedTab>,
    pub tower_focus: bool,
    pub tower_inbox: Vec<Update>,
    pub ended_missions: Vec<Ended>,
    pub keep_awake: bool,
    pub next_tab_id: TabId,
}

#[derive(Serialize, Deserialize)]
pub struct HandedClient {
    pub id: ClientId,
    pub fd: RawFd,
    pub size: (u16, u16),
    pub config: Config,
    pub colors: (Option<Rgb>, Option<Rgb>),
}

#[derive(Serialize, Deserialize)]
pub struct HandedGroup {
    pub name: String,
    pub color: usize,
    pub collapsed: bool,
    pub last_active: Option<TabId>,
}

#[derive(Serialize, Deserialize)]
pub struct HandedWorkspace {
    pub path: PathBuf,
    pub name: Option<String>,
    pub tabs: Vec<HandedTab>,
    pub active: usize,
    pub previous: Option<TabId>,
    /// Its tabs not reopened yet.
    pub dormant: Option<SavedWorkspace>,
    pub section_hidden: bool,
}

/// A running terminal: its pty, the program in it, and what sasha knows of
/// it. Groups are by their place in the list.
#[derive(Serialize, Deserialize)]
pub struct HandedTab {
    pub id: TabId,
    pub fd: RawFd,
    pub pid: u32,
    pub shell_pid: Option<i32>,
    pub size: (u16, u16),
    pub title: Option<String>,
    /// The process group that set the title.
    pub title_owner: Option<i32>,
    pub cursor_style: u16,
    pub custom_prefix: Option<String>,
    pub pinned: bool,
    pub group: Option<usize>,
    pub group_before_pin: Option<usize>,
    pub mission: Option<Mission>,
    pub mailbox: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub conversation: Option<Conversation>,
}

impl Handoff {
    /// Every descriptor that has to live through the exec.
    pub fn fds(&self) -> Vec<RawFd> {
        let tabs = self.workspaces.iter().flat_map(|ws| &ws.tabs).chain(&self.tower);
        let mut fds: Vec<RawFd> = tabs.map(|tab| tab.fd).collect();
        fds.push(self.listener);
        fds.extend(self.client.as_ref().map(|client| client.fd));
        fds
    }

    /// Writes it, and each terminal's screen, into `dir` (made private);
    /// returns the file to hand the new sasha.
    pub fn write(&self, dir: &Path, screens: &[(TabId, Vec<u8>)]) -> Result<PathBuf> {
        let _ = fs::remove_dir_all(dir);
        fs::DirBuilder::new().mode(0o700).create(dir).with_context(|| format!("can't create {}", dir.display()))?;
        for (id, screen) in screens {
            private_file(&dir.join(format!("{id}.screen")))?.write_all(screen)?;
        }
        let path = dir.join("handoff.json");
        private_file(&path)?.write_all(&serde_json::to_vec(self)?)?;
        Ok(path)
    }

    /// Reads what the sasha before handed over, and each terminal's screen.
    pub fn read(path: &Path) -> Result<(Self, HashMap<TabId, Vec<u8>>)> {
        let handoff: Handoff = serde_json::from_slice(&fs::read(path)?).context("can't read what was handed over")?;
        if handoff.version != VERSION {
            bail!("handed over by a sasha this one can't take over from (version {})", handoff.version);
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        let tabs = handoff.workspaces.iter().flat_map(|ws| &ws.tabs).chain(&handoff.tower);
        let screens = tabs.filter_map(|tab| Some((tab.id, fs::read(dir.join(format!("{}.screen", tab.id))).ok()?))).collect();
        Ok((handoff, screens))
    }
}

fn private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

/// Lets `fd` live through an exec, or stops it.
pub fn keep_on_exec(fd: RawFd, keep: bool) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor number has no memory-safety preconditions.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if keep { flags & !libc::FD_CLOEXEC } else { flags | libc::FD_CLOEXEC };
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The pty of a terminal the sasha before started.
pub struct HandedPty(pub OwnedFd);

impl MasterPty for HandedPty {
    fn resize(&self, size: PtySize) -> Result<()> {
        let size = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };
        // SAFETY: TIOCSWINSZ reads a winsize, which we pass.
        if unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCSWINSZ, &size) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize> {
        // SAFETY: winsize is plain old data, all-zero is a valid value.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ writes a winsize, which we pass.
        if unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(PtySize { rows: size.ws_row, cols: size.ws_col, pixel_width: size.ws_xpixel, pixel_height: size.ws_ypixel })
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(fs::File::from(self.0.try_clone()?)))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        Ok(Box::new(fs::File::from(self.0.try_clone()?)))
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        // SAFETY: tcgetpgrp on a descriptor has no memory-safety preconditions.
        match unsafe { libc::tcgetpgrp(self.0.as_raw_fd()) } {
            pid if pid > 0 => Some(pid),
            _ => None,
        }
    }

    fn as_raw_fd(&self) -> Option<RawFd> {
        Some(self.0.as_raw_fd())
    }

    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// The program the sasha before started in a terminal: still this
/// process's child.
#[derive(Debug)]
pub struct HandedChild {
    pub pid: u32,
    status: Option<ExitStatus>,
}

impl HandedChild {
    pub fn new(pid: u32) -> Self {
        HandedChild { pid, status: None }
    }

    fn reaped(&mut self, status: libc::c_int) -> ExitStatus {
        let status = if libc::WIFSIGNALED(status) {
            ExitStatus::with_signal(&format!("signal {}", libc::WTERMSIG(status)))
        } else {
            ExitStatus::with_exit_code(libc::WEXITSTATUS(status) as u32)
        };
        self.status = Some(status.clone());
        status
    }
}

impl ChildKiller for HandedChild {
    fn kill(&mut self) -> io::Result<()> {
        // A hangup, as portable-pty sends its own children.
        // SAFETY: kill has no memory-safety preconditions.
        if unsafe { libc::kill(self.pid as i32, libc::SIGHUP) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(HandedChild::new(self.pid))
    }
}

impl Child for HandedChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = &self.status {
            return Ok(Some(status.clone()));
        }
        let mut status = 0;
        // SAFETY: waitpid writes the status we pass.
        match unsafe { libc::waitpid(self.pid as i32, &mut status, libc::WNOHANG) } {
            0 => Ok(None),
            -1 => {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ECHILD) {
                    return Err(err);
                }
                // Not a child to wait for: gone once there's no such process.
                // SAFETY: kill with signal 0 only checks.
                let alive = unsafe { libc::kill(self.pid as i32, 0) } == 0;
                Ok((!alive).then(|| {
                    let status = ExitStatus::with_exit_code(0);
                    self.status = Some(status.clone());
                    status
                }))
            }
            _ => Ok(Some(self.reaped(status))),
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = &self.status {
            return Ok(status.clone());
        }
        let mut status = 0;
        // SAFETY: waitpid writes the status we pass.
        if unsafe { libc::waitpid(self.pid as i32, &mut status, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(self.reaped(status))
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }
}
