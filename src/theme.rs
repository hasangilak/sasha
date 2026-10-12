//! Chrome's dark vertical-tabs palette.

use ratatui::style::Color;

pub const ACTIVITY_BG: Color = rgb(0x17, 0x17, 0x19);
pub const SIDEBAR_BG: Color = rgb(0x20, 0x21, 0x24);
pub const HOVER_BG: Color = rgb(0x2d, 0x2e, 0x31);
pub const ACTIVE_BG: Color = rgb(0x3c, 0x40, 0x43);
pub const BUTTON_BG: Color = rgb(0x35, 0x36, 0x3a);
pub const BUTTON_HOVER_BG: Color = rgb(0x45, 0x47, 0x4b);
pub const BORDER: Color = rgb(0x3c, 0x40, 0x43);
pub const TEXT: Color = rgb(0xe8, 0xea, 0xed);
pub const TEXT_DIM: Color = rgb(0x9a, 0xa0, 0xa6);
pub const ACCENT: Color = rgb(0x8a, 0xb4, 0xf8);
pub const ON_ACCENT: Color = rgb(0x20, 0x21, 0x24);

pub const GREEN: Color = rgb(0x81, 0xc9, 0x95);
pub const BLUE: Color = rgb(0x8a, 0xb4, 0xf8);
pub const ORANGE: Color = rgb(0xfc, 0xad, 0x70);
pub const PURPLE: Color = rgb(0xc5, 0x8a, 0xf9);
pub const YELLOW: Color = rgb(0xfd, 0xd6, 0x63);
pub const CYAN: Color = rgb(0x78, 0xd9, 0xec);
pub const RED: Color = rgb(0xf2, 0x8b, 0x82);
pub const PINK: Color = rgb(0xff, 0x8b, 0xcb);

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Mixes `from` towards `to`; `t` = 0.0 is `from`, 1.0 is `to`.
pub fn blend(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => from,
    }
}

/// A favicon stand-in: glyph and colour for the program running in a tab.
pub fn process_icon(name: &str) -> (&'static str, Color) {
    let name = name.to_ascii_lowercase();
    let is = |names: &[&str]| names.iter().any(|n| name == *n);
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));

    if is(&["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "nu", "elvish", "xonsh", "pwsh"]) {
        ("❯", GREEN)
    } else if is(&["vim", "nvim", "vi", "hx", "helix", "nano", "emacs", "micro", "kak", "zed"]) {
        ("✎", BLUE)
    } else if is(&["git", "lazygit", "tig", "gitui", "gh"]) {
        ("±", ORANGE)
    } else if is(&["ssh", "mosh", "mosh-client", "et", "telnet"]) {
        ("⇄", PURPLE)
    } else if is(&["top", "htop", "btop", "btm", "glances", "k9s"]) {
        ("◔", CYAN)
    } else if is(&["claude", "codex", "aider"]) {
        ("✻", ORANGE)
    } else if is(&["man", "less", "more", "bat", "tail"]) {
        ("≡", TEXT_DIM)
    } else if starts(&["python", "node", "bun", "deno", "ruby", "cargo", "rustc", "go", "make", "npm"])
        || is(&["pnpm", "yarn", "java", "gradle", "mvn", "docker", "kubectl", "swift", "xcodebuild"])
    {
        ("▸", YELLOW)
    } else {
        ("●", TEXT_DIM)
    }
}
