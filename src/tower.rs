//! `sasha tower`: pi, the coding-agent harness, set up as a control tower
//! over sasha's terminals: on your Claude subscription, or your ChatGPT one
//! through Codex. It's what the tower's terminal runs.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::store;

const EXTENSION: &str = include_str!("tower/extension.ts");
const PROMPT: &str = include_str!("tower/prompt.md");
/// pi's npm package.
const PACKAGE: &str = "@earendil-works/pi-coding-agent";

/// The subscription the tower thinks with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Claude,
    Codex,
}

impl Engine {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" | "anthropic" => Some(Engine::Claude),
            "codex" | "openai" | "chatgpt" => Some(Engine::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Engine::Claude => "claude",
            Engine::Codex => "codex",
        }
    }

    /// As people say it.
    pub fn label(self) -> &'static str {
        match self {
            Engine::Claude => "Claude",
            Engine::Codex => "Codex",
        }
    }

    /// The model pi runs, as `provider/id`: `$SASHA_TOWER_CLAUDE_MODEL` or
    /// `$SASHA_TOWER_CODEX_MODEL`, else the strongest at the time of writing.
    fn model(self) -> String {
        let (var, default) = match self {
            Engine::Claude => ("SASHA_TOWER_CLAUDE_MODEL", "anthropic/claude-opus-5-5"),
            Engine::Codex => ("SASHA_TOWER_CODEX_MODEL", "openai-codex/gpt-6.1-sol"),
        };
        std::env::var(var).ok().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| default.to_owned())
    }
}

/// Where the tower keeps its files, beside sasha's state:
/// `~/.config/sasha/tower`.
pub fn dir() -> Option<PathBuf> {
    Some(store::path()?.parent()?.join("tower"))
}

/// Starts pi as the tower, on `engine` or else the one it last ran on
/// (Claude the first time). Doesn't return unless pi can't start.
pub fn run(engine: Option<Engine>) -> Result<()> {
    let dir = dir().context("no home folder to keep the tower's files in")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
    let extension = dir.join("extension.ts");
    let prompt = dir.join("prompt.md");
    std::fs::write(&extension, EXTENSION)?;
    std::fs::write(&prompt, PROMPT)?;
    let chosen = dir.join("engine");
    let engine = match engine {
        Some(engine) => {
            std::fs::write(&chosen, format!("{}\n", engine.name()))?;
            engine
        }
        None => std::fs::read_to_string(&chosen).ok().and_then(|name| Engine::parse(&name)).unwrap_or(Engine::Claude),
    };
    let Some(pi) = find_pi().or_else(install) else { return Ok(()) };

    let mut command = Command::new(&pi);
    command
        .current_dir(&dir)
        .arg("--extension")
        .arg(&extension)
        .arg("--append-system-prompt")
        .arg(&prompt)
        .args(["--model", &engine.model(), "--continue"])
        .env("SASHA_EXE", std::env::current_exe().context("can't find the sasha executable")?)
        .env("SASHA_TOWER_DIR", &dir)
        .env("SASHA_TOWER_CLAUDE_MODEL", Engine::Claude.model())
        .env("SASHA_TOWER_CODEX_MODEL", Engine::Codex.model());
    // pi is a node script: the node it was installed with sits beside it.
    if let Some(bin) = pi.parent() {
        command.env("PATH", with_first(bin));
    }
    let err = command.exec();
    bail!("can't start {}: {err}", pi.display())
}

/// `$PATH` with `dir` first.
fn with_first(dir: &Path) -> OsString {
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&rest).filter(|p| p != dir));
    std::env::join_paths(paths).unwrap_or(rest)
}

/// `$SASHA_PI`, else `pi` on the `$PATH`, else where npm, bun, volta and
/// Homebrew put it.
fn find_pi() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SASHA_PI") {
        return Some(PathBuf::from(path));
    }
    find_program("pi")
}

fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if let Some(home) = std::env::home_dir() {
        // The newest node nvm has.
        let mut nodes: Vec<PathBuf> = std::fs::read_dir(home.join(".nvm/versions/node"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("bin"))
            .collect();
        nodes.sort_by_key(|bin| version_of(bin));
        dirs.extend(nodes.into_iter().rev());
        for sub in [".local/bin", ".npm-global/bin", ".bun/bin", ".volta/bin"] {
            dirs.push(home.join(sub));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.into_iter().map(|dir| dir.join(name)).find(|path| is_executable(path))
}

/// `[24, 19, 0]` for `…/node/v24.19.0/bin`.
fn version_of(bin: &Path) -> Vec<u32> {
    let name = bin.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.trim_start_matches('v').split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// pi isn't installed: offers to install it with npm, and returns where it
/// went.
fn install() -> Option<PathBuf> {
    println!("The tower runs pi, the coding-agent harness from pi.dev, and pi isn't installed.\n");
    let Some(npm) = find_program("npm") else {
        println!("Install Node.js 22 or newer (nodejs.org), then:\n\n    npm install -g {PACKAGE}\n");
        pause("Then open the tower again.");
        return None;
    };
    print!("Install it now with `npm install -g {PACKAGE}`? [Y/n] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    let _ = io::stdin().lock().read_line(&mut answer);
    if answer.trim().to_ascii_lowercase().starts_with('n') {
        pause("Install it whenever you like, then open the tower again.");
        return None;
    }
    let mut command = Command::new(&npm);
    command.args(["install", "-g", PACKAGE]);
    if let Some(bin) = npm.parent() {
        command.env("PATH", with_first(bin));
    }
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(_) | Err(_) => {
            pause("npm couldn't install it; the message above says why.");
            return None;
        }
    }
    // npm puts it beside itself.
    let installed = npm.with_file_name("pi");
    if is_executable(&installed) { Some(installed) } else { find_pi() }
}

fn pause(message: &str) {
    println!("{message}\n\nPress Enter to close.");
    let _ = io::stdin().lock().read_line(&mut String::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_go_by_their_names() {
        assert_eq!(Engine::parse("Claude\n"), Some(Engine::Claude));
        assert_eq!(Engine::parse("codex"), Some(Engine::Codex));
        assert_eq!(Engine::parse("chatgpt"), Some(Engine::Codex));
        assert_eq!(Engine::parse("gemini"), None);
        assert!(Engine::Claude.model().starts_with("anthropic/"));
        assert!(Engine::Codex.model().starts_with("openai-codex/"));
    }

    #[test]
    fn node_versions_sort_by_number() {
        let mut bins = ["v9.1.0", "v24.19.0", "v22.3.1"].map(|v| PathBuf::from(format!("/n/{v}/bin")));
        bins.sort_by_key(|bin| version_of(bin));
        assert_eq!(bins[2], PathBuf::from("/n/v24.19.0/bin"));
    }
}
