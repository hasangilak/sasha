//! Copying to the system clipboard: native tools first, OSC 52 through the
//! host terminal as a fallback.

use std::io::Write;
use std::process::{Command, Stdio};

const TOOLS: &[(&str, &[&str])] = &[
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// Copies `text`; returns an OSC 52 sequence to send to the terminal sasha
/// runs in when no clipboard tool is around to do it.
#[must_use]
pub fn copy(text: &str) -> Option<String> {
    for (program, args) in TOOLS {
        if pipe_to(program, args, text) {
            return None;
        }
    }
    Some(format!("\x1b]52;c;{}\x07", encode_base64(text.as_bytes())))
}

/// Handles an OSC 52 request from a program (payload already base64).
#[must_use]
pub fn copy_base64(data: &[u8]) -> Option<String> {
    let text = decode_base64(data).and_then(|bytes| String::from_utf8(bytes).ok())?;
    copy(&text)
}

fn pipe_to(program: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().is_ok_and(|status| status.success())
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn decode_base64(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in data.iter().filter(|c| !c.is_ascii_whitespace() && **c != b'=') {
        let value = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = acc << 6 | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for text in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "héllo ✓"] {
            let encoded = encode_base64(text.as_bytes());
            assert_eq!(decode_base64(encoded.as_bytes()).unwrap(), text.as_bytes());
        }
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
    }
}
