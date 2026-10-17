//! `sasha mcp`: the terminals of the running sasha as tools, over the Model
//! Context Protocol on stdin and stdout — for the tower, or any agent that
//! speaks MCP. Listing them, reading them, what Claude or Codex in them
//! last did, typing prompts and keys, waiting for them to finish, and
//! opening, showing and closing them.

use std::io::{self, BufRead, Stdout, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::agents::{self, Agent};
use crate::{hooks, voice};
use crate::mission::{Ended, Mission, State};
use crate::control::{self, Reply, Request, Terminal};
use crate::notify;
use crate::tab::TabId;
use crate::workspace::{expand_path, pretty_path};

/// What's answered when the client asks for a version this doesn't know.
const PROTOCOL: &str = "2025-06-18";
/// A terminal printing within this is at work.
const BUSY: Duration = Duration::from_secs(3);
/// Between a prompt's text and the Enter that sends it, so the program
/// takes the text as typed before the key.
const BEFORE_ENTER: Duration = Duration::from_millis(300);
/// How often a wait looks again, and tells the client it's still waiting.
const POLL: Duration = Duration::from_millis(500);
const PROGRESS_EVERY: Duration = Duration::from_secs(10);
/// The longest an answer from an agent is quoted.
const ANSWER_MAX: usize = 6_000;

const INSTRUCTIONS: &str = "The terminals open in sasha. list_terminals first: ids stay the same while a terminal lives. \
agent_report says what Claude or Codex in a terminal was asked and answered; read_terminal shows the screen. \
send_prompt types and sends; wait_for_terminal waits until it's done.";

type Out = Arc<Mutex<Stdout>>;

pub fn run() -> Result<()> {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let mut calls = Vec::new();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            send(&out, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "not JSON"}}));
            continue;
        };
        // Notifications (no id) need no answer.
        let Some(id) = message.get("id").cloned() else { continue };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                let version = params["protocolVersion"].as_str().unwrap_or(PROTOCOL);
                let info = json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "sasha", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                });
                answer(&out, id, info);
            }
            "ping" => answer(&out, id, json!({})),
            "tools/list" => answer(&out, id, json!({"tools": if is_peer() { peer_tools() } else { tools() }})),
            "tools/call" => {
                // Waits take minutes: each call gets its own thread.
                let out = Arc::clone(&out);
                calls.retain(|call: &thread::JoinHandle<()>| !call.is_finished());
                calls.push(thread::spawn(move || {
                    let progress = Progress { out: Arc::clone(&out), token: params.pointer("/_meta/progressToken").cloned() };
                    let name = params["name"].as_str().unwrap_or_default();
                    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    let called = if is_peer() { peer_call(name, &args) } else { call(name, &args, &progress) };
                    let result = match called {
                        Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                        Err(err) => json!({"content": [{"type": "text", "text": format!("{err:#}")}], "isError": true}),
                    };
                    answer(&out, id, result);
                }));
            }
            _ => send(&out, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}})),
        }
    }
    // The client hung up: answer what it asked first.
    for call in calls {
        let _ = call.join();
    }
    Ok(())
}

