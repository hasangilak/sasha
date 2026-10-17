//! How a sasha client talks to the sasha server over a Unix socket.
//!
//! Every message is a frame: a tag byte, a big-endian u32 length, then the
//! payload. Clients send JSON `ClientMessage`s; the server sends the bytes
//! to draw, and finally why the client should exit.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use crossterm::event::Event;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::control::Request;
use crate::theme::Rgb;

/// Bumped whenever the messages change.
pub const VERSION: u32 = 1;

/// Server → client: bytes for the client's terminal.
pub const OUTPUT: u8 = 0;
/// Server → client: the client should exit, printing the payload.
pub const EXIT: u8 = 1;
/// Client → server: a JSON `ClientMessage`.
const MESSAGE: u8 = 2;
/// Server → client: the JSON `control::Reply` to a request.
pub const REPLY: u8 = 3;

const MAX_FRAME: usize = 64 << 20;

#[derive(Debug, Serialize, Deserialize)]
pub enum ClientMessage {
    /// The first message of a client that wants to see the tabs.
    Attach(Hello),
    Input(Event),
    /// Close every tab and stop the server.
    Kill,
    /// A question or an order from another program, answered with a reply.
    Control(Request),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// Identifies the client's sasha binary, to notice a rebuilt one.
    pub build: u64,
    pub config: Config,
    /// Columns and rows of the client's terminal.
    pub size: (u16, u16),
    /// The client terminal's foreground and background colours.
    pub colors: (Option<Rgb>, Option<Rgb>),
    /// Where sasha was started.
    pub cwd: PathBuf,
}

pub fn write_frame(out: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| io::Error::other("frame too large"))?;
    let mut header = [tag, 0, 0, 0, 0];
    header[1..].copy_from_slice(&len.to_be_bytes());
    out.write_all(&header)?;
    out.write_all(payload)?;
    out.flush()
}

pub fn read_frame(input: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    input.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::other("frame too large"));
    }
    let mut payload = vec![0; len];
    input.read_exact(&mut payload)?;
    Ok((header[0], payload))
}

pub fn send(out: &mut impl Write, message: &ClientMessage) -> io::Result<()> {
    write_frame(out, MESSAGE, &serde_json::to_vec(message)?)
}

pub fn receive(input: &mut impl Read) -> io::Result<ClientMessage> {
    match read_frame(input)? {
        (MESSAGE, payload) => Ok(serde_json::from_slice(&payload)?),
        (tag, _) => Err(io::Error::other(format!("unexpected frame {tag}"))),
    }
}

/// `$SASHA_SOCKET`, else `/tmp/sasha-<uid>/default`, like tmux. The
/// default folder is created private to the user, and refused if anyone
/// else could reach it: whoever can connect can type into the shells.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SASHA_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/sasha-{uid}"));
    ensure_private_dir(&dir, uid)?;
    Ok(dir.join("default"))
}

fn ensure_private_dir(dir: &Path, uid: libc::uid_t) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err).with_context(|| format!("can't create {}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(dir).with_context(|| format!("can't read {}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        bail!("{} must be a folder only you can access (chmod 700)", dir.display());
    }
    Ok(())
}

/// A fingerprint of the running executable, so a client can tell that the
/// server is an older (or newer) build of sasha.
pub fn build_id() -> u64 {
    let Ok(bytes) = std::env::current_exe().and_then(std::fs::read) else { return 0 };
    // FNV-1a over 8-byte words: fast enough for a few MB.
    bytes
        .chunks(8)
        .map(|chunk| chunk.iter().rev().fold(0u64, |n, &b| n << 8 | u64::from(b)))
        .fold(0xcbf29ce484222325, |h, word| (h ^ word).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, OUTPUT, b"\x1b[2J").unwrap();
        send(&mut wire, &ClientMessage::Kill).unwrap();
        let mut input = wire.as_slice();
        assert_eq!(read_frame(&mut input).unwrap(), (OUTPUT, b"\x1b[2J".to_vec()));
        assert!(matches!(receive(&mut input).unwrap(), ClientMessage::Kill));
        assert!(read_frame(&mut input).is_err());
    }
}
