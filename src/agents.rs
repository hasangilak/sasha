//! What Claude Code and Codex just did, read from the session logs they
//! keep on disk, so a notification can say it: "Claude finished · sasha"
//! with the gist of its answer, or "Claude needs you" with the command
//! it wants to run.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::notify;

/// How much of the end of a session log is read.
const TAIL: u64 = 2 * 1024 * 1024;
/// Logs untouched for longer belong to sessions that aren't this one.
const FRESH: Duration = Duration::from_secs(30 * 60);
/// The longest notification text.
const BODY_MAX: usize = 220;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    /// pi, as the tower.
    Pi,
}

impl Agent {
    /// The agent a tab's foreground program is, if it's one.
    pub fn of(process: &str) -> Option<Self> {
        match process {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Pi => "The tower",
        }
    }

    /// What the agent working in `cwd` last did, from its newest session
    /// log there.
    pub fn report(self, cwd: &Path) -> Option<Report> {
        match self {
            Agent::Claude => claude_turn(&read_tail(&claude_log(cwd)?)),
            Agent::Codex => codex_turn(&read_tail(&codex_log(cwd)?)),
            Agent::Pi => pi_turn(&read_tail(&pi_log(cwd)?)),
        }
    }

    /// What the agent last did, from the session log at `log`.
    pub fn report_from(self, log: &Path) -> Option<Report> {
        let entries = read_tail(log);
        match self {
            Agent::Claude => claude_turn(&entries),
            Agent::Codex => codex_turn(&entries),
            Agent::Pi => pi_turn(&entries),
        }
    }

    /// A report in a line: the gist of the answer and what was done, or
    /// what it waits for.
    pub fn summary(self, report: &Report) -> String {
        self.message(report, "").1
    }

    /// The notification's heading and text for a report, in the tab called
    /// `place`.
    pub fn message(self, report: &Report, place: &str) -> (String, String) {
        let name = self.name();
        if let Some(ask) = &report.waiting {
            return (format!("{name} needs you · {place}"), clip(ask, BODY_MAX));
        }
        // The tower is a place of its own.
        let mut heading = if self == Agent::Pi { format!("{name} finished") } else { format!("{name} finished · {place}") };
        if let Some(took) = report.took {
            heading.push_str(&format!(" · {}", notify::duration(took)));
        }
        let mut done = Vec::new();
        match report.files {
            0 => {}
            1 => done.push("1 file changed".to_owned()),
            n => done.push(format!("{n} files changed")),
        }
        match report.commands {
            0 => {}
            1 => done.push("1 command".to_owned()),
            n => done.push(format!("{n} commands")),
        }
        let done = if done.is_empty() { String::new() } else { format!(" ({})", done.join(", ")) };
        let said = gist(&report.answer);
        let body = match (said.is_empty(), report.prompt.is_empty()) {
            (false, _) => format!("{}{done}", clip(&said, BODY_MAX.saturating_sub(done.len()))),
            (true, false) => format!("Done: {}{done}", clip(&gist(&report.prompt), BODY_MAX.saturating_sub(done.len() + 6))),
            (true, true) if !done.is_empty() => done.trim().trim_matches(['(', ')']).to_owned(),
            (true, true) => "is done".to_owned(),
        };
        (heading, body)
    }
}

/// An agent's last turn.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// What it was asked.
    pub prompt: String,
    /// Its last words.
    pub answer: String,
    /// What it's waiting for the user to allow or answer, if it is.
    pub waiting: Option<String>,
    pub took: Option<Duration>,
    pub files: usize,
    pub commands: usize,
}

// ── Finding the logs ────────────────────────────────────────────────────

fn home() -> Option<PathBuf> {
    std::env::home_dir()
}

/// Where Claude Code keeps the sessions of `cwd`: in its projects folder,
/// under the path with everything but letters and digits made `-`.
fn claude_project(cwd: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| Some(home()?.join(".claude")))?;
    let name: String = cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    Some(base.join("projects").join(name))
}

/// Claude Code's log of the newest session in `cwd`.
fn claude_log(cwd: &Path) -> Option<PathBuf> {
    newest(fs::read_dir(claude_project(cwd)?).ok()?.flatten().map(|e| e.path()))
}

