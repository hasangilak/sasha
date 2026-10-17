//! `sasha tower`: an agent set up as the control tower over sasha's
//! terminals, with them as its tools (`sasha mcp`). On Claude it's Claude
//! Code itself, so it runs on your Claude plan; on Codex it's pi, the
//! coding-agent harness, signed in to your ChatGPT plan. It's what the
//! tower's panel runs.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::{agents, hooks, store};

const EXTENSION: &str = include_str!("tower/extension.ts");
const PROMPT: &str = include_str!("tower/prompt.md");
/// What `role.md` starts as: how to use it, in a comment.
const ROLE_TEMPLATE: &str = include_str!("tower/role.md");
/// On Claude Code, the tower hands each session to a subagent: how it does
/// that, and the subagent's own role.
const DELEGATE: &str = include_str!("tower/delegate.md");
const SESSION_HANDLER: &str = include_str!("tower/session-handler.md");
/// How long Claude Code lets a tool call go without word: waits are long.
const CLAUDE_TOOL_IDLE: &str = "3700000";
/// pi's npm package.
const PACKAGE: &str = "@earendil-works/pi-coding-agent";
/// The tower's tools Claude Code may use without asking each time: all but
/// closing a terminal.
const CLAUDE_ALLOWED: &str = "mcp__sasha__list_terminals,mcp__sasha__read_terminal,mcp__sasha__agent_report,\
mcp__sasha__send_prompt,mcp__sasha__send_keys,mcp__sasha__wait_for_terminal,mcp__sasha__open_terminal,\
mcp__sasha__show_terminal,mcp__sasha__wait_for_any,mcp__sasha__rename_terminal,mcp__sasha__speak";
/// How long Claude Code lets a tool call run, in ms: waits take a while.
const CLAUDE_TOOL_TIMEOUT: &str = "3700000";

/// What the tower thinks with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Claude Code, on your Claude plan.
    Claude,
    /// pi, on your ChatGPT plan through Codex.
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
}

/// `$SASHA_TOWER_CLAUDE_MODEL`: Claude Code's own default otherwise.
fn claude_model() -> Option<String> {
    let model = std::env::var("SASHA_TOWER_CLAUDE_MODEL").ok()?;
    let model = model.trim().trim_start_matches("anthropic/");
    (!model.is_empty()).then(|| model.to_owned())
}

/// The model pi runs, as `provider/id`: `$SASHA_TOWER_CODEX_MODEL`, else the
/// strongest at the time of writing.
fn codex_model() -> String {
    let model = std::env::var("SASHA_TOWER_CODEX_MODEL").ok().filter(|m| !m.trim().is_empty());
    model.unwrap_or_else(|| "openai-codex/gpt-6.1-sol".to_owned())
}

/// What the tower runs on, as it last said (Claude the first time).
pub fn current_engine() -> Engine {
    dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("engine")).ok())
        .and_then(|name| Engine::parse(&name))
        .unwrap_or(Engine::Claude)
}

/// The tower, as the sessions it starts know it.
pub fn described() -> &'static str {
    match current_engine() {
        Engine::Claude => "the Claude Code session named \"tower\"",
        Engine::Codex => "the pi session named \"tower\" (on Codex)",
    }
}

/// The tower's role on `engine`: sasha's, how it delegates where it can,
/// then yours from `role.md`.
pub fn role(engine: Engine) -> String {
    let mut role = PROMPT.to_owned();
    if engine == Engine::Claude {
        role.push('\n');
        role.push_str(DELEGATE);
    }
    let yours = dir().and_then(|dir| std::fs::read_to_string(dir.join("role.md")).ok()).map(|text| uncommented(&text));
    if let Some(yours) = yours.filter(|text| !text.is_empty()) {
        role.push_str("\n\n## From the user\n\nThe user wrote these instructions for you themselves. Where they differ from the above, follow them.\n\n");
        role.push_str(&yours);
        role.push('\n');
    }
    role
}

