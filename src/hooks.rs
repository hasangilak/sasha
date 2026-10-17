//! How the agents the tower runs tell sasha how they're doing, and how the
//! tower hears it: `sasha event` is what Claude Code's hooks and Codex's
//! `notify` run in a session; `sasha inbox` is what the tower's own hook
//! runs as each of its turns starts.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::agents::{self, Agent};
use crate::control::{self, Request};
use crate::mission::Signal;
use crate::tab::TabId;

/// The most of a hook's input read.
const INPUT_MAX: u64 = 1 << 20;

/// `sasha event claude` (the hook's input on stdin) or `sasha event codex
/// JSON`: tells sasha what the agent in this terminal just did. Never
/// fails, so it can't get in the agent's way.
pub fn event(source: &str, payload: Option<&str>) -> Result<()> {
    let signal = match source {
        "claude" => {
            let mut input = String::new();
            let _ = std::io::stdin().take(INPUT_MAX).read_to_string(&mut input);
            serde_json::from_str(&input).ok().and_then(|input| claude_signal(&input))
        }
        "codex" => {
            let payload = payload.unwrap_or_default();
            pass_on_codex_notify(payload);
            serde_json::from_str(payload).ok().and_then(|input| codex_signal(&input))
        }
        _ => None,
    };
    let id = std::env::var("SASHA_TAB").ok().and_then(|id| id.parse::<TabId>().ok());
    if let (Some(id), Some(signal)) = (id, signal) {
        let _ = control::call(Request::Signal { id, signal });
    }
    Ok(())
}

/// What a Claude Code hook's input says: a prompt went in, the turn ended,
/// or it wants permission.
fn claude_signal(input: &Value) -> Option<Signal> {
    let log = input["transcript_path"].as_str().map(PathBuf::from);
    let report = log.as_deref().and_then(|log| Agent::Claude.report_from(log));
    match input["hook_event_name"].as_str()? {
        "UserPromptSubmit" => Some(Signal::Started { prompt: input["prompt"].as_str().unwrap_or_default().to_owned() }),
        "Stop" => {
            let summary = report.map_or_else(|| "finished its turn".to_owned(), |r| Agent::Claude.summary(&r));
            Some(Signal::Finished { summary, log })
        }
        "Notification" => {
            let message = input["message"].as_str().unwrap_or_default();
            // Waiting for a prompt after its turn isn't news: the Stop was.
            if message.contains("waiting for your input") {
                return None;
            }
            let what = report.and_then(|r| r.waiting).unwrap_or_else(|| message.to_owned());
            Some(Signal::Waiting { what, log })
        }
        _ => None,
    }
}

/// What Codex's `notify` says: a turn complete, with its last words.
fn codex_signal(input: &Value) -> Option<Signal> {
    if input["type"].as_str()? != "agent-turn-complete" {
        return None;
    }
    let said = agents::gist(input["last-assistant-message"].as_str().unwrap_or_default());
    let summary = if said.is_empty() { "finished its turn".to_owned() } else { agents::clip(&said, 220) };
    Some(Signal::Finished { summary, log: None })
}

/// Codex has room for one `notify` program: sasha's, in the sessions it
/// starts. The one in your config hears from it all the same.
fn pass_on_codex_notify(payload: &str) {
    let Some(mut program) = codex_notify() else { return };
    if program.is_empty() || std::env::current_exe().is_ok_and(|exe| Path::new(&program[0]) == exe) {
        return;
    }
    program.push(payload.to_owned());
    let _ = Command::new(&program[0])
        .args(&program[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// `notify = [...]` from Codex's config.toml, before any [section].
fn codex_notify() -> Option<Vec<String>> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| Some(std::env::home_dir()?.join(".codex")))?;
    let config = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let line = config.lines().take_while(|line| !line.trim_start().starts_with('[')).find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "notify").then(|| value.trim().to_owned())
    })?;
    serde_json::from_str(&line).ok()
}

/// `sasha inbox`: prints the tower's news, taking it off the pile, for its
/// hook to put before its next turn.
pub fn inbox() -> Result<()> {
    let Ok(updates) = control::updates(None, true) else { return Ok(()) };
    if updates.is_empty() {
        return Ok(());
    }
    println!("[sasha] News from your sessions since your last turn:");
    for update in updates {
        println!("- {}", update.text);
    }
    Ok(())
}

/// The tools a session the tower starts gets, to work with the others.
const PEER_TOOLS: &str = "mcp__sasha__list_sessions,mcp__sasha__session_report,mcp__sasha__message_session";