fn send(out: &Out, message: &Value) {
    let mut out = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn answer(out: &Out, id: Value, result: Value) {
    send(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

/// Tells the client a long call is still going, which keeps it from
/// timing out.
struct Progress {
    out: Out,
    token: Option<Value>,
}

impl Progress {
    fn tick(&self, done: u64, message: &str) {
        let Some(token) = &self.token else { return };
        let params = json!({"progressToken": token, "progress": done, "message": message});
        send(&self.out, &json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));
    }
}

// ── The tools ───────────────────────────────────────────────────────────

fn tools() -> Value {
    let id = json!({"type": "integer", "description": "The terminal's id, from list_terminals"});
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    let types = json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false});
    json!([
        {
            "name": "list_terminals",
            "description": "Every terminal open in sasha: its id, workspace, group and name, what runs in it (Claude Code, Codex, a server, a shell…), whether it's busy or quiet, and for agents what they were last asked and whether they're waiting for the user. Your sessions (the ones you opened or prompted) come first, with their task and how it's going, then the ones that ended.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": read_only,
        },
        {
            "name": "read_terminal",
            "description": "The text a terminal shows. `lines` reaches back into the scrollback: that many lines up from the bottom, at least the whole screen.",
            "inputSchema": {"type": "object", "properties": {"id": id, "lines": {"type": "integer", "description": "Lines from the bottom, scrollback included (default: the screen)"}}, "required": ["id"]},
            "annotations": read_only,
        },
        {
            "name": "agent_report",
            "description": "What Claude Code or Codex in a terminal last did, from its session log: the prompt of its last turn, its last answer in full, files changed, commands run, how long it took, and what it's waiting for the user to allow or answer, if anything.",
            "inputSchema": {"type": "object", "properties": {"id": id}, "required": ["id"]},
            "annotations": read_only,
        },
        {
            "name": "send_prompt",
            "description": "Types text into a terminal and presses Enter: a prompt for the agent running there, or a command for its shell. Multi-line text arrives as one paste. With `submit: false` the text is typed but not sent.",
            "inputSchema": {"type": "object", "properties": {"id": id, "text": {"type": "string"}, "submit": {"type": "boolean", "description": "Press Enter after it (default true)"}}, "required": ["id", "text"]},
            "annotations": types,
        },
        {
            "name": "send_keys",
            "description": "Presses keys in a terminal, in order: names like enter, esc, tab, shift-tab, up, down, left, right, space, backspace, pageup, ctrl-c, alt-x, f5, or single characters (y, n, 1, 2). For menus, approvals, and interrupting.",
            "inputSchema": {"type": "object", "properties": {"id": id, "keys": {"type": "array", "items": {"type": "string"}}}, "required": ["id", "keys"]},
            "annotations": types,
        },
        {
            "name": "wait_for_terminal",
            "description": "Waits until a terminal has printed nothing for `quiet_seconds` (its agent finished its turn, or stopped to ask for something), or, given `text`, until that text shows up. Returns its state: for agents, their report; else the end of the screen.",
            "inputSchema": {"type": "object", "properties": {
                "id": id,
                "quiet_seconds": {"type": "number", "description": "How long it must stay quiet (default 5)"},
                "text": {"type": "string", "description": "Wait for this text on screen instead"},
                "timeout_seconds": {"type": "number", "description": "Give up after this (default 600, at most 3600)"},
            }, "required": ["id"]},
            "annotations": read_only,
        },
        {
            "name": "open_terminal",
            "description": "Opens a new terminal in a folder, in the background, and returns its id. With `agent` it starts Claude Code or Codex there on `prompt`, hooked so sasha hears the moment it finishes or needs an approval; else `command` is typed into its shell (`npm run dev`, `make test`…). Name it after its job: the name shows in the sidebar, in Claude Code's own session list, and in every update.",
            "inputSchema": {"type": "object", "properties": {
                "cwd": {"type": "string", "description": "Absolute path, or starting with ~"},
                "name": {"type": "string", "description": "A short name for its job, like api-tests or auth-fix"},
                "agent": {"type": "string", "enum": ["claude", "codex"], "description": "Start this agent in it"},
                "prompt": {"type": "string", "description": "The agent's task: complete and self-contained"},
                "command": {"type": "string", "description": "Typed into the shell once it starts, when there's no agent"},
            }, "required": ["cwd"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false},
        },
        {
            "name": "rename_terminal",
            "description": "Names a terminal after its job (empty: back to its folder's name), as it shows in the sidebar and in updates. Name the sessions you take over.",
            "inputSchema": {"type": "object", "properties": {"id": id, "name": {"type": "string"}}, "required": ["id", "name"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "wait_for_any",
            "description": "Waits until any of your sessions (or the ones in `ids`) has news: it finished, stopped for an approval, or closed. Returns the news; call it again for the rest. Use it to watch several sessions at once.",
            "inputSchema": {"type": "object", "properties": {
                "ids": {"type": "array", "items": {"type": "integer"}, "description": "Only these terminals (default: all your sessions)"},
                "timeout_seconds": {"type": "number", "description": "Give up after this (default 1800, at most 3600)"},
            }},
            "annotations": read_only,
        },
        {
            "name": "speak",
            "description": "Says something aloud to the user, cutting off anything it was saying. Use it when the user asks you to talk to them, to tell them, or for a spoken summary of everything or of one session. Write it to be heard: plain sentences, the big picture, sessions by their names, what needs the user. No code, paths, ids or symbols, and a few sentences unless they ask for more. Give it the tone that fits: calm or warm for routine status, gentle or tired late in the day, thoughtful or serious for decisions, pleased, relieved, proud, amused or excited for good news of different sizes, surprised when something's unexpected, concerned, sad or frustrated when things go wrong, urgent when something needs them now. When the mood shifts, as with good news and then a problem, send `parts`, each with its own tone. Empty text stops it talking.",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string", "description": "What to say, in one tone"},
                "tone": {"type": "string", "enum": voice::TONES, "description": "How it sounds (default warm)"},
                "parts": {"type": "array", "description": "Or what to say in parts, each in its tone, one after another", "items": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}, "tone": {"type": "string", "enum": voice::TONES}},
                    "required": ["text"],
                }},
                "as": {"type": "string", "description": "Which of your voices says it, by name (the one the user called); it keeps speaking until another is called"},
            }},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "show_terminal",
            "description": "Brings a terminal on screen for the user.",
            "inputSchema": {"type": "object", "properties": {"id": id}, "required": ["id"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "close_terminal",
            "description": "Closes a terminal and stops what runs in it. Only when the user asks for it.",
            "inputSchema": {"type": "object", "properties": {"id": id}, "required": ["id"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": false},
        },
    ])
}

/// What a session the tower started can do: see the others, read what
/// they did, and message them or the tower.
fn peer_tools() -> Value {
    let to = json!({"type": "string", "description": "A session's name (or #id), from list_sessions"});
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    json!([
        {
            "name": "list_sessions",
            "description": "The other agent sessions in sasha, by name: what each was asked and how it's going. The tower coordinates them; you're one of them.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": read_only,
        },
        {
            "name": "session_report",
            "description": "What another session last did: what it was asked, its last answer, files changed, commands run, and what it's waiting for.",
            "inputSchema": {"type": "object", "properties": {"to": to}, "required": ["to"]},
            "annotations": read_only,
        },
        {
            "name": "message_session",
            "description": "Sends a message to another session by name, or to `tower`. It reaches them when their current turn ends, as \"[message from …]\"; their answer reaches you the same way. Keep it short and specific: a question, a heads-up, a hand-off.",
            "inputSchema": {"type": "object", "properties": {"to": {"type": "string", "description": "A session's name or #id, or tower"}, "text": {"type": "string"}}, "required": ["to", "text"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false},
        },
    ])
}

fn peer_call(name: &str, args: &Value) -> Result<String> {
    let me = std::env::var("SASHA_TAB").ok().and_then(|id| id.parse::<TabId>().ok());
    match name {
        "list_sessions" => {
            let terminals = terminals()?;
            let mut text = String::from("The tower: coordinates every session (message it with to: tower)\n");
            for terminal in terminals.iter().filter(|t| t.mission.is_some() || agent(t).is_some()) {
                let you = if Some(terminal.id) == me { " (you)" } else { "" };
                text.push_str(&format!("{}{you} · {}", heading(terminal), state(terminal)));
                if let Some(mission) = &terminal.mission {
                    text.push_str(&format!("\n    task: {}\n    {}", mission.task, mission_line(mission)));
                }
                text.push('\n');
            }
            Ok(text)
        }
        "session_report" => {
            let to = args["to"].as_str().context("`to` is missing")?;
            report(&named(to)?)
        }
        "message_session" => {
            let to = args["to"].as_str().context("`to` is missing")?;
            let text = args["text"].as_str().context("`text` is missing")?;
            control::text(Request::Message { from: me, to: to.to_owned(), text: text.to_owned() })
        }
        _ => bail!("no tool called {name}"),
    }
}

/// The terminal a session's name or `#id` stands for.
fn named(to: &str) -> Result<Terminal> {
    let to = to.trim();
    let terminals = terminals()?;
    if let Ok(id) = to.trim_start_matches('#').parse::<TabId>()
        && let Some(terminal) = terminals.iter().find(|t| t.id == id)
    {
        return Ok(terminal.clone());
    }
    let mut named = terminals.into_iter().filter(|t| t.name.eq_ignore_ascii_case(to));
    match (named.next(), named.next()) {
        (Some(terminal), None) => Ok(terminal),
        (None, _) => bail!("no session called {to}: list_sessions shows their names"),
        (Some(_), Some(_)) => bail!("several sessions are called {to}: give its #id"),
    }
}

fn call(name: &str, args: &Value, progress: &Progress) -> Result<String> {
    match name {
        "list_terminals" => list(),
        "read_terminal" => {
            let lines = args["lines"].as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
            let id = id(args)?;
            let terminal = find(id)?;
            Ok(format!("{}\n\n{}", heading(&terminal), control::text(Request::Read { id, lines })?))
        }
        "agent_report" => {
            let terminal = find(id(args)?)?;
            report(&terminal)
        }
        "send_prompt" => {
            let id = id(args)?;
            let text = args["text"].as_str().context("`text` is missing")?;
            let terminal = find(id)?;
            // A session the tower started can report back: ask it to.
            let can_report = is_tower() && terminal.mission.as_ref().is_some_and(|m| m.hooked);
            let text = text.trim_end_matches(['\n', '\r']);
            let text = if can_report { hooks::seeded(text) } else { text.to_owned() };
            control::call(Request::Paste { id, text, tower: is_tower() })?;
            let submit = args["submit"].as_bool().unwrap_or(true);
            if submit {
                thread::sleep(BEFORE_ENTER);
                control::call(Request::Keys { id, keys: vec!["enter".to_owned()] })?;
            }
            let mut said = format!("{} {}", if submit { "Sent to" } else { "Typed into" }, heading(&terminal));
            if busy(&terminal) {
                said.push_str("\nIt was busy: the text may wait in its queue until it's done.");
            }
            Ok(said)
        }
        "send_keys" => {
            let id = id(args)?;
            let keys: Vec<String> = args["keys"]
                .as_array()
                .context("`keys` is missing")?
                .iter()
                .filter_map(|k| k.as_str().map(str::to_owned))
                .collect();
            let terminal = find(id)?;
            control::call(Request::Keys { id, keys: keys.clone() })?;
            Ok(format!("Pressed {} in {}", keys.join(" "), heading(&terminal)))
        }
        "wait_for_terminal" => wait(args, progress),
        "open_terminal" => {
            let cwd = args["cwd"].as_str().context("`cwd` is missing")?;
            let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            let cwd = expand_path(cwd, &home);
            let name = args["name"].as_str().map(str::trim).filter(|n| !n.is_empty());
            let prompt = args["prompt"].as_str().filter(|p| !p.trim().is_empty());
            let agent = match args["agent"].as_str() {
                Some("claude") => Some(Agent::Claude),
                Some("codex") => Some(Agent::Codex),
                Some(other) => bail!("agent is claude or codex, not {other}"),
                None => None,
            };
            let (command, task) = match agent {
                Some(agent) => {
                    let task = prompt.map_or_else(|| format!("{} session, waiting for a prompt", agent.name()), str::to_owned);
                    let seeded = prompt.map(hooks::seeded);
                    (Some(hooks::agent_command(agent, name, seeded.as_deref())?), Some(task))
                }
                None => (args["command"].as_str().map(str::to_owned), None),
            };
            let request =
                Request::Open { cwd: cwd.clone(), name: name.map(str::to_owned), command: command.clone(), tower: is_tower(), task };
            let Reply::Opened(id) = control::call(request)? else {
                bail!("unexpected answer from sasha");
            };
            let running = match (agent, &command) {
                (Some(agent), _) => format!(", running {}", agent.name()),
                (None, Some(command)) => format!(", running `{command}`"),
                (None, None) => String::new(),
            };
            let label = name.map(|n| format!(" ({n})")).unwrap_or_default();
            Ok(format!("Opened #{id}{label} in {}{running}. It's in the background; show_terminal brings it on screen.", pretty_path(&cwd)))
        }
        "rename_terminal" => {
            let id = id(args)?;
            let name = args["name"].as_str().context("`name` is missing")?;
            control::call(Request::Rename { id, name: name.to_owned() })?;
            Ok(format!("#{id} is called {}", if name.trim().is_empty() { "after its folder again" } else { name.trim() }))
        }
        "wait_for_any" => wait_for_any(args, progress),
        "speak" => {
            let tone = |value: &Value| {
                let tone = value.as_str().unwrap_or("warm");
                if voice::TONES.contains(&tone) { tone.to_owned() } else { "warm".to_owned() }
            };
            let mut parts: Vec<(String, String)> = match args["parts"].as_array() {
                Some(parts) => parts.iter().map(|p| (p["text"].as_str().unwrap_or_default().trim().to_owned(), tone(&p["tone"]))).collect(),
                None => vec![(args["text"].as_str().unwrap_or_default().trim().to_owned(), tone(&args["tone"]))],
            };
            parts.retain(|(text, _)| !text.is_empty());
            if parts.is_empty() {
                voice::hush();
                return Ok("Stopped talking.".to_owned());
            }
            let words: usize = parts.iter().map(|(text, _)| text.split_whitespace().count()).sum();
            let tones: Vec<&str> = parts.iter().map(|(_, tone)| tone.as_str()).collect();
            let person = args["as"].as_str().filter(|name| !name.trim().is_empty());
            if let Some(name) = person
                && !voice::people().is_empty()
                && !voice::people().iter().any(|p| p.name.eq_ignore_ascii_case(name.trim()))
            {
                let names: Vec<String> = voice::people().into_iter().map(|p| p.name).collect();
                bail!("no voice called {name}: there's {}", names.join(" and "));
            }
            voice::say_parts(&parts, person);
            let who = voice::current_person().map(|p| format!(" as {}", p.name)).unwrap_or_default();
            Ok(format!("Saying it aloud{who} ({words} words; {}).", tones.join(", then ")))
        }
        "show_terminal" => {
            let id = id(args)?;
            control::call(Request::Show { id })?;
            Ok(format!("#{id} is on screen"))
        }
        "close_terminal" => {
            let id = id(args)?;
            let terminal = find(id)?;
            control::call(Request::Close { id })?;
            Ok(format!("Closed {}", heading(&terminal)))
        }
        _ => bail!("no tool called {name}"),
    }
}

fn id(args: &Value) -> Result<TabId> {
    args["id"].as_u64().or_else(|| args["id"].as_str()?.trim_start_matches('#').parse().ok()).context("`id` is missing")
}

/// Every terminal; the tower's own isn't one of them.
fn terminals() -> Result<Vec<Terminal>> {
    control::terminals()
}

fn find(id: TabId) -> Result<Terminal> {
    terminals()?.into_iter().find(|t| t.id == id).ok_or_else(|| anyhow!("no terminal #{id}: list them again"))
}

fn agent(terminal: &Terminal) -> Option<Agent> {
    Agent::of(&terminal.program)
}

fn busy(terminal: &Terminal) -> bool {
    Duration::from_millis(terminal.quiet_ms) < BUSY
}

/// "#12 api (Claude, in ~/src/api)".
fn heading(terminal: &Terminal) -> String {
    let what = match agent(terminal) {
        Some(agent) => agent.name().to_owned(),
        None if terminal.program.is_empty() => "shell".to_owned(),
        None => terminal.program.clone(),
    };
    let place = terminal.cwd.as_deref().map(|cwd| format!(", in {}", pretty_path(cwd))).unwrap_or_default();
    format!("#{} {} ({what}{place})", terminal.id, terminal.name)
}

/// "busy", "quiet for 2m 5s", or "at its prompt".
fn state(terminal: &Terminal) -> String {
    let quiet = Duration::from_millis(terminal.quiet_ms);
    if terminal.program.is_empty() && !busy(terminal) {
        return format!("shell at its prompt, quiet for {}", notify::duration(quiet));
    }
    if busy(terminal) { "busy".to_owned() } else { format!("quiet for {}", notify::duration(quiet)) }
}

fn list() -> Result<String> {
    let (terminals, ended) = control::everything()?;
    if terminals.is_empty() && ended.is_empty() {
        return Ok("No terminals are open.".to_owned());
    }
    let (yours, others): (Vec<&Terminal>, Vec<&Terminal>) = terminals.iter().partition(|t| t.mission.is_some());
    let mut text = String::new();
    if !yours.is_empty() {
        let count = |state: State| yours.iter().filter(|t| t.mission.as_ref().is_some_and(|m| m.state == state)).count();
        text.push_str(&format!(
            "Your sessions: {} working, {} waiting for approval, {} done\n",
            count(State::Working),
            count(State::Waiting),
            count(State::Done)
        ));
        for terminal in &yours {
            text.push_str(&describe(terminal));
        }
        text.push('\n');
    }
    if !others.is_empty() {
        text.push_str("Other terminals:\n");
        for terminal in &others {
            text.push_str(&describe(terminal));
        }
    }
    if !ended.is_empty() {
        text.push_str("\nYour sessions that ended:\n");
        for Ended { id, name, workspace, mission } in ended.iter().rev().take(10) {
            text.push_str(&format!("#{id} {name} · workspace {workspace} · {}\n", mission_line(mission)));
        }
    }
    Ok(text)
}

/// A terminal in a few lines: where it is, what runs in it and how it's
/// doing, and for the tower's sessions their task.
fn describe(terminal: &Terminal) -> String {
    let mut line = format!("{} · {}", heading(terminal), state(terminal));
    let mut place = format!("workspace {}", terminal.workspace);
    if let Some(group) = &terminal.group {
        place.push_str(&format!(", group {group}"));
    }
    line.push_str(&format!(" · {place}"));
    if terminal.on_screen {
        line.push_str(" · on screen");
    } else if terminal.bell {
        line.push_str(" · rang the bell");
    }
    if let Some(mission) = &terminal.mission {
        line.push_str(&format!("\n    task: {}\n    {}", mission.task, mission_line(mission)));
    } else {
        if let Some(title) = &terminal.title {
            line.push_str(&format!("\n    title: {title}"));
        }
        if let (Some(agent), Some(cwd)) = (agent(terminal), &terminal.cwd)
            && let Some(report) = agent.report(cwd)
        {
            if let Some(ask) = &report.waiting {
                line.push_str(&format!("\n    WAITING for the user: {}", agents::clip(ask, 200)));
            }
            if !report.prompt.is_empty() {
                line.push_str(&format!("\n    last asked: {}", agents::clip(&agents::gist(&report.prompt), 160)));
            }
        }
    }
    line.push('\n');
    line
}

/// "done 3m ago: All 12 tests pass".
fn mission_line(mission: &Mission) -> String {
    let mut line = format!("{} {} ago", mission.state.word(), notify::duration(mission.age()));
    if let Some(note) = &mission.note {
        line.push_str(&format!(": {note}"));
    }
    if !mission.hooked && mission.state == State::Working {
        line.push_str(" (no hooks: its state is a guess from its output)");
    }
    line
}

fn report(terminal: &Terminal) -> Result<String> {
    // A log handed over by hooks is Claude Code's, whatever the program's name.
    let hooked_log = terminal.mission.as_ref().and_then(|m| m.log.as_ref());
    let Some(agent) = agent(terminal).or_else(|| hooked_log.map(|_| Agent::Claude)) else {
        let what = if terminal.program.is_empty() { "a shell" } else { terminal.program.as_str() };
        bail!("#{} runs {what}, not Claude or Codex: read_terminal shows it", terminal.id);
    };
    // The log its hooks named, else the newest where it runs.
    let log = terminal.mission.as_ref().and_then(|m| m.log.as_deref());
    let report = match log {
        Some(log) => agent.report_from(log),
        None => {
            let cwd = terminal.cwd.as_deref().context("can't tell where it runs")?;
            agent.report(cwd)
        }
    };
    let place = terminal.cwd.as_deref().map(pretty_path).unwrap_or_default();
    let report = report.with_context(|| format!("no recent {} session log for {place}", agent.name()))?;
    let mut text = format!("{} · {}\n", heading(terminal), state(terminal));
    if let Some(ask) = &report.waiting {
        text.push_str(&format!("\nWAITING for the user: {ask}\n"));
    }
    if !report.prompt.is_empty() {
        text.push_str(&format!("\nLast asked:\n{}\n", agents::clip(&report.prompt, 1_500)));
    }
    if !report.answer.is_empty() {
        text.push_str(&format!("\nLast answer:\n{}\n", agents::clip(&report.answer, ANSWER_MAX)));
    }
    let mut done = Vec::new();
    match report.files {
        0 => {}
        1 => done.push("1 file changed".to_owned()),
        n => done.push(format!("{n} files changed")),
    }
    match report.commands {
        0 => {}
        1 => done.push("1 command run".to_owned()),
        n => done.push(format!("{n} commands run")),
    }
    if let Some(took) = report.took {
        done.push(format!("took {}", notify::duration(took)));
    }
    if !done.is_empty() {
        text.push_str(&format!("\n{}\n", done.join(", ")));
    }
    Ok(text)
}

fn wait(args: &Value, progress: &Progress) -> Result<String> {
    let id = id(args)?;
    let quiet = Duration::from_secs_f64(args["quiet_seconds"].as_f64().unwrap_or(5.0).clamp(1.0, 600.0));
    let timeout = Duration::from_secs_f64(args["timeout_seconds"].as_f64().unwrap_or(600.0).clamp(1.0, 3600.0));
    let text = args["text"].as_str().filter(|t| !t.is_empty());
    let started = Instant::now();
    let mut last_tick = Instant::now();
    loop {
        let Some(terminal) = terminals()?.into_iter().find(|t| t.id == id) else {
            return Ok(format!("#{id} closed while waiting."));
        };
        let done = match text {
            Some(text) => control::text(Request::Read { id, lines: 200 })?.contains(text),
            // Quiet for long enough since the wait began.
            None => Duration::from_millis(terminal.quiet_ms) >= quiet && started.elapsed() >= quiet,
        };
        let timed_out = started.elapsed() >= timeout;
        if done || timed_out {
            let mut said = match (done, text) {
                (true, Some(text)) => format!("`{text}` showed up after {}.", notify::duration(started.elapsed())),
                (true, None) => format!("Quiet after {}.", notify::duration(started.elapsed())),
                (false, Some(text)) => format!("`{text}` didn't show up within {}.", notify::duration(timeout)),
                (false, None) => format!("Still going after {}: wait again, or look now.", notify::duration(timeout)),
            };
            said.push('\n');
            said.push_str(&match agent(&terminal) {
                Some(_) => report(&terminal).or_else(|_| tail(&terminal))?,
                None => tail(&terminal)?,
            });
            return Ok(said);
        }
        if last_tick.elapsed() >= PROGRESS_EVERY {
            last_tick = Instant::now();
            progress.tick(started.elapsed().as_secs(), &format!("#{id} {}", state(&terminal)));
        }
        thread::sleep(POLL);
    }
}

/// The tower's sessions, or `ids`: waits for the first news about them.
fn wait_for_any(args: &Value, progress: &Progress) -> Result<String> {
    let ids: Option<Vec<TabId>> = args["ids"].as_array().map(|ids| ids.iter().filter_map(Value::as_u64).collect());
    let timeout = Duration::from_secs_f64(args["timeout_seconds"].as_f64().unwrap_or(1800.0).clamp(1.0, 3600.0));
    let started = Instant::now();
    let mut last_tick = Instant::now();
    loop {
        let news = control::updates(ids.clone(), true)?;
        if !news.is_empty() {
            let lines: Vec<String> = news.iter().map(|u| format!("- {}", u.text)).collect();
            return Ok(format!("After {}:\n{}", notify::duration(started.elapsed()), lines.join("\n")));
        }
        // Nothing left that could have news.
        let terminals = terminals()?;
        let watched = |t: &&Terminal| match &ids {
            Some(ids) => ids.contains(&t.id),
            None => t.mission.as_ref().is_some_and(|m| matches!(m.state, State::Working | State::Waiting)),
        };
        let live: Vec<&Terminal> = terminals.iter().filter(watched).collect();
        if live.is_empty() {
            return Ok(match ids {
                Some(_) => "None of those terminals is open any more.".to_owned(),
                None => "None of your sessions is working or waiting: nothing to wait for.".to_owned(),
            });
        }
        if started.elapsed() >= timeout {
            let names: Vec<String> = live.iter().map(|t| format!("#{} {}", t.id, t.name)).collect();
            return Ok(format!("No news after {}; still going: {}.", notify::duration(timeout), names.join(", ")));
        }
        if last_tick.elapsed() >= PROGRESS_EVERY {
            last_tick = Instant::now();
            progress.tick(started.elapsed().as_secs(), &format!("waiting on {} sessions", live.len()));
        }
        thread::sleep(POLL);
    }
}

/// Whether the tower is asking: what it prompts becomes one of its
/// sessions.
fn is_tower() -> bool {
    std::env::var_os("SASHA_TOWER").is_some()
}

/// Whether a session the tower started is asking: it gets the peer tools.
fn is_peer() -> bool {
    std::env::var_os("SASHA_PEER").is_some()
}

/// The heading and the end of the screen.
fn tail(terminal: &Terminal) -> Result<String> {
    let screen = control::text(Request::Read { id: terminal.id, lines: 0 })?;
    let lines: Vec<&str> = screen.lines().collect();
    let end = lines[lines.len().saturating_sub(40)..].join("\n");
    Ok(format!("{} · {}\n\n{end}", heading(terminal), state(terminal)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_schema_and_is_answered() {
        let tools = tools();
        let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), 12);
        // Sessions get only what reaches the others.
        let peers: Vec<String> = peer_tools().as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
        assert_eq!(peers, ["list_sessions", "session_report", "message_session"]);
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["description"].as_str().is_some_and(|d| d.len() > 20));
        }
        // Only closing a terminal is destructive.
        let destructive: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["annotations"]["destructiveHint"] == true)
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(destructive, ["close_terminal"]);
    }

    #[test]
    fn ids_come_as_numbers_or_strings() {
        assert_eq!(id(&json!({"id": 7})).unwrap(), 7);
        assert_eq!(id(&json!({"id": "#7"})).unwrap(), 7);
        assert!(id(&json!({})).is_err());
    }
}
