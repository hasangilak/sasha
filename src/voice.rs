//! The tower's voice: it says things aloud to you when you ask (its
//! `speak` tool) or, with voice replies on, the big picture of each answer.
//! With Kokoro set up (`sasha voice install`), a neural voice close to a
//! person's, kept loaded by a small speaker process; with Chatterbox chosen
//! (`voice/engine`), that voice with feeling, a tone per part. Else
//! macOS's own.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use anyhow::{Context, Result, bail};

use crate::agents::{self, Agent};
use crate::{store, tower};

/// The most of an answer said aloud.
const SPOKEN_MAX: usize = 400;
/// The speaker that keeps Kokoro loaded, written beside it.
const SPEAKER: &str = include_str!("voice/speaker.py");
/// Kokoro's voice unless you pick another: warm, clear, American.
const KOKORO_VOICE: &str = "af_heart";
/// The tones a part can be said in, from the quiet ones to the loudest.
pub const TONES: [&str; 16] = [
    "tired", "gentle", "calm", "thoughtful", "serious", "warm", "sad", "pleased", "relieved", "concerned", "proud",
    "amused", "frustrated", "excited", "urgent", "surprised",
];
/// How long a starting speaker gets to load Kokoro.
const SPEAKER_START: Duration = Duration::from_secs(30);
/// What Kokoro needs, beside mlx-audio: its English text processing.
const KOKORO_PACKAGES: [&str; 3] = [
    "mlx-audio",
    "misaki[en]",
    "en_core_web_sm @ https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl",
];

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
    say_parts(&[(text.to_owned(), "warm".to_owned())]);
}

/// Says `parts` one after another, each `(text, tone)` in its tone where
/// the voice can do feeling.
pub fn say_parts(parts: &[(String, String)]) {
    if let Some(python) = kokoro_python()
        && say_with_speaker(&python, parts).is_ok()
    {
        return;
    }
    let text: Vec<&str> = parts.iter().map(|(text, _)| text.as_str()).collect();
    say_with_mac(&text.join(" "));
}

/// The speaker's engine: `$SASHA_VOICE_ENGINE`, else the first line of
/// `voice/engine` (`chatterbox` for feeling), else Kokoro.
fn chosen_engine() -> String {
    let engine = std::env::var("SASHA_VOICE_ENGINE").ok().or_else(|| std::fs::read_to_string(dir()?.join("engine")).ok());
    engine.and_then(|e| e.lines().next().map(|l| l.trim().to_ascii_lowercase())).filter(|e| !e.is_empty()).unwrap_or_else(|| "kokoro".to_owned())
}

/// The voice you picked: `$SASHA_VOICE`, else the first line of
/// `voice/voice`.
fn chosen_voice() -> Option<String> {
    let voice = std::env::var("SASHA_VOICE").ok().or_else(|| std::fs::read_to_string(dir()?.join("voice")).ok())?;
    let voice = voice.lines().next()?.trim().to_owned();
    (!voice.is_empty()).then_some(voice)
}

/// Kokoro's voices go by language, sex and name: `af_heart`, `bm_george`.
fn is_kokoro_voice(voice: &str) -> bool {
    let bytes = voice.as_bytes();
    bytes.len() > 3 && bytes[0].is_ascii_lowercase() && bytes[1].is_ascii_lowercase() && bytes[2] == b'_'
}

/// Kokoro's Python, once it's set up.
fn kokoro_python() -> Option<PathBuf> {
    let python = dir()?.join("kokoro/bin/python");
    python.is_file().then_some(python)
}