/// What a session the tower starts is told about the others.
fn peer_instructions(name: Option<&str>) -> String {
    let you = name.map_or_else(|| "one of the sessions".to_owned(), |name| format!("the session named \"{name}\""));
    format!(
        "You run in sasha as {you}: one of several agent sessions, each in its own terminal, coordinated by the tower. \
         Your sasha tools reach the others: list_sessions shows who's who and what they're on, session_report what one \
         has done, and message_session sends one a message by name, or the tower one (to: tower). Messages from them \
         reach you as \"[message from …]\" between your turns; answer with message_session when they ask something. \
         Message only when it helps the work: a question, a heads-up about something you changed that touches theirs, \
         a hand-off. Keep it short and specific."
    )
}

/// The short command that starts Claude Code or Codex in a session's shell,
/// called `name`, on `prompt`: `sasha agent` with what to start written
/// down beside the tower, since a long line typed before the shell is up
/// gets cut off.
pub fn agent_command(agent: Agent, name: Option<&str>, prompt: Option<&str>) -> Result<String> {
    let exe = std::env::current_exe().context("can't find the sasha executable")?;
    let dir = crate::tower::dir().context("no home folder")?.join("sessions");
    std::fs::create_dir_all(&dir)?;
    // Several can open in the same second.
    static OPENED: AtomicU64 = AtomicU64::new(0);
    let n = OPENED.fetch_add(1, Ordering::Relaxed);
    let spec = dir.join(format!("{}-{}-{n}.json", crate::mission::now(), std::process::id()));
    let spec_json = json!({"agent": agent.name().to_ascii_lowercase(), "name": name, "prompt": prompt});
    std::fs::write(&spec, spec_json.to_string())?;
    Ok(format!("{} agent {}", quote(&exe.to_string_lossy()), quote(&spec.to_string_lossy())))
}

/// `sasha agent SPEC`: starts the agent a session was opened for, with
/// its hooks telling sasha how it goes and its sasha tools reaching the
/// other sessions. Returns only if it can't start.
pub fn run_agent(spec: &Path) -> Result<()> {
    let text = std::fs::read_to_string(spec).with_context(|| format!("can't read {}", spec.display()))?;
    let _ = std::fs::remove_file(spec);
    let spec: Value = serde_json::from_str(&text)?;
    let agent = match spec["agent"].as_str() {
        Some("codex") => Agent::Codex,
        _ => Agent::Claude,
    };
    let exe = std::env::current_exe().context("can't find the sasha executable")?;
    let exe = exe.to_string_lossy().into_owned();
    let hooks = match agent {
        Agent::Claude => Some(write_settings("session-hooks.json", &session_hooks(&exe))?),
        Agent::Codex | Agent::Pi => None,
    };
    let here = Here {
        tab: std::env::var("SASHA_TAB").unwrap_or_default(),
        socket: std::env::var("SASHA_SOCKET").or_else(|_| std::env::var("SASHA")).unwrap_or_default(),
    };
    let args = agent_args(agent, &exe, hooks.as_deref(), &here, spec["name"].as_str(), spec["prompt"].as_str())?;
    // `$SASHA_CLAUDE` or `$SASHA_CODEX`, else whichever the shell finds.
    let (var, name) = if agent == Agent::Claude { ("SASHA_CLAUDE", "claude") } else { ("SASHA_CODEX", "codex") };
    let program = std::env::var_os(var).unwrap_or_else(|| name.into());
    let err = Command::new(&program).args(&args).exec();
    anyhow::bail!("can't start {}: {err}", program.to_string_lossy())
}

/// Which terminal a session runs in, and which sasha.
struct Here {
    tab: String,
    socket: String,
}

/// Claude Code's or Codex's arguments for a session the tower opened.
fn agent_args(
    agent: Agent,
    exe: &str,
    hooks: Option<&Path>,
    here: &Here,
    name: Option<&str>,
    prompt: Option<&str>,
) -> Result<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    match agent {
        Agent::Claude => {
            let hooks = hooks.context("Claude Code's hooks are missing")?;
            let env = json!({"SASHA_PEER": "1", "SASHA_TAB": here.tab, "SASHA": here.socket, "SASHA_SOCKET": here.socket});
            let peers = json!({"mcpServers": {"sasha": {"type": "stdio", "command": exe, "args": ["mcp"], "env": env}}});
            args.extend(["--settings".to_owned(), hooks.to_string_lossy().into_owned()]);
            args.extend(["--mcp-config".to_owned(), peers.to_string()]);
            args.extend(["--allowedTools".to_owned(), PEER_TOOLS.to_owned()]);
            args.extend(["--append-system-prompt".to_owned(), peer_instructions(name)]);
            if let Some(name) = name {
                args.extend(["--name".to_owned(), name.to_owned()]);
            }
        }
        Agent::Codex | Agent::Pi => {
            // TOML takes JSON's arrays and strings as they are.
            let text = |value: &str| serde_json::to_string(value);
            let env = format!(
                "{{SASHA_PEER=\"1\",SASHA_TAB={},SASHA={},SASHA_SOCKET={}}}",
                text(&here.tab)?,
                text(&here.socket)?,
                text(&here.socket)?
            );
            for setting in [
                format!("notify={}", serde_json::to_string(&[exe, "event", "codex"])?),
                format!("mcp_servers.sasha.command={}", text(exe)?),
                "mcp_servers.sasha.args=[\"mcp\"]".to_owned(),
                format!("mcp_servers.sasha.env={env}"),
                format!("developer_instructions={}", text(&peer_instructions(name))?),
            ] {
                args.extend(["-c".to_owned(), setting]);
            }
        }
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.push(prompt.to_owned());
    }
    Ok(args)
}

