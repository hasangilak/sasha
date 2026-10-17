//! The tower's voice: it says things aloud to you in macOS's own voice,
//! when you ask (its `speak` tool) or, with voice replies on, the big
//! picture of each answer.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use anyhow::Result;

use crate::agents::{self, Agent};
use crate::{store, tower};

/// The most of an answer said aloud.
const SPOKEN_MAX: usize = 400;

/// Where the voice keeps its state, beside sasha's: `~/.config/sasha/voice`.
pub fn dir() -> Option<PathBuf> {
    Some(store::path()?.parent()?.join("voice"))
}

/// The file that says voice replies are on, for the hooks to see.
fn on_flag() -> Option<PathBuf> {
    Some(dir()?.join("speak"))
}

pub fn set_replies(on: bool) {
    let Some(flag) = on_flag() else { return };
    if on {
        let _ = flag.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(flag, "on\n");
    } else {
        let _ = std::fs::remove_file(flag);
    }
}

fn replies_on() -> bool {
    on_flag().is_some_and(|flag| flag.is_file())
}

/// `sasha speak tower`, from the tower's hook once it has answered: says
/// the big picture of its answer, when voice replies are on.
pub fn speak_tower() -> Result<()> {
    if !replies_on() {
        return Ok(());
    }
    // Claude Code's hook says where its log is; pi's is found by folder.
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    let log = serde_json::from_str::<Value>(&input).ok().and_then(|v| v["transcript_path"].as_str().map(PathBuf::from));
    let report = match log {
        Some(log) => Agent::Claude.report_from(&log),
        None => tower::dir().and_then(|dir| Agent::Pi.report(&dir).or_else(|| Agent::Claude.report(&dir))),
    };
    let Some(report) = report else { return Ok(()) };
    let text = spoken(&report.answer);
    if !text.is_empty() {
        say(&text);
    }
    Ok(())
}

/// An answer's opening, the big picture, as it reads aloud.
pub fn spoken(answer: &str) -> String {
    agents::clip(&agents::gist(answer), SPOKEN_MAX)
}

/// Says `text`, cutting off whatever it was saying.
pub fn say(text: &str) {
    hush();
    let mut command = Command::new("say");
    if let Ok(voice) = std::env::var("SASHA_VOICE") {
        command.args(["-v", &voice]);
    }
    let Ok(child) = command.arg(text).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return;
    };
    if let Some(dir) = dir() {
        let _ = std::fs::write(dir.join("saying"), child.id().to_string());
    }
}

/// Stops talking, so it doesn't talk over you.
pub fn hush() {
    let Some(file) = dir().map(|dir| dir.join("saying")) else { return };
    if let Some(pid) = std::fs::read_to_string(&file).ok().and_then(|pid| pid.trim().parse::<i32>().ok()) {
        // Only if it's still `say`: the pid may have been reused.
        if crate::procinfo::name(pid).as_deref() == Some("say") {
            // SAFETY: kill has no memory-safety preconditions.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
    let _ = std::fs::remove_file(file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_big_picture_is_said() {
        let answer = "All three sessions are done and the tests pass.\n\n```rust\nfn main() {}\n```\n\n| a | b |";
        assert_eq!(spoken(answer), "All three sessions are done and the tests pass.");
        assert!(spoken(&"Word. ".repeat(200)).chars().count() <= SPOKEN_MAX);
    }
}