/// Hands `parts` to the speaker, starting it (and loading its model)
/// first if it isn't running.
fn say_with_speaker(python: &Path, parts: &[(String, String)]) -> Result<()> {
    let dir = dir().context("no home folder")?;
    let socket = speaker_socket()?;
    let voice = chosen_voice().filter(|v| is_kokoro_voice(v)).unwrap_or_else(|| KOKORO_VOICE.to_owned());
    let parts: Vec<Value> = parts.iter().map(|(text, tone)| json!({"text": text, "tone": tone})).collect();
    let request = json!({"say": parts, "voice": voice, "engine": chosen_engine()});
    if tell_speaker(&socket, &request).is_ok() {
        return Ok(());
    }
    let script = dir.join("speaker.py");
    std::fs::write(&script, SPEAKER)?;
    let log = std::fs::File::create(dir.join("speaker.log")).map_or_else(|_| Stdio::null(), Stdio::from);
    let mut command = Command::new(python);
    command.arg(&script).arg(&socket).arg(&dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // A session of its own: it outlives whoever asked first.
    // SAFETY: setsid is async-signal-safe, so it may run between fork and exec.
    unsafe {
        command.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    command.spawn().context("can't start the speaker")?;
    let started = Instant::now();
    while started.elapsed() < SPEAKER_START {
        std::thread::sleep(Duration::from_millis(200));
        if tell_speaker(&socket, &request).is_ok() {
            return Ok(());
        }
    }
    bail!("the speaker didn't start: see {}", dir.join("speaker.log").display())
}

/// The speaker's socket, beside sasha's in its private folder: a socket's
/// path can't be long.
fn speaker_socket() -> Result<PathBuf> {
    let sasha = crate::protocol::socket_path()?;
    Ok(sasha.with_file_name("speaker.sock"))
}

fn tell_speaker(socket: &Path, request: &Value) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    writeln!(stream, "{request}")
}

/// macOS's own voice: `say`, with your system voice or the one you picked.
fn say_with_mac(text: &str) {
    hush();
    let mut command = Command::new("say");
    if let Some(voice) = chosen_voice().filter(|v| !is_kokoro_voice(v)) {
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
    if let Ok(socket) = speaker_socket() {
        let _ = tell_speaker(&socket, &json!({"stop": true}));
    }
    let Some(dir) = dir() else { return };
    let file = dir.join("saying");
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

/// `sasha voice install`: sets Kokoro up beside sasha with uv, and says
/// so in its voice.
pub fn install() -> Result<()> {
    let uv = tower::find_program("uv").context("Kokoro is set up with uv: brew install uv")?;
    let dir = dir().context("no home folder")?;
    let venv = dir.join("kokoro");
    if kokoro_python().is_none() {
        println!("Setting Kokoro up in {}…", venv.display());
        let made = Command::new(&uv).args(["venv", "--python", "3.12"]).arg(&venv).status()?;
        if !made.success() {
            bail!("uv couldn't make Kokoro's environment");
        }
    }
    let python = venv.join("bin/python");
    let installed = Command::new(&uv).args(["pip", "install", "--python"]).arg(&python).args(KOKORO_PACKAGES).status()?;
    if !installed.success() {
        bail!("uv couldn't install Kokoro");
    }
    println!("Loading Kokoro (the first time, it downloads its model)…");
    say_with_speaker(&python, &[("Hello. This is how the tower sounds now.".to_owned(), "warm".to_owned())])?;
    println!("Done. The tower speaks with Kokoro; pick another voice in {}.", dir.join("voice").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_voices_are_told_from_macs() {
        assert!(is_kokoro_voice("af_heart") && is_kokoro_voice("bm_george"));
        assert!(!is_kokoro_voice("Daniel") && !is_kokoro_voice("Ava (Premium)") && !is_kokoro_voice("a_b"));
    }

    #[test]
    fn only_the_big_picture_is_said() {
        let answer = "All three sessions are done and the tests pass.\n\n```rust\nfn main() {}\n```\n\n| a | b |";
        assert_eq!(spoken(answer), "All three sessions are done and the tests pass.");
        assert!(spoken(&"Word. ".repeat(200)).chars().count() <= SPOKEN_MAX);
    }
}
