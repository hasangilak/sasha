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

use serde::Deserialize;
use serde_json::{Value, json};

use anyhow::{Context, Result, bail};

use crate::agents::{self, Agent};
use crate::{store, tower};

pub mod timeline;

pub use timeline::Word;

/// The most of an answer said aloud.
const SPOKEN_MAX: usize = 400;
/// The speaker that keeps Kokoro loaded, written beside it.
const SPEAKER: &str = include_str!("voice/speaker.py");
/// Kokoro's voice unless you pick another: warm, clear, American.
const KOKORO_VOICE: &str = "af_heart";
/// How fast the voice talks unless you say: a tenth slower than its own.
const DEFAULT_PACE: f64 = 0.9;
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
    say_parts(&[(text.to_owned(), "warm".to_owned())], None);
}

/// Says `parts` one after another, each `(text, tone)` in its tone where
/// the voice can do feeling, in the voice of `person` (who then speaks
/// from now on) or of whoever spoke last.
pub fn say_parts(parts: &[(String, String)], person: Option<&str>) {
    if let Some(name) = person {
        set_current(name);
    }
    if let Some(python) = kokoro_python()
        && say_with_speaker(&python, parts, &speaking()).is_ok()
    {
        return;
    }
    let text: Vec<&str> = parts.iter().map(|(text, _)| text.as_str()).collect();
    say_with_mac(&text.join(" "));
}

/// One of the tower's voices, which you call by name: in
/// `voice/people.json`, by name.
#[derive(Clone, Debug, Deserialize)]
pub struct Person {
    #[serde(skip)]
    pub name: String,
    /// The model's voice: one of Kokoro's (`af_bella`, `bm_fable`…).
    pub voice: String,
    /// `kokoro`, plain, or `chatterbox`, with feeling.
    #[serde(default = "plain")]
    pub engine: String,
    /// Its pace, else the one in `voice/pace`.
    #[serde(default)]
    pub pace: Option<f64>,
    /// How it sounds, for the tower to know.
    #[serde(default)]
    pub about: String,
}

fn plain() -> String {
    "kokoro".to_owned()
}

/// The voices you can call by name.
pub fn people() -> Vec<Person> {
    let Some(text) = dir().and_then(|dir| std::fs::read_to_string(dir.join("people.json")).ok()) else { return Vec::new() };
    let people: std::collections::BTreeMap<String, Person> = serde_json::from_str(&text).unwrap_or_default();
    people
        .into_iter()
        .map(|(name, mut person)| {
            person.name = name;
            person
        })
        .collect()
}

/// Who speaks now: the one last called, else the first.
pub fn current_person() -> Option<Person> {
    let people = people();
    let current = dir().and_then(|dir| std::fs::read_to_string(dir.join("current")).ok()).unwrap_or_default();
    let found = people.iter().find(|p| p.name.eq_ignore_ascii_case(current.trim())).cloned();
    found.or_else(|| people.into_iter().next())
}

/// The voice a message calls, if it opens or closes with one's name:
/// "Hey Bella, …", "Fable, what's…", "…, Bella?". Misheard names a letter
/// off count ("Bela", "Fabel").
pub fn called(message: &str) -> Option<String> {
    let names: Vec<String> = people().into_iter().map(|p| p.name).collect();
    called_among(message, &names)
}

/// `called`, among `names`.
fn called_among(message: &str, names: &[String]) -> Option<String> {
    const FILLER: [&str; 12] = ["hey", "hi", "hello", "ok", "okay", "yo", "so", "and", "um", "uh", "oh", "well"];
    let words: Vec<String> = message
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let opening = words.iter().filter(|w| !FILLER.contains(&w.as_str())).take(2);
    let closing = words.last();
    opening.chain(closing).find_map(|word| names.iter().find(|name| sounds_like(word, &name.to_lowercase())).cloned())
}

