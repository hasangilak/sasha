//! Colours, in the spirit of Chrome's vertical tabs but taken from the
//! terminal sasha runs in.
//!
//! Large areas are painted with the terminal's own background (`BG`), so its
//! colour, transparency, blur or background image shows through. Everything
//! drawn on top (tab pills, buttons, popups, text) is mixed from the
//! terminal's real foreground and background colours, which `termcolors`
//! asks for at startup.

use std::sync::OnceLock;

use ratatui::style::Color;

pub type Rgb = (u8, u8, u8);

/// The terminal's default background.
pub const BG: Color = Color::Reset;

pub struct Palette {
    /// What `BG` looks like, for blending.
    base: Rgb,
    pub hover_bg: Color,
    pub active_bg: Color,
    pub button_bg: Color,
    pub button_hover_bg: Color,
    pub popup_bg: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,
    /// Text on accent-coloured badges.
    pub on_accent: Color,

    pub green: Color,
    pub blue: Color,
    pub orange: Color,
    pub purple: Color,
    pub yellow: Color,
    pub cyan: Color,
    pub red: Color,
    pub pink: Color,
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

/// Sets the palette from the terminal's colours, where it reported them.
pub fn init(fg: Option<Rgb>, bg: Option<Rgb>) {
    let _ = PALETTE.set(Palette::new(fg, bg));
}

pub fn palette() -> &'static Palette {
    PALETTE.get_or_init(|| Palette::new(None, None))
}

impl Palette {
    fn new(fg: Option<Rgb>, bg: Option<Rgb>) -> Self {
        let light = bg.map_or_else(colorfgbg_is_light, is_light);
        // Chrome's own colours stand in for whatever the terminal didn't tell us.
        let (default_fg, default_bg) =
            if light { ((0x1f, 0x1f, 0x1f), (0xff, 0xff, 0xff)) } else { ((0xe8, 0xea, 0xed), (0x20, 0x21, 0x24)) };
        let (fg, bg) = (fg.unwrap_or(default_fg), bg.unwrap_or(default_bg));
        let surface = |t: f32| mix(bg, fg, t);
        let hue = |dark: u32, light_hue: u32| hex(if light { light_hue } else { dark });
        Self {
            base: bg,
            hover_bg: surface(0.07),
            active_bg: surface(0.15),
            button_bg: surface(0.10),
            button_hover_bg: surface(0.20),
            popup_bg: surface(0.04),
            border: surface(0.15),
            text: Color::Rgb(fg.0, fg.1, fg.2),
            text_dim: surface(0.6),
            accent: hue(0x8ab4f8, 0x1a73e8),
            on_accent: Color::Rgb(bg.0, bg.1, bg.2),
            green: hue(0x81c995, 0x188038),
            blue: hue(0x8ab4f8, 0x1a73e8),
            orange: hue(0xfcad70, 0xc26401),
            purple: hue(0xc58af9, 0x9334e6),
            yellow: hue(0xfdd663, 0xb06000),
            cyan: hue(0x78d9ec, 0x007b83),
            red: hue(0xf28b82, 0xd93025),
            pink: hue(0xff8bcb, 0xd01884),
        }
    }

    /// Colours a tab group can be, in Chrome's order.
    pub fn group_colors(&self) -> [Color; 9] {
        [self.text_dim, self.blue, self.red, self.yellow, self.green, self.pink, self.purple, self.cyan, self.orange]
    }

    /// Colours a workspace badge can be.
    pub fn badge_colors(&self) -> [Color; 8] {
        [self.blue, self.green, self.orange, self.purple, self.yellow, self.cyan, self.red, self.pink]
    }
}

const fn hex(rgb: u32) -> Color {
    Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

fn mix(from: Rgb, to: Rgb, t: f32) -> Color {
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color::Rgb(channel(from.0, to.0), channel(from.1, to.1), channel(from.2, to.2))
}

fn is_light((r, g, b): Rgb) -> bool {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b) > 127.5
}

/// Some terminals describe themselves in `COLORFGBG` ("15;0" is light text
/// on a dark background); a fallback for when they don't answer queries.
fn colorfgbg_is_light() -> bool {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
        .is_some_and(|bg| matches!(bg, 7 | 9..=15))
}

/// Mixes `from` towards `to`; `t` = 0.0 is `from`, 1.0 is `to`. `BG` counts
/// as the terminal's background colour.
pub fn blend(from: Color, to: Color, t: f32) -> Color {
    let rgb = |color: Color| match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Reset => Some(palette().base),
        _ => None,
    };
    match (rgb(from), rgb(to)) {
        (Some(from), Some(to)) => mix(from, to, t),
        _ => from,
    }
}

/// A favicon stand-in: glyph and colour for the program running in a tab.
pub fn process_icon(name: &str) -> (&'static str, Color) {
    let p = palette();
    let name = name.to_ascii_lowercase();
    let is = |names: &[&str]| names.iter().any(|n| name == *n);
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));

    if is(&["zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "nu", "elvish", "xonsh", "pwsh"]) {
        ("❯", p.green)
    } else if is(&["vim", "nvim", "vi", "hx", "helix", "nano", "emacs", "micro", "kak", "zed"]) {
        ("✎", p.blue)
    } else if is(&["git", "lazygit", "tig", "gitui", "gh"]) {
        ("±", p.orange)
    } else if is(&["ssh", "mosh", "mosh-client", "et", "telnet"]) {
        ("⇄", p.purple)
    } else if is(&["top", "htop", "btop", "btm", "glances", "k9s"]) {
        ("◔", p.cyan)
    } else if is(&["claude", "codex", "aider"]) {
        ("✻", p.orange)
    } else if is(&["man", "less", "more", "bat", "tail"]) {
        ("≡", p.text_dim)
    } else if starts(&["python", "node", "bun", "deno", "ruby", "cargo", "rustc", "go", "make", "npm"])
        || is(&["pnpm", "yarn", "java", "gradle", "mvn", "docker", "kubectl", "swift", "xcodebuild"])
    {
        ("▸", p.yellow)
    } else {
        ("●", p.text_dim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_follows_the_terminal() {
        let dark = Palette::new(Some((0xab, 0xb2, 0xbf)), Some((0x28, 0x2c, 0x34)));
        assert_eq!(dark.text, Color::Rgb(0xab, 0xb2, 0xbf));
        assert_eq!(dark.on_accent, Color::Rgb(0x28, 0x2c, 0x34));
        assert_eq!(dark.accent, hex(0x8ab4f8));
        // Surfaces sit between the two, closer to the background.
        let Color::Rgb(r, _, _) = dark.active_bg else { panic!() };
        assert!(r > 0x28 && r < 0x50);

        let light = Palette::new(None, Some((0xfa, 0xfa, 0xfa)));
        assert_eq!(light.accent, hex(0x1a73e8));
        assert_eq!(light.text, Color::Rgb(0x1f, 0x1f, 0x1f));
    }
}
