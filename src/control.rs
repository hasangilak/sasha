//! What other programs may ask the running sasha: the terminals open, what
//! they show, typing into them, and opening, showing and closing them. The
//! tower's tools (`sasha mcp`) are built on it.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::mission::{Ended, Mission, Signal, Update};
use crate::protocol::{self, ClientMessage};
use crate::tab::TabId;

/// A request's answer comes back within this, or something is stuck.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// How many times a request dropped by an update is asked again, a fifth
/// of a second apart.
const UPDATE_WAIT: usize = 25;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    /// Every terminal that's running.
    List,
    /// What a terminal shows, with the scrollback above it up to `lines`
    /// lines in all.
    Read { id: TabId, lines: usize },
    /// Text into a terminal, the way a paste arrives. From the tower, it's
    /// a task: the terminal becomes one of its sessions.
    Paste {
        id: TabId,
        text: String,
        #[serde(default)]
        tower: bool,
    },
    /// Keys by name: `enter`, `esc`, `ctrl-c`, `up`, `y`…
    Keys { id: TabId, keys: Vec<String> },
    /// A new terminal in `cwd`, called `name`, with `command` typed into its
    /// shell. It opens without taking the screen. From the tower, it's one
    /// of its sessions, set to do `task`.
    Open {
        cwd: PathBuf,
        name: Option<String>,
        command: Option<String>,
        #[serde(default)]
        tower: bool,
        #[serde(default)]
        task: Option<String>,
    },
    /// Brings a terminal on screen.
    Show { id: TabId },
    /// Names a terminal (empty: back to its folder's name).
    Rename { id: TabId, name: String },
    /// Closes a terminal, hanging up what runs in it.
    Close { id: TabId },
    /// What an agent's hooks say about its terminal.
    Signal { id: TabId, signal: Signal },
    /// A message from the session in terminal `from` to the one called
    /// `to` (a name, `#id`, or `tower`), delivered when it's free.
    Message { from: Option<TabId>, to: String, text: String },
    /// The news for the tower about its sessions (`ids`, else all), taken
    /// off the pile with `take`.
    Inbox {
        ids: Option<Vec<TabId>>,
        take: bool,
    },
    /// Update to the sasha on disk, keeping every terminal running.
    Update,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The terminals, and the tower's sessions that ended.
    Terminals(Vec<Terminal>, Vec<Ended>),
    Updates(Vec<Update>),
    Text(String),
    Opened(TabId),
    Done,
    Error(String),
}

/// A terminal, as the tower sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Terminal {
    pub id: TabId,
    /// The name the user gave it, else its folder's.
    pub name: String,
    pub workspace: String,
    pub group: Option<String>,
    /// The program in the foreground; empty while the shell waits at its
    /// prompt.
    pub program: String,
    /// What that program says it's doing (its title), if it says.
    pub title: Option<String>,
    /// Where the program runs, else the shell's folder.
    pub cwd: Option<PathBuf>,
    /// Since it last printed anything.
    pub quiet_ms: u64,
    /// The one the user is looking at.
    pub on_screen: bool,
    /// Printed since the user last looked, or rang the bell.
    pub unseen: bool,
    pub bell: bool,
    /// What the tower set it to do, if it's one of the tower's sessions.
    pub mission: Option<Mission>,
}

/// Asks the running sasha. A sasha updating in place drops what it was
/// asked: it's asked again, of the new one.
pub fn call(request: Request) -> Result<Reply> {
    let path = socket()?;
    let mut answer = ask(&path, &request);
    for _ in 0..UPDATE_WAIT {
        if !matches!(&answer, Err(err) if err.kind() == io::ErrorKind::UnexpectedEof) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
        answer = ask(&path, &request);
    }
    let (tag, payload) = answer.map_err(|err| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            anyhow!("sasha isn't running (no server at {})", path.display())
        }
        _ => anyhow!("the sasha running is older and doesn't take this: update it (prefix, then U)"),
    })?;
    if tag != protocol::REPLY {
        bail!("unexpected answer from sasha");
    }
    match serde_json::from_slice(&payload)? {
        Reply::Error(err) => bail!(err),
        reply => Ok(reply),
    }
}

/// One request, on a connection of its own: the reply's frame.
fn ask(path: &Path, request: &Request) -> io::Result<(u8, Vec<u8>)> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    protocol::send(&mut stream, &ClientMessage::Control(request.clone()))?;
    protocol::read_frame(&mut stream)
}

/// The server of the sasha this runs in (its terminals say in `$SASHA`),
/// else the one sasha would start.
fn socket() -> Result<PathBuf> {
    if std::env::var_os("SASHA_SOCKET").is_none()
        && let Some(path) = std::env::var_os("SASHA")
    {
        return Ok(PathBuf::from(path));
    }
    protocol::socket_path()
}

pub fn terminals() -> Result<Vec<Terminal>> {
    Ok(everything()?.0)
}

/// The terminals, and the tower's sessions that ended.
pub fn everything() -> Result<(Vec<Terminal>, Vec<Ended>)> {
    match call(Request::List)? {
        Reply::Terminals(terminals, ended) => Ok((terminals, ended)),
        _ => bail!("unexpected answer from sasha"),
    }
}

pub fn updates(ids: Option<Vec<TabId>>, take: bool) -> Result<Vec<Update>> {
    match call(Request::Inbox { ids, take })? {
        Reply::Updates(updates) => Ok(updates),
        _ => bail!("unexpected answer from sasha"),
    }
}

pub fn text(request: Request) -> Result<String> {
    match call(request)? {
        Reply::Text(text) => Ok(text),
        _ => bail!("unexpected answer from sasha"),
    }
}