/// How a message asks for quiet, if it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    /// Only that: "Bella, hush", "shh", "stop talking", "Fable, stop".
    Only,
    /// "Stop!" alone: quiet, but it may mean more (stop what's about to
    /// happen), so the tower should still hear it.
    AndMore,
}

/// Whether a message asks the voice to be quiet: one of the tower's names
/// or none, and a word for it.
pub fn quiet(message: &str) -> Option<Quiet> {
    let names: Vec<String> = people().into_iter().map(|p| p.name).collect();
    quiet_among(message, &names)
}

fn quiet_among(message: &str, names: &[String]) -> Option<Quiet> {
    const POLITE: [&str; 9] = ["hey", "ok", "okay", "please", "now", "uh", "um", "oh", "just"];
    const ONLY: [&[&str]; 12] = [
        &["hush"],
        &["shh"],
        &["shhh"],
        &["shush"],
        &["quiet"],
        &["be", "quiet"],
        &["stop", "talking"],
        &["stop", "speaking"],
        &["shut", "up"],
        &["silence"],
        &["enough"],
        &["that's", "enough"],
    ];
    let words: Vec<String> = message
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let named = words.iter().any(|w| names.iter().any(|n| sounds_like(w, &n.to_lowercase())));
    let rest: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !POLITE.contains(w) && !names.iter().any(|n| sounds_like(w, &n.to_lowercase())))
        .collect();
    if ONLY.contains(&rest.as_slice()) {
        return Some(Quiet::Only);
    }
    match rest.as_slice() {
        ["stop"] if named => Some(Quiet::Only),
        ["stop"] => Some(Quiet::AndMore),
        _ => None,
    }
}

/// The same word, or for names of four letters or more one letter off
/// (added, dropped, changed or swapped) after the same first letter:
/// "Bela" is Bella, "table" isn't Fable.
fn sounds_like(word: &str, name: &str) -> bool {
    word == name || (name.chars().count() >= 4 && word.chars().next() == name.chars().next() && edits(word, name) <= 1)
}

/// Edits between two words, a swap of neighbours counting as one.
fn edits(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// A name as it's written in a sentence.
pub fn shown(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn set_current(name: &str) {
    let person = people().into_iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()));
    if let (Some(person), Some(dir)) = (person, dir()) {
        let _ = std::fs::write(dir.join("current"), format!("{}\n", person.name));
    }
}

/// How the speaker speaks: who, in which voice, with which model, how fast.
struct Speaking {
    who: String,
    voice: String,
    engine: String,
    pace: f64,
}

/// The current person's way of speaking, else the one set in `voice/`.
fn speaking() -> Speaking {
    match current_person() {
        Some(person) => Speaking {
            pace: person.pace.filter(|p| (0.5..=1.5).contains(p)).unwrap_or_else(chosen_pace),
            who: person.name,
            voice: person.voice,
            engine: person.engine,
        },
        None => Speaking { who: String::new(), voice: speaker_voice(), engine: chosen_engine(), pace: chosen_pace() },
    }
}

