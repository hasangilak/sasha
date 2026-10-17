//! The tower's voice. You talk to it: sasha records you with ffmpeg and
//! hears you with Whisper (through whisper.cpp). It talks back: the big
//! picture of each answer in a sentence or two, in macOS's own voice.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::agents::{self, Agent};
use crate::{store, tower};

/// Whisper's model, multilingual and quick on Apple Silicon.
const MODEL: &str = "ggml-large-v3-turbo-q5_0.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";
/// The most of an answer said aloud.
const SPOKEN_MAX: usize = 400;
/// How long ffmpeg gets to finish the recording once told to stop.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// Where the voice keeps its model and state, beside sasha's:
/// `~/.config/sasha/voice`.
pub fn dir() -> Option<PathBuf> {
    Some(store::path()?.parent()?.join("voice"))
}

/// Whisper's model: `$SASHA_WHISPER_MODEL`, else one in the voice folder.
pub fn model() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SASHA_WHISPER_MODEL") {
        return Some(PathBuf::from(path));
    }
    let dir = dir()?;
    let preferred = dir.join(MODEL);
    if preferred.is_file() {
        return Some(preferred);
    }
    let mut models: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ggml-")))
        .collect();
    models.sort();
    models.pop()
}

/// What's missing for you to talk to the tower, said how to fix it.
pub fn missing() -> Option<String> {
    if tower::find_program("ffmpeg").is_none() {
        return Some("talking to the tower needs ffmpeg: brew install ffmpeg".to_owned());
    }
    if tower::find_program("whisper-cli").is_none() {
        return Some("talking to the tower needs whisper.cpp: brew install whisper-cpp".to_owned());
    }
    if model().is_none() {
        return Some("no Whisper model yet: run `sasha voice install`".to_owned());
    }
    None
}

// ── Hearing you ─────────────────────────────────────────────────────────

/// You, being recorded.
pub struct Recording {
    child: Child,
    stdin: Option<ChildStdin>,
    path: PathBuf,
    pub since: Instant,
}

/// Starts recording the default microphone.
pub fn record() -> Result<Recording> {
    let ffmpeg = tower::find_program("ffmpeg").context("ffmpeg isn't installed: brew install ffmpeg")?;
    let dir = dir().context("no home folder")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("you.wav");
    let mic = std::env::var("SASHA_MIC").unwrap_or_else(|_| "default".to_owned());
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "avfoundation", "-i", &format!(":{mic}")])
        .args(["-ac", "1", "-ar", "16000", "-y"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("can't start ffmpeg")?;
    let stdin = child.stdin.take();
    Ok(Recording { child, stdin, path, since: Instant::now() })
}

impl Recording {
    /// Stops recording and gives the file, finished.
    pub fn finish(mut self) -> Result<PathBuf> {
        // q is how ffmpeg is asked to stop and close the file properly.
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"q");
        }
        let deadline = Instant::now() + STOP_WAIT;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(self.path.clone());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        bail!("the recording didn't stop")
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// What you said, as Whisper heard it, in whatever language you spoke.
pub fn transcribe(wav: &Path) -> Result<String> {
    let whisper = tower::find_program("whisper-cli").context("whisper.cpp isn't installed: brew install whisper-cpp")?;
    let model = model().context("no Whisper model yet: run `sasha voice install`")?;
    let output = Command::new(whisper)
        .args(["-l", "auto", "-nt", "-np", "-m"])
        .arg(&model)
        .arg("-f")
        .arg(wav)
        .stdin(Stdio::null())
        .output()
        .context("can't run whisper-cli")?;
    if !output.status.success() {
        bail!("whisper-cli failed: {}", String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or_default());
    }
    Ok(clean(&String::from_utf8_lossy(&output.stdout)))
}

/// Whisper's lines as one, without its marks for silence and noise.
fn clean(heard: &str) -> String {
    let words: Vec<&str> = heard
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !(line.starts_with(['[', '(']) && line.ends_with([']', ')'])))
        .collect();
    words.join(" ")
}

// ── Talking back ────────────────────────────────────────────────────────

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

// ── Setting it up ───────────────────────────────────────────────────────

/// `sasha voice install`: fetches Whisper's model.
pub fn install() -> Result<()> {
    if let Some(model) = model() {
        println!("Whisper's model is here already: {}", model.display());
        return Ok(());
    }
    let dir = dir().context("no home folder")?;
    std::fs::create_dir_all(&dir)?;
    let part = dir.join(format!("{MODEL}.part"));
    println!("Fetching Whisper's model ({MODEL}, about 570 MB)…");
    let status = Command::new("curl").args(["-fL", "--progress-bar", "-o"]).arg(&part).arg(MODEL_URL).status()?;
    if !status.success() {
        bail!("couldn't fetch the model");
    }
    std::fs::rename(&part, dir.join(MODEL))?;
    println!("Done. Prefix, then V, talks to the tower.");
    if let Some(missing) = missing() {
        println!("Still to do: {missing}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_marks_for_silence_are_left_out() {
        assert_eq!(clean(" Run the tests\n [BLANK_AUDIO]\n and tell me.\n(music)\n"), "Run the tests and tell me.");
    }

    #[test]
    fn only_the_big_picture_is_said() {
        let answer = "All three sessions are done and the tests pass.\n\n```rust\nfn main() {}\n```\n\n| a | b |";
        assert_eq!(spoken(answer), "All three sessions are done and the tests pass.");
        assert!(spoken(&"Word. ".repeat(200)).chars().count() <= SPOKEN_MAX);
    }
}