/// Claude Code settings for the sessions the tower starts: every prompt,
/// turn end and request for permission goes to `sasha event`.
fn session_hooks(exe: &str) -> Value {
    let hook = json!([{"hooks": [{"type": "command", "command": format!("{} event claude", quote(exe))}]}]);
    json!({"hooks": {"UserPromptSubmit": hook, "Stop": hook, "Notification": hook}})
}

/// Claude Code settings for the tower: its news goes in before each turn.
pub fn tower_hooks() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("can't find the sasha executable")?;
    let exe = quote(&exe.to_string_lossy());
    let inbox = json!([{"hooks": [{"type": "command", "command": format!("{exe} inbox")}]}]);
    // Its answer, said aloud when voice replies are on.
    let speak = json!([{"hooks": [{"type": "command", "command": format!("{exe} speak")}]}]);
    write_settings("tower-hooks.json", &json!({"hooks": {"UserPromptSubmit": inbox, "Stop": speak}}))
}

fn write_settings(name: &str, settings: &Value) -> Result<PathBuf> {
    let dir = crate::tower::dir().context("no home folder")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_string_pretty(settings)?)?;
    Ok(path)
}

/// `text` in single quotes, for the shell.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_hooks_say_what_happened() {
        let started = claude_signal(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix it"}));
        assert!(matches!(started, Some(Signal::Started { prompt }) if prompt == "fix it"));
        let stopped = claude_signal(&json!({"hook_event_name": "Stop", "transcript_path": "/nowhere.jsonl"}));
        assert!(matches!(stopped, Some(Signal::Finished { summary, .. }) if summary == "finished its turn"));
        let asks = claude_signal(&json!({"hook_event_name": "Notification", "message": "Claude needs your permission to use Bash"}));
        assert!(matches!(asks, Some(Signal::Waiting { what, .. }) if what.contains("permission")));
        // Idle after a turn isn't news.
        let idle = claude_signal(&json!({"hook_event_name": "Notification", "message": "Claude is waiting for your input"}));
        assert!(idle.is_none());
    }

    #[test]
    fn codex_says_when_a_turn_is_complete() {
        let done = codex_signal(&json!({"type": "agent-turn-complete", "last-assistant-message": "All **12** tests pass."}));
        assert!(matches!(done, Some(Signal::Finished { summary, .. }) if summary == "All 12 tests pass."));
        assert!(codex_signal(&json!({"type": "something-else"})).is_none());
    }

    #[test]
    fn sessions_get_hooks_peers_and_a_name() {
        let here = Here { tab: "7".to_owned(), socket: "/tmp/s".to_owned() };
        let hooks = Path::new("/h.json");
        let claude = agent_args(Agent::Claude, "/bin/sasha", Some(hooks), &here, Some("api-tests"), Some("run the tests")).unwrap();
        assert_eq!(claude[..2], ["--settings", "/h.json"]);
        assert!(claude[3].contains(r#""SASHA_TAB":"7""#) && claude[3].contains(r#""SASHA_PEER":"1""#));
        assert_eq!(claude[claude.len() - 3..], ["--name", "api-tests", "run the tests"]);
        let codex = agent_args(Agent::Codex, "/bin/sasha", None, &here, None, Some("review it")).unwrap();
        assert!(codex.iter().any(|a| a == r#"mcp_servers.sasha.env={SASHA_PEER="1",SASHA_TAB="7",SASHA="/tmp/s",SASHA_SOCKET="/tmp/s"}"#));
        assert!(codex.iter().any(|a| a.starts_with("notify=[")));
        assert_eq!(codex.last().map(String::as_str), Some("review it"));
    }

    #[test]
    fn quotes_survive_the_shell() {
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("a b"), "'a b'");
    }
}