/// `role.md`, made from its template the first time.
pub fn role_file() -> Result<PathBuf> {
    let dir = dir().context("no home folder")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("role.md");
    if !path.exists() {
        std::fs::write(&path, ROLE_TEMPLATE)?;
    }
    Ok(path)
}

/// Markdown without its `<!-- -->` comments, trimmed.
fn uncommented(text: &str) -> String {
    let mut kept = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        kept.push_str(&rest[..start]);
        rest = rest[start..].find("-->").map_or("", |end| &rest[start + end + 3..]);
    }
    kept.push_str(rest);
    kept.trim().to_owned()
}

/// Where the tower keeps its files, beside sasha's state:
/// `~/.config/sasha/tower`.
pub fn dir() -> Option<PathBuf> {
    Some(store::path()?.parent()?.join("tower"))
}

/// Starts the tower on `engine`, else the one it ran on last (Claude the
/// first time). Doesn't return unless it can't start.
pub fn run(engine: Option<Engine>) -> Result<()> {
    let dir = dir().context("no home folder to keep the tower's files in")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
    let chosen = dir.join("engine");
    let engine = match engine {
        Some(engine) => {
            std::fs::write(&chosen, format!("{}\n", engine.name()))?;
            engine
        }
        None => current_engine(),
    };
    match engine {
        Engine::Claude => run_claude(&dir),
        Engine::Codex => run_pi(&dir),
    }
}

/// Claude Code with sasha's tools and nothing else's, the tower's
/// instructions, and its last conversation.
fn run_claude(dir: &Path) -> Result<()> {
    let Some(claude) = find_claude() else {
        println!("The tower runs on Claude through Claude Code, and it isn't installed.\n");
        println!("Install it from https://claude.com/claude-code, or run the tower on Codex:");
        println!("click the tower's name above this panel, then Run on Codex.\n");
        pause("");
        return Ok(());
    };
    let config = dir.join("mcp.json");
    std::fs::write(&config, serde_json::to_string_pretty(&mcp_config()?)?)?;
    let mut command = Command::new(&claude);
    command
        .current_dir(dir)
        .arg("--mcp-config")
        .arg(&config)
        .arg("--strict-mcp-config")
        // The sessions it starts know it by that name.
        .args(["--name", "tower"])
        // Its news goes in before each of its turns.
        .arg("--settings")
        .arg(hooks::tower_hooks()?)
        .args(["--allowedTools", CLAUDE_ALLOWED])
        .args(["--append-system-prompt", &role(Engine::Claude)])
        .args(["--agents", &subagents().to_string()])
        .env("CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT", CLAUDE_TOOL_IDLE)
        .env("MCP_TOOL_TIMEOUT", CLAUDE_TOOL_TIMEOUT);
    if let Some(model) = claude_model() {
        command.args(["--model", &model]);
    }
    if agents::claude_has_session(dir) {
        command.arg("--continue");
    }
    let err = command.exec();
    bail!("can't start {}: {err}", claude.display())
}

/// The tower's subagents on Claude Code: a session handler, which takes
/// one session's sending, waiting, routine approvals and checking off its
/// hands. It has sasha's tools but closing a terminal, and can read the
/// project to check a claim.
fn subagents() -> serde_json::Value {
    json!({
        "session-handler": {
            "description": "Handles one sasha terminal session for the tower: sends it a brief (or opens it with one), waits while it works, approves routine steps, checks the result against the brief, and returns a short report. Use one per session; run several in parallel.",
            "prompt": SESSION_HANDLER,
            "tools": ["mcp__sasha__*", "Read", "Grep", "Glob"],
            // Closing is the user's call, and talking to them the tower's.
            "disallowedTools": ["mcp__sasha__close_terminal", "mcp__sasha__speak"],
            "mcpServers": ["sasha"],
            "model": "inherit",
        }
    })
}

