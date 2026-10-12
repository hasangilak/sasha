//! Asks the terminal sasha runs in for its default foreground and background
//! colours (OSC 10 / OSC 11), so the sidebar can match them.

use std::io::Write;
use std::time::{Duration, Instant};

use crate::theme::Rgb;

/// Only reached when the terminal doesn't answer device attributes either.
const TIMEOUT: Duration = Duration::from_millis(500);

/// Returns (foreground, background). Must run in raw mode, before anything
/// else reads from the terminal.
pub fn query() -> (Option<Rgb>, Option<Rgb>) {
    let fd = libc::STDIN_FILENO;
    // SAFETY: isatty only inspects the descriptor.
    if unsafe { libc::isatty(fd) } != 1 {
        return (None, None);
    }
    // Ask for both colours, then for the primary device attributes, which
    // every terminal answers. Replies come back in order, so once that last
    // one arrives, any colour reply has too: no waiting on a timeout.
    let mut out = std::io::stdout().lock();
    if out.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c").and_then(|()| out.flush()).is_err() {
        return (None, None);
    }
    let reply = read_reply(fd);
    (parse_color(&reply, b"10"), parse_color(&reply, b"11"))
}

fn read_reply(fd: libc::c_int) -> Vec<u8> {
    let deadline = Instant::now() + TIMEOUT;
    let mut reply = Vec::new();
    while !has_device_attributes(&reply) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !wait_readable(fd, left) {
            break;
        }
        let mut chunk = [0u8; 256];
        // SAFETY: reads into a buffer of the given length.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..n as usize]);
    }
    reply
}

/// select(2) rather than poll(2), which can't wait on ttys on macOS.
fn wait_readable(fd: libc::c_int, timeout: Duration) -> bool {
    // SAFETY: the fd_set and timeval are initialised before use.
    unsafe {
        let mut set: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut set);
        libc::FD_SET(fd, &mut set);
        let mut tv = libc::timeval {
            tv_sec: timeout.as_secs() as _,
            tv_usec: timeout.subsec_micros() as _,
        };
        libc::select(fd + 1, &mut set, std::ptr::null_mut(), std::ptr::null_mut(), &mut tv) > 0
    }
}

/// Whether `reply` contains a DA1 answer: `ESC [ ? <digits;…> c`.
fn has_device_attributes(reply: &[u8]) -> bool {
    reply.windows(3).enumerate().any(|(start, w)| {
        w == b"\x1b[?"
            && reply[start + 3..]
                .iter()
                .find(|b| !(b.is_ascii_digit() || **b == b';'))
                .is_some_and(|b| *b == b'c')
    })
}

/// Finds `ESC ] <code> ; rgb:RRRR/GGGG/BBBB` (ended by BEL or ST) in `reply`.
fn parse_color(reply: &[u8], code: &[u8]) -> Option<Rgb> {
    let prefix = [b"\x1b]", code, b";"].concat();
    let start = reply.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
    let rest = &reply[start..];
    let end = rest.iter().position(|b| *b == 0x07 || *b == 0x1b).unwrap_or(rest.len());
    let value = std::str::from_utf8(&rest[..end]).ok()?;
    let channels = value.strip_prefix("rgb:").or_else(|| value.strip_prefix("rgba:"))?;
    let mut channels = channels.split('/').map(|hex| {
        // 1 to 4 hex digits per channel, scaled to 0..=255.
        let max = 16u32.checked_pow(hex.len() as u32)?.checked_sub(1).filter(|_| hex.len() <= 4)?;
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some(((value * 255 + max / 2) / max) as u8)
    });
    Some((channels.next()??, channels.next()??, channels.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies() {
        let reply = b"\x1b]10;rgb:abab/b2b2/bfbf\x1b\\\x1b]11;rgb:28/2c/34\x07\x1b[?62;22c";
        assert_eq!(parse_color(reply, b"10"), Some((0xab, 0xb2, 0xbf)));
        assert_eq!(parse_color(reply, b"11"), Some((0x28, 0x2c, 0x34)));
        assert!(has_device_attributes(reply));
        assert_eq!(parse_color(b"\x1b]11;rgb:f/0/8\x07", b"11"), Some((0xff, 0x00, 0x88)));
        assert_eq!(parse_color(b"\x1b]11;rgba:ffff/0000/0000/ffff\x07", b"11"), Some((0xff, 0, 0)));
    }

    #[test]
    fn ignores_missing_or_broken_replies() {
        assert_eq!(parse_color(b"\x1b[?62c", b"11"), None);
        assert_eq!(parse_color(b"\x1b]11;rgb:zz/00/00\x07", b"11"), None);
        assert_eq!(parse_color(b"\x1b]11;rgb:12345/0/0\x07", b"11"), None);
        assert!(!has_device_attributes(b"\x1b]11;rgb:0/0/0\x07"));
        assert!(!has_device_attributes(b"\x1b[?62"));
    }
}
