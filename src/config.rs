use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::input;
use crate::tower::Engine;

const USAGE: &str = "\
sasha — a terminal with Chrome-style vertical tabs

USAGE:
    sasha [--prefix KEY]
    sasha tower [claude|codex]
    sasha mcp

Your tabs live in a background server: closing the window (or detaching
with the prefix, then d) leaves them running, and running sasha again
brings them back.

OPTIONS:
    -p, --prefix KEY   Key that enters tab mode, e.g. ctrl-b (default), ctrl-a, ctrl-space.
                       Also read from $SASHA_PREFIX.
        --kill-server  Close every tab and stop the background server.
    -h, --help         Show this help.
    -V, --version      Show the version.

COMMANDS:
    tower              Run pi as the control tower over your terminals, on your
                       Claude subscription (default) or Codex. The tower's tab
                       (prefix, then T) runs this.
    mcp                Serve the terminals as tools over MCP on stdin/stdout:
                       list, read, prompt, wait for, open and close them.

Press the prefix, then ? inside sasha to see every key binding.";

/// What this run of sasha does.
pub enum Command {
    /// Show the tabs, starting the server first if it isn't running.
    Attach(Config),
    /// Be the server (clients start it).
    Server,
    KillServer,
    /// Be the tower: pi, steering the terminals.
    Tower(Option<Engine>),
    /// Serve the terminals as MCP tools.
    Mcp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub prefix: Prefix,
}

impl Config {
    /// Parses the command line. Returns `None` when sasha should exit
    /// right away (after printing help or the version).
    pub fn from_args() -> Result<Option<Command>> {
        let mut prefix = std::env::var("SASHA_PREFIX").ok();
        let mut args = std::env::args().skip(1).peekable();
        match args.peek().map(String::as_str) {
            Some("mcp") => return Ok(Some(Command::Mcp)),
            Some("tower") => {
                args.next();
                let engine = match args.next() {
                    Some(name) => Some(Engine::parse(&name).ok_or_else(|| anyhow::anyhow!("the tower runs on claude or codex, not `{name}`"))?),
                    None => None,
                };
                return Ok(Some(Command::Tower(engine)));
            }
            _ => {}
        }
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => return Ok(Some(Command::Server)),
                "--kill-server" => return Ok(Some(Command::KillServer)),
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
        Ok(Some(Command::Attach(Self { prefix })))
    }
}

/// A Ctrl+<key> chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
