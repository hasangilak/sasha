use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::input;

const USAGE: &str = "\
sasha — a terminal with Chrome-style vertical tabs

USAGE:
    sasha [--prefix KEY]

OPTIONS:
    -p, --prefix KEY   Key that enters tab mode, e.g. ctrl-b (default), ctrl-a, ctrl-space.
                       Also read from $SASHA_PREFIX.
    -h, --help         Show this help.
    -V, --version      Show the version.

Press the prefix, then ? inside sasha to see every key binding.";

pub struct Config {
    pub prefix: Prefix,
}

impl Config {
    /// Parses the command line. Returns `None` when sasha should exit
    /// right away (after printing help or the version).
    pub fn from_args() -> Result<Option<Self>> {
        let mut prefix = std::env::var("SASHA_PREFIX").ok();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    println!("{USAGE}");
                    return Ok(None);
                }
                "-V" | "--version" => {
                    println!("sasha {}", env!("CARGO_PKG_VERSION"));
                    return Ok(None);
                }
                "-p" | "--prefix" => match args.next() {
                    Some(value) => prefix = Some(value),
                    None => bail!("--prefix needs a key, e.g. --prefix ctrl-a"),
                },
                other => match other.strip_prefix("--prefix=") {
                    Some(value) => prefix = Some(value.to_owned()),
                    None => bail!("unknown argument `{other}`\n\n{USAGE}"),
                },
            }
        }
        let prefix = match prefix {
            Some(spec) => Prefix::parse(&spec)?,
            None => Prefix('b'),
        };
        Ok(Some(Self { prefix }))
    }
}

/// A Ctrl+<key> chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefix(char);

impl Prefix {
    pub fn parse(spec: &str) -> Result<Self> {
        let lower = spec.trim().to_ascii_lowercase();
        let key = ["ctrl-", "ctrl+", "c-", "^"]
            .iter()
            .find_map(|p| lower.strip_prefix(p))
            .ok_or_else(|| anyhow::anyhow!("prefix `{spec}` must be a ctrl chord, e.g. ctrl-b"))?;
        let c = match key {
            "space" => ' ',
            _ if key.chars().count() == 1 => key.chars().next().unwrap_or_default(),
            _ => bail!("unsupported prefix key `{spec}`"),
        };
        if input::control_byte(c).is_none() {
            bail!("ctrl-{c} can't be typed in a terminal; try ctrl-b or ctrl-a");
        }
        Ok(Self(c))
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&self.0))
    }

    /// The byte the shell would have received for this chord.
    pub fn byte(&self) -> u8 {
        input::control_byte(self.0).unwrap_or(0)
    }

    pub fn label(&self) -> String {
        match self.0 {
            ' ' => "^Space".to_owned(),
            c => format!("^{}", c.to_ascii_uppercase()),
        }
    }
}