/// Whether Claude Code has had a session in `cwd` to continue.
pub fn claude_has_session(cwd: &Path) -> bool {
    let entries = claude_project(cwd).and_then(|dir| fs::read_dir(dir).ok());
    entries.into_iter().flatten().flatten().any(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
}

/// Codex's log of the newest session in `cwd`: they're filed by the day
/// they started, and the first line says where.
fn codex_log(cwd: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| Some(home()?.join(".codex")))?;
    // sessions/YYYY/MM/DD: the last week's days.
    let mut days = Vec::new();
    for year in sorted_dirs(&base.join("sessions")).into_iter().rev().take(2) {
        for month in sorted_dirs(&year).into_iter().rev().take(2) {
            days.extend(sorted_dirs(&month));
        }
    }
    days.sort();
    let logs = days.iter().rev().take(7).flat_map(|day| fs::read_dir(day).into_iter().flatten().flatten()).map(|e| e.path());
    let mut logs: Vec<(SystemTime, PathBuf)> = fresh_jsonl(logs).collect();
    logs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    logs.into_iter().map(|(_, path)| path).find(|path| codex_cwd(path).as_deref() == Some(cwd))
}

/// pi's log of the newest session in `cwd`: in its sessions folder, under
/// the path with its separators made `-`, between `--`.
fn pi_log(cwd: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).or_else(|| Some(home()?.join(".pi/agent")))?;
    let path = cwd.to_string_lossy();
    let name = format!("--{}--", path.trim_start_matches('/').replace(['/', '\\', ':'], "-"));
    newest(fs::read_dir(base.join("sessions").join(name)).ok()?.flatten().map(|e| e.path()))
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs
}

/// `.jsonl` files written to lately, with when.
fn fresh_jsonl(paths: impl Iterator<Item = PathBuf>) -> impl Iterator<Item = (SystemTime, PathBuf)> {
    paths.filter(|p| p.extension().is_some_and(|e| e == "jsonl")).filter_map(|path| {
        let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
        (modified.elapsed().unwrap_or_default() < FRESH).then_some((modified, path))
    })
}

fn newest(paths: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    fresh_jsonl(paths).max_by_key(|(modified, _)| *modified).map(|(_, path)| path)
}

/// Where a Codex session runs, from its first line.
fn codex_cwd(log: &Path) -> Option<PathBuf> {
    let mut head = String::new();
    File::open(log).ok()?.take(64 * 1024).read_to_string(&mut head).ok();
    let first: Value = serde_json::from_str(head.lines().next()?).ok()?;
    Some(PathBuf::from(first.pointer("/payload/cwd")?.as_str()?))
}

/// The last lines of a log, as JSON.
fn read_tail(path: &Path) -> Vec<Value> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(TAIL);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    // Starting mid-file, the first line is only part of one.
    let skip = usize::from(start > 0);
    text.lines().skip(skip).filter_map(|line| serde_json::from_str(line).ok()).collect()
}

// ── Reading a turn ──────────────────────────────────────────────────────

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or_default()
}

/// Claude Code's last turn: the prompt that started it, the tools it used,
/// its last words, and a tool call still waiting for permission.
fn claude_turn(entries: &[Value]) -> Option<Report> {
    let mut report = None::<Report>;
    let mut files = HashSet::new();
    // Tool calls without a result yet, by id.
    let mut open: HashMap<String, String> = HashMap::new();
    for entry in entries {
        if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let content = entry.pointer("/message/content");
        match entry.get("type").and_then(Value::as_str) {
            Some("user") => {
                let results: Vec<&str> = content
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .map(|block| str_at(block, "/tool_use_id"))
                    .collect();
                for id in &results {
                    open.remove(*id);
                }
                let prompt = text_of(content);
                let meta = entry.get("isMeta").and_then(Value::as_bool) == Some(true);
                // A new prompt starts a turn; command echoes and the like
                // come wrapped in <tags>.
                let prompt_like = !prompt.trim().is_empty()
                    && !prompt.trim_start().starts_with('<')
                    && !prompt.starts_with("[Request interrupted");
                if results.is_empty() && !meta && prompt_like {
                    report = Some(Report { prompt, ..Report::default() });
                    files.clear();
                    open.clear();
                }
            }
            Some("assistant") => {
                let report = report.get_or_insert_with(Report::default);
                for block in content.and_then(Value::as_array).into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = str_at(block, "/text");
                            if !text.trim().is_empty() {
                                report.answer = text.to_owned();
                            }
                        }
                        Some("tool_use") => {
                            let name = str_at(block, "/name");
                            let input = block.get("input").unwrap_or(&Value::Null);
                            match name {
                                "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
                                    let path = str_at(input, "/file_path");
                                    files.insert(if path.is_empty() { str_at(input, "/notebook_path") } else { path }.to_owned());
                                }
                                "Bash" => report.commands += 1,
                                _ => {}
                            }
                            open.insert(str_at(block, "/id").to_owned(), claude_ask(name, input));
                        }
                        _ => {}
                    }
                }
            }
            Some("system") if str_at(entry, "/subtype") == "turn_duration" => {
                if let Some(report) = &mut report {
                    report.took = entry.get("durationMs").and_then(Value::as_u64).map(Duration::from_millis);
                }
                open.clear();
            }
            _ => {}
        }
    }
    let mut report = report?;
    report.files = files.len();
    // A call still open when the agent has gone quiet is waiting for a yes.
    report.waiting = open.into_values().next();
    Some(report)
}