/// How fast it talks: `$SASHA_VOICE_PACE`, else the first line of
/// `voice/pace`, else a tenth slower than the model's own (0.9). 1 is the
/// model's pace; smaller is slower.
fn chosen_pace() -> f64 {
    let pace = std::env::var("SASHA_VOICE_PACE").ok().or_else(|| std::fs::read_to_string(dir()?.join("pace")).ok());
    pace.and_then(|p| p.lines().next()?.trim().parse::<f64>().ok()).filter(|p| (0.5..=1.5).contains(p)).unwrap_or(DEFAULT_PACE)
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
fn say_with_speaker(python: &Path, parts: &[(String, String)], how: &Speaking) -> Result<()> {
    let dir = dir().context("no home folder")?;
    let socket = speaker_socket()?;
    let parts: Vec<Value> = parts.iter().map(|(text, tone)| json!({"text": text, "tone": tone})).collect();
    let request = json!({"say": parts, "voice": how.voice, "engine": how.engine, "pace": how.pace, "who": how.who});
    if tell_speaker(&socket, &request).is_ok() {
        return Ok(());
    }
    start_speaker(python, &dir, &socket)?;
    let started = Instant::now();
    while started.elapsed() < SPEAKER_START {
        std::thread::sleep(Duration::from_millis(200));
        if tell_speaker(&socket, &request).is_ok() {
            return Ok(());
        }
    }
    bail!("the speaker didn't start: see {}", dir.join("speaker.log").display())
}

/// The voice the speaker speaks in: yours if it's one of Kokoro's.
fn speaker_voice() -> String {
    chosen_voice().filter(|v| is_kokoro_voice(v)).unwrap_or_else(|| KOKORO_VOICE.to_owned())
}

/// Starts the speaker, which gets the chosen voice ready straight away.
fn start_speaker(python: &Path, dir: &Path, socket: &Path) -> Result<()> {
    let script = dir.join("speaker.py");
    std::fs::write(&script, SPEAKER)?;
    let log = std::fs::File::create(dir.join("speaker.log")).map_or_else(|_| Stdio::null(), Stdio::from);
    let mut command = Command::new(python);
    let how = speaking();
    command.arg(&script).arg(socket).arg(dir).arg(&how.voice).arg(&how.engine);
    command.env("SASHA_SPEAKER_VERSION", speaker_version());
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // A session of its own: it outlives whoever asked first.
    // SAFETY: setsid is async-signal-safe, so it may run between fork and exec.
    unsafe {
        command.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    command.spawn().context("can't start the speaker")?;
    Ok(())
}

/// Starts the speaker if it isn't running, without waiting for it: the
/// tower's voice is ready by the time it has something to say. One left
/// from an older sasha is replaced.
pub fn warm_up() {
    let (Some(python), Some(dir), Ok(socket)) = (kokoro_python(), dir(), speaker_socket()) else { return };
    if UnixStream::connect(&socket).is_ok() {
        if speaker_status().and_then(|s| s["version"].as_str().map(str::to_owned)).as_deref() == Some(speaker_version().as_str()) {
            return;
        }
        let _ = tell_speaker(&socket, &json!({"quit": true}));
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = start_speaker(&python, &dir, &socket);
}

/// Which speaker this sasha writes: a fingerprint of its script.
fn speaker_version() -> String {
    let hash = SPEAKER.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
}

/// What the speaker says of itself, if it's running and answers.
fn speaker_status() -> Option<Value> {
    let mut stream = UnixStream::connect(speaker_socket().ok()?).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    writeln!(stream, "{}", json!({"status": true})).ok()?;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(stream), &mut line).ok()?;
    serde_json::from_str(&line).ok()
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

/// What the speaker is playing: who, the words and their moods and when
/// each is said, how long it is so far and how far it's got, and its sound
/// levels.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Playing {
    #[serde(default)]
    pub who: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tones: Vec<String>,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub levels: Vec<f32>,
    /// When each word is said.
    #[serde(default)]
    pub words: Vec<Word>,
    /// A message from the library, played again.
    #[serde(default)]
    pub replay: bool,
}

/// What the tower's voice is doing: talking or not, and what it's playing.
pub fn now() -> Option<Playing> {
    let status = speaker_status()?;
    if status["speaking"].as_bool() != Some(true) {
        return None;
    }
    Some(serde_json::from_value(status["now"].clone()).unwrap_or_default())
}

/// A message the tower said, kept in the library.
#[derive(Clone, Debug, Deserialize)]
pub struct Clip {
    pub file: String,
    pub time: u64,
    #[serde(default)]
    pub who: String,
    #[serde(default)]
    pub parts: Vec<Value>,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub duration: f64,
    /// It was stopped before it finished.
    #[serde(default)]
    pub stopped: bool,
}

impl Clip {
    pub fn text(&self) -> String {
        let texts: Vec<&str> = self.parts.iter().filter_map(|p| p["text"].as_str()).collect();
        texts.join(" ")
    }

    pub fn tones(&self) -> Vec<String> {
        self.parts.iter().map(|p| p["tone"].as_str().unwrap_or("warm").to_owned()).collect()
    }
}

/// Every message the tower said, newest first.
pub fn library() -> Vec<Clip> {
    let Some(index) = dir().and_then(|dir| std::fs::read_to_string(dir.join("library/index.jsonl")).ok()) else {
        return Vec::new();
    };
    let mut clips: Vec<Clip> = index.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    clips.reverse();
    clips
}

/// Plays a kept message again.
pub fn replay(clip: &Clip) {
    let Some(python) = kokoro_python() else { return };
    let (Some(dir), Ok(socket)) = (dir(), speaker_socket()) else { return };
    let request = json!({"play": clip.file, "who": clip.who, "parts": clip.parts, "words": clip.words});
    if tell_speaker(&socket, &request).is_ok() {
        return;
    }
    if start_speaker(&python, &dir, &socket).is_err() {
        return;
    }
    let started = Instant::now();
    while started.elapsed() < SPEAKER_START {
        std::thread::sleep(Duration::from_millis(200));
        if tell_speaker(&socket, &request).is_ok() {
            return;
        }
    }
}

/// Whether macOS's own voice is talking for the tower.
pub fn mac_speaking() -> bool {
    let pid = dir().and_then(|dir| std::fs::read_to_string(dir.join("saying")).ok()).and_then(|pid| pid.trim().parse::<i32>().ok());
    pid.is_some_and(|pid| crate::procinfo::name(pid).as_deref() == Some("say"))
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
    say_with_speaker(&python, &[("Hello. This is how the tower sounds now.".to_owned(), "warm".to_owned())], &speaking())?;
    println!("Done. The tower speaks with Kokoro; pick another voice in {}.", dir.join("voice").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_called_at_the_start_or_the_end() {
        let names = ["bella".to_owned(), "fable".to_owned()];
        let called = |message: &str| called_among(message, &names);
        assert_eq!(called("Hey Bella, what's up?").as_deref(), Some("bella"));
        assert_eq!(called("Uh, hey, Bela, the sign page?").as_deref(), Some("bella"));
        assert_eq!(called("Fable, what has been pushed to main?").as_deref(), Some("fable"));
        assert_eq!(called("Fabel what's new").as_deref(), Some("fable"));
        assert_eq!(called("what's new on main, Fable?").as_deref(), Some("fable"));
        // Only a name that addresses, not one that's talked about, nor a
        // word that's merely close.
        assert_eq!(called("make the voice that Fable uses a bit slower please"), None);
        assert_eq!(called("tell me about the table"), None);
        assert_eq!((edits("fabel", "fable"), edits("bela", "bella")), (1, 1));
    }

    #[test]
    fn quiet_is_asked_for_by_name_or_not() {
        let names = ["bella".to_owned(), "fable".to_owned()];
        let quiet = |message: &str| quiet_among(message, &names);
        assert_eq!(quiet("hush"), Some(Quiet::Only));
        assert_eq!(quiet("Hush."), Some(Quiet::Only));
        assert_eq!(quiet("Bella hush"), Some(Quiet::Only));
        assert_eq!(quiet("Fable, hush."), Some(Quiet::Only));
        assert_eq!(quiet("hush, Bela"), Some(Quiet::Only));
        assert_eq!(quiet("Shh!"), Some(Quiet::Only));
        assert_eq!(quiet("Okay Fable, stop talking please"), Some(Quiet::Only));
        assert_eq!(quiet("Fable stop"), Some(Quiet::Only));
        // Stop alone may mean more than the voice.
        assert_eq!(quiet("Stop!"), Some(Quiet::AndMore));
        assert_eq!(quiet("stop the deploy"), None);
        assert_eq!(quiet("Bella, what's quiet today?"), None);
        assert_eq!(quiet("Hey Bella, what's up?"), None);
    }

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
