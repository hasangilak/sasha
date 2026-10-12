//! Translates crossterm key / mouse events back into the byte sequences an
//! xterm would send to the program running inside a tab.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

pub fn encode_key(key: &KeyEvent, application_cursor: bool) -> Vec<u8> {
    let mods = key.modifiers;
    let alt = mods.contains(KeyModifiers::ALT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    // xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    let param = 1
        + u8::from(mods.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(alt)
        + 4 * u8::from(ctrl);

    let with_alt = |mut bytes: Vec<u8>| {
        if alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    // Cursor-style keys: ESC [ A, ESC O A in application mode, ESC [ 1;5 A with modifiers.
    let cursor_key = |c: char, app: bool| {
        if param > 1 {
            format!("\x1b[1;{param}{c}").into_bytes()
        } else if app {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde_key = |n: u8| {
        if param > 1 {
            format!("\x1b[{n};{param}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    match key.code {
        KeyCode::Char(c) if ctrl => with_alt(match control_byte(c) {
            Some(b) => vec![b],
            None => c.to_string().into_bytes(),
        }),
        KeyCode::Char(c) => with_alt(c.to_string().into_bytes()),
        KeyCode::Enter => with_alt(vec![b'\r']),
        KeyCode::Tab => with_alt(vec![b'\t']),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if ctrl => with_alt(vec![0x08]),
        KeyCode::Backspace => with_alt(vec![0x7f]),
        KeyCode::Esc => with_alt(vec![0x1b]),
        KeyCode::Up => cursor_key('A', application_cursor),
        KeyCode::Down => cursor_key('B', application_cursor),
        KeyCode::Right => cursor_key('C', application_cursor),
        KeyCode::Left => cursor_key('D', application_cursor),
        KeyCode::Home => cursor_key('H', application_cursor),
        KeyCode::End => cursor_key('F', application_cursor),
        KeyCode::Insert => tilde_key(2),
        KeyCode::Delete => tilde_key(3),
        KeyCode::PageUp => tilde_key(5),
        KeyCode::PageDown => tilde_key(6),
        KeyCode::F(n @ 1..=4) => cursor_key(['P', 'Q', 'R', 'S'][n as usize - 1], true),
        KeyCode::F(n @ 5..=12) => tilde_key([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        _ => Vec::new(),
    }
}

/// The C0 control byte for Ctrl+<c>, following xterm's legacy mapping.
pub fn control_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Encodes a mouse event for a child that enabled mouse reporting.
/// `col` / `row` are zero-based and relative to the tab's screen.
pub fn encode_mouse(
    event: &MouseEvent,
    col: u16,
    row: u16,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    // (button code, is release)
    let (mut code, release): (u8, bool) = match event.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => {
            if mode == MouseProtocolMode::Press {
                return None;
            }
            (button_code(b), true)
        }
        MouseEventKind::Drag(b) => {
            if !matches!(mode, MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion) {
                return None;
            }
            (button_code(b) + 32, false)
        }
        MouseEventKind::Moved => {
            if mode != MouseProtocolMode::AnyMotion {
                return None;
            }
            (3 + 32, false)
        }
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }

    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    match encoding {
        MouseProtocolEncoding::Sgr => {
            let end = if release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
        }
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            // Legacy encodings can't say which button was released.
            let code = if release { 3 | (code & !3) } else { code };
            let mut out = b"\x1b[M".to_vec();
            for v in [u32::from(code), x, y] {
                let v = v + 32;
                if encoding == MouseProtocolEncoding::Utf8 {
                    out.extend_from_slice(char::from_u32(v)?.to_string().as_bytes());
                } else {
                    out.push(u8::try_from(v).ok()?);
                }
            }
            Some(out)
        }
    }
}