/// pi's last turn: the prompt that started it, its last words, the
/// commands it ran and files it changed.
fn pi_turn(entries: &[Value]) -> Option<Report> {
    let mut report = None::<Report>;
    let mut files = HashSet::new();
    let mut started = None;
    for entry in entries.iter().filter(|e| str_at(e, "/type") == "message") {
        let content = entry.pointer("/message/content");
        let at = entry.pointer("/message/timestamp").and_then(Value::as_u64);
        match str_at(entry, "/message/role") {
            "user" => {
                report = Some(Report { prompt: text_of(content), ..Report::default() });
                files.clear();
                started = at;
            }
            "assistant" => {
                let report = report.get_or_insert_with(Report::default);
                for block in content.and_then(Value::as_array).into_iter().flatten() {
                    match str_at(block, "/type") {
                        "text" if !str_at(block, "/text").trim().is_empty() => report.answer = str_at(block, "/text").to_owned(),
                        "toolCall" => match str_at(block, "/name") {
                            "bash" => report.commands += 1,
                            "edit" | "write" => {
                                files.insert(str_at(block, "/arguments/path").to_owned());
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                if let (Some(start), Some(end)) = (started, at) {
                    report.took = Some(Duration::from_millis(end.saturating_sub(start)));
                }
            }
            _ => {}
        }
    }
    let mut report = report?;
    report.files = files.len();
    Some(report)
}

/// What Claude wants to do with a tool, in words.
fn claude_ask(name: &str, input: &Value) -> String {
    let file = |key: &str| {
        let path = str_at(input, key);
        Path::new(path).file_name().map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
    };
    match name {
        "Bash" => format!("Wants to run: {}", str_at(input, "/command")),
        "Edit" | "MultiEdit" => format!("Wants to edit {}", file("/file_path")),
        "Write" => format!("Wants to write {}", file("/file_path")),
        "WebFetch" => format!("Wants to fetch {}", str_at(input, "/url")),
        "AskUserQuestion" => format!("Asks: {}", str_at(input, "/questions/0/question")),
        "ExitPlanMode" => "Has a plan ready for you".to_owned(),
        _ => format!("Wants to use {name}"),
    }
}

/// Codex's last turn, from its events: the task's start and end, messages,
/// commands, file changes and requests for approval.
fn codex_turn(entries: &[Value]) -> Option<Report> {
    let mut report = None::<Report>;
    let mut files = HashSet::new();
    let mut ended = false;
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let payload = entry.get("payload").unwrap_or(&Value::Null);
        let item = payload.get("item").unwrap_or(&Value::Null);
        let kind = str_at(payload, "/type");
        if kind == "task_started" {
            report = Some(Report::default());
            files.clear();
            ended = false;
            continue;
        }
        let report = report.get_or_insert_with(Report::default);
        match (kind, str_at(item, "/type")) {
            ("task_complete", _) => {
                ended = true;
                report.waiting = None;
                report.took = payload.get("duration_ms").and_then(Value::as_u64).map(Duration::from_millis);
                let last = str_at(payload, "/last_agent_message");
                if !last.trim().is_empty() {
                    report.answer = last.to_owned();
                }
            }
            // Stopped with Esc: nothing's waiting.
            ("turn_aborted", _) => {
                ended = true;
                report.waiting = None;
            }
            ("user_message", _) => report.prompt = str_at(payload, "/message").to_owned(),
            ("agent_message", _) => report.answer = str_at(payload, "/message").to_owned(),
            ("item_completed", "UserMessage") => report.prompt = text_of(item.get("content")),
            ("item_completed", "AgentMessage") => report.answer = text_of(item.get("content")),
            ("item_completed", "CommandExecution") => report.commands += 1,
            ("item_completed", "FileChange") => {
                files.extend(item.get("changes").and_then(Value::as_object).into_iter().flat_map(|c| c.keys().cloned()));
            }
            ("exec_approval_request", _) => {
                let command = match payload.get("command") {
                    Some(Value::Array(argv)) => argv.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                    other => text_of(other),
                };
                report.waiting = Some(format!("Wants to run: {command}"));
            }
            ("apply_patch_approval_request", _) => report.waiting = Some("Wants to change files".to_owned()),
            _ => {}
        }
    }
    let mut report = report?;
    report.files = files.len();
    // Quiet in the middle of a task: it's waiting for you.
    if !ended && report.waiting.is_none() {
        report.waiting = Some(match gist(&report.answer) {
            said if said.is_empty() => "Is waiting for you".to_owned(),
            said => said,
        });
    }
    Some(report)
}

// ── Saying it briefly ───────────────────────────────────────────────────

/// The first paragraphs of an answer that say something, without markdown,
/// on one line; list items run on with semicolons.
pub fn gist(text: &str) -> String {
    let mut said = String::new();
    let mut in_code = false;
    for paragraph in text.split("\n\n") {
        let mut part = String::new();
        for line in paragraph.lines() {
            let line = line.trim();
            if line.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            // Code, tables, headings and rules don't read well in a
            // notification.
            if in_code || line.starts_with(['|', '#']) || line.chars().all(|c| "-*_=".contains(c)) {
                continue;
            }
            let (item, line) = match ["- ", "* ", "• "].iter().find_map(|marker| line.strip_prefix(marker)) {
                Some(rest) => (true, rest),
                None => match strip_numbering(line) {
                    rest if rest.len() < line.len() => (true, rest),
                    rest => (false, rest),
                },
            };
            let line = line.trim_start_matches('>').trim().replace("**", "").replace("__", "").replace('`', "");
            if line.is_empty() {
                continue;
            }
            if !part.is_empty() {
                part.push_str(if item && !part.ends_with(':') { "; " } else { " " });
            }
            part.push_str(&line);
        }
        if part.chars().any(char::is_alphanumeric) {
            if !said.is_empty() {
                said.push(' ');
            }
            said.push_str(&part);
        }
        // A short opening ("Done.") reads better with what follows.
        if said.chars().count() >= 60 {
            break;
        }
    }
    said
}

fn strip_numbering(line: &str) -> &str {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    match line[digits..].strip_prefix(". ") {
        Some(rest) if digits > 0 => rest,
        _ => line,
    }
}

/// `text` cut to `max` characters, at the end of a sentence if one ends
/// late enough, else at a word, with "…".
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    if let Some(end) = cut.rfind(". ").filter(|&end| end >= max / 2) {
        return cut[..=end].to_owned();
    }
    let at_word = cut.rfind(' ').filter(|&space| space >= max / 2).unwrap_or(cut.len());
    format!("{}…", cut[..at_word].trim_end_matches([',', ';', ':', ' ']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Value> {
        text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
    }

    #[test]
    fn claude_turns_say_what_was_done() {
        let log = lines(
            r#"{"type":"user","message":{"role":"user","content":"old prompt"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Old answer."}]}}
{"type":"user","message":{"role":"user","content":"add a files bar"}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/p/src/a.rs"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}
{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"a subagent"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo test"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Write","input":{"file_path":"/p/src/b.rs"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t3"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"I've added a **files bar**.\n\nIt shows the tree."}]}}
{"type":"system","subtype":"turn_duration","durationMs":125000}"#,
        );
        let report = claude_turn(&log).unwrap();
        assert_eq!(report.prompt, "add a files bar");
        assert_eq!((report.files, report.commands, report.waiting.as_deref()), (2, 1, None));
        assert_eq!(report.took, Some(Duration::from_secs(125)));
        let (heading, body) = Agent::Claude.message(&report, "sasha");
        assert_eq!(heading, "Claude finished · sasha · 2m 5s");
        assert_eq!(body, "I've added a files bar. It shows the tree. (2 files changed, 1 command)");
    }

    #[test]
    fn a_tool_call_without_a_result_is_waiting_for_permission() {
        let log = lines(
            r#"{"type":"user","message":{"content":"ship it"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Pushing now."},{"type":"tool_use","id":"t9","name":"Bash","input":{"command":"git push origin main"}}]}}"#,
        );
        let report = claude_turn(&log).unwrap();
        let (heading, body) = Agent::Claude.message(&report, "api");
        assert_eq!(heading, "Claude needs you · api");
        assert_eq!(body, "Wants to run: git push origin main");
    }

    #[test]
    fn codex_turns_read_their_events() {
        let log = lines(
            r#"{"type":"session_meta","payload":{"cwd":"/p"}}
{"type":"event_msg","payload":{"type":"task_started"}}
{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"fix the login"}]}}}
{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["npm","test"]}}}
{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","changes":{"/p/a.ts":{},"/p/b.ts":{}}}}}
{"type":"event_msg","payload":{"type":"task_complete","duration_ms":61000,"last_agent_message":"Fixed the login redirect; tests pass."}}"#,
        );
        let report = codex_turn(&log).unwrap();
        let (heading, body) = Agent::Codex.message(&report, "web");
        assert_eq!(heading, "Codex finished · web · 1m 1s");
        assert_eq!(body, "Fixed the login redirect; tests pass. (2 files changed, 1 command)");
        // Still going, but quiet: waiting for an approval.
        let waiting = lines(
            r#"{"type":"event_msg","payload":{"type":"task_started"}}
{"type":"event_msg","payload":{"type":"exec_approval_request","command":["rm","-rf","dist"]}}"#,
        );
        let (heading, body) = Agent::Codex.message(&codex_turn(&waiting).unwrap(), "web");
        assert_eq!((heading.as_str(), body.as_str()), ("Codex needs you · web", "Wants to run: rm -rf dist"));
    }

    #[test]
    fn pi_turns_say_what_the_tower_did() {
        let log = lines(
            r#"{"type":"session","version":3,"id":"s","cwd":"/t"}
{"type":"message","id":"a","message":{"role":"user","content":"old","timestamp":1000}}
{"type":"message","id":"b","message":{"role":"assistant","content":[{"type":"text","text":"Old."}],"timestamp":2000}}
{"type":"message","id":"c","message":{"role":"user","content":[{"type":"text","text":"Get the api tests green"}],"timestamp":10000}}
{"type":"message","id":"d","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"toolCall","id":"1","name":"mcp__sasha__send_prompt","arguments":{"id":3}}],"timestamp":11000}}
{"type":"message","id":"e","message":{"role":"toolResult","toolCallId":"1","content":[{"type":"text","text":"Sent"}],"timestamp":11500}}
{"type":"message","id":"f","message":{"role":"assistant","content":[{"type":"toolCall","id":"2","name":"bash","arguments":{"command":"ls"}},{"type":"toolCall","id":"3","name":"edit","arguments":{"path":"a.rs"}}],"timestamp":12000}}
{"type":"message","id":"g","message":{"role":"assistant","content":[{"type":"text","text":"All green: api 42 passed."}],"timestamp":70000}}"#,
        );
        let report = pi_turn(&log).unwrap();
        assert_eq!(report.prompt, "Get the api tests green");
        assert_eq!(report.answer, "All green: api 42 passed.");
        assert_eq!((report.commands, report.files, report.waiting.as_deref()), (1, 1, None));
        assert_eq!(report.took, Some(Duration::from_secs(60)));
        let (heading, body) = Agent::Pi.message(&report, "tower");
        assert_eq!(heading, "The tower finished · 1m 0s");
        assert!(body.starts_with("All green"));
    }

    #[test]
    fn answers_are_cut_down_to_their_gist() {
        let answer = "## Summary\n\nDone.\n\n- Added the **bar**\n- Wrote `tests`\n\n```rust\nfn x() {}\n```\n\n| a | b |";
        assert_eq!(gist(answer), "Done. Added the bar; Wrote tests");
        assert_eq!(gist("Changes:\n1. one\n2. two"), "Changes: one; two");
        assert_eq!(clip("One two three. Four five six seven", 20), "One two three.");
        assert_eq!(clip("alpha beta gamma delta epsilon", 20), "alpha beta gamma…");
        assert_eq!(strip_numbering("12. step"), "step");
    }
}