/// The MCP server Claude Code starts: this sasha's `sasha mcp`, told which
/// sasha to reach.
fn mcp_config() -> Result<serde_json::Value> {
    let exe = std::env::current_exe().context("can't find the sasha executable")?;
    // It's the tower asking: what it prompts becomes one of its sessions.
    let mut env = serde_json::Map::from_iter([("SASHA_TOWER".to_owned(), "1".into())]);
    for var in ["SASHA_SOCKET", "SASHA"] {
        if let Ok(value) = std::env::var(var) {
            env.insert(var.to_owned(), value.into());
        }
    }
    Ok(json!({"mcpServers": {"sasha": {"type": "stdio", "command": exe, "args": ["mcp"], "env": env}}}))
}

/// pi with the tower's extension (sasha's tools) and instructions, on
/// Codex, picking its conversation up.
fn run_pi(dir: &Path) -> Result<()> {
    let extension = dir.join("extension.ts");
    let prompt = dir.join("prompt.md");
    std::fs::write(&extension, EXTENSION)?;
    std::fs::write(&prompt, role(Engine::Codex))?;
    let Some(pi) = find_pi().or_else(install_pi) else { return Ok(()) };

    let mut command = Command::new(&pi);
    command
        .current_dir(dir)
        .arg("--extension")
        .arg(&extension)
        .arg("--append-system-prompt")
        .arg(&prompt)
        .args(["--model", &codex_model(), "--continue", "--name", "tower"])
        .env("SASHA_EXE", std::env::current_exe().context("can't find the sasha executable")?);
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

/// `$SASHA_CLAUDE`, else `claude` where it's usually installed.
fn find_claude() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SASHA_CLAUDE") {
        return Some(PathBuf::from(path));
    }
    find_program("claude").or_else(|| {
        let local = std::env::home_dir()?.join(".claude/local/claude");
        is_executable(&local).then_some(local)
    })
}

/// `$SASHA_PI`, else `pi` where it's usually installed.
fn find_pi() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SASHA_PI") {
        return Some(PathBuf::from(path));
    }
    find_program("pi")
}

/// `name` on the `$PATH`, else where npm, bun, volta and Homebrew put
/// programs.
pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
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
fn install_pi() -> Option<PathBuf> {
    println!("The tower runs on Codex through pi, the coding-agent harness from pi.dev, and pi isn't installed.\n");
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
    if !message.is_empty() {
        println!("{message}\n");
    }
    println!("Press Enter to close.");
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
        assert!(codex_model().contains('/'));
    }

    #[test]
    fn on_claude_the_tower_hands_sessions_to_handlers() {
        assert!(role(Engine::Claude).contains("session-handler"));
        assert!(!role(Engine::Codex).contains("session-handler"));
        let agents = subagents();
        let handler = &agents["session-handler"];
        assert!(handler["prompt"].as_str().is_some_and(|p| p.contains("wait_for_any")));
        assert_eq!(handler["disallowedTools"][0], "mcp__sasha__close_terminal");
    }

    #[test]
    fn your_role_is_what_you_wrote_outside_comments() {
        assert_eq!(uncommented(ROLE_TEMPLATE), "");
        assert_eq!(uncommented("<!-- how -->\nCodex reviews.\n<!-- more -->\nReport in Persian."), "Codex reviews.\n\nReport in Persian.");
        assert_eq!(uncommented("Keep it short. <!-- unfinished"), "Keep it short.");
    }

    #[test]
    fn node_versions_sort_by_number() {
        let mut bins = ["v9.1.0", "v24.19.0", "v22.3.1"].map(|v| PathBuf::from(format!("/n/{v}/bin")));
        bins.sort_by_key(|bin| version_of(bin));
        assert_eq!(bins[2], PathBuf::from("/n/v24.19.0/bin"));
    }

    #[test]
    fn claude_code_may_use_every_tool_but_closing() {
        let names: Vec<&str> = CLAUDE_ALLOWED.split(',').collect();
        assert_eq!(names.len(), 11);
        assert!(names.iter().all(|n| n.starts_with("mcp__sasha__")));
        assert!(!names.contains(&"mcp__sasha__close_terminal"));
    }
}
