//! What other programs may ask the running sasha: the terminals open, what
//! they show, typing into them, and opening, showing and closing them. The
//! tower's tools (`sasha mcp`) are built on it.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::protocol::{self, ClientMessage};
use crate::tab::TabId;

/// A request's answer comes back within this, or something is stuck.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Every terminal that's running.
    List,
    /// What a terminal shows, with the scrollback above it up to `lines`
    /// lines in all.
    Read { id: TabId, lines: usize },
    /// Text into a terminal, the way a paste arrives.
    Paste { id: TabId, text: String },
    /// Keys by name: `enter`, `esc`, `ctrl-c`, `up`, `y`…
    Keys { id: TabId, keys: Vec<String> },
    /// A new terminal in `cwd`, called `name`, with `command` typed into its
    /// shell. It opens without taking the screen.
    Open { cwd: PathBuf, name: Option<String>, command: Option<String> },
    /// Brings a terminal on screen.
    Show { id: TabId },
    /// Closes a terminal, hanging up what runs in it.
    Close { id: TabId },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Terminals(Vec<Terminal>),
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
}

/// Asks the running sasha.
pub fn call(request: Request) -> Result<Reply> {
    let path = socket()?;
    let mut stream =
        UnixStream::connect(&path).with_context(|| format!("sasha isn't running (no server at {})", path.display()))?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    protocol::send(&mut stream, &ClientMessage::Control(request))?;
    let (tag, payload) = protocol::read_frame(&mut stream)
        .map_err(|_| anyhow!("the sasha running doesn't take requests: quit it (prefix, q q) and start it again"))?;
    if tag != protocol::REPLY {
        bail!("unexpected answer from sasha");
    }
    match serde_json::from_slice(&payload)? {
        Reply::Error(err) => bail!(err),
        reply => Ok(reply),
    }
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
    match call(Request::List)? {
        Reply::Terminals(terminals) => Ok(terminals),
        _ => bail!("unexpected answer from sasha"),
    }
}

pub fn text(request: Request) -> Result<String> {
    match call(request)? {
        Reply::Text(text) => Ok(text),
        _ => bail!("unexpected answer from sasha"),
    }
}
