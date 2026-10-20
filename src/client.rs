//! The sasha you run: it starts the server if there isn't one, then passes
//! keys and mouse events to it and puts what it draws on the screen.
//! Closing the window ends only the client; the tabs keep running.

use std::io::{BufReader, Read, Write, stdout};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

use crate::config::Config;
use crate::protocol::{self, ClientMessage, Hello};
use crate::{server, termcolors};

const START_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(config: Config) -> Result<()> {
    let path = protocol::socket_path()?;
    // Attaching from a shell inside sasha would draw sasha inside itself.
    if std::env::var_os("SASHA").is_some_and(|inside| inside == path.as_os_str()) {
        bail!("this shell is already running inside sasha");
    }
    let stream = connect_or_start(&path)?;

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        hook(info);
    }));
    setup_terminal()?;
    let result = attach(stream, config, &path);
    restore_terminal();
    let message = result?;
    if !message.is_empty() {
        println!("{message}");
    }
    // The input thread may still be waiting for a key; don't wait for it.
    std::process::exit(0);
}

/// Shows the tabs until the server says goodbye; returns its last words.
fn attach(stream: UnixStream, config: Config, path: &Path) -> Result<String> {
    let (cols, rows) = crossterm::terminal::size()?;
    let hello = Hello {
        version: protocol::VERSION,
        build: protocol::build_id(),
        config,
        size: (cols, rows),
        colors: termcolors::query(),
        cwd: std::env::current_dir()
            .ok()
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("/")),
    };
    let mut writer = stream.try_clone()?;
    protocol::send(&mut writer, &ClientMessage::Attach(hello))?;
    thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if protocol::send(&mut writer, &ClientMessage::Input(event)).is_err() {
                break;
            }
        }
    });

    let mut input = BufReader::new(stream);
    let mut out = stdout().lock();
    loop {
        match protocol::read_frame(&mut input) {
            Ok((protocol::OUTPUT, bytes)) => {
                out.write_all(&bytes)?;
                out.flush()?;
            }
            Ok((protocol::EXIT, message)) => return Ok(String::from_utf8_lossy(&message).into_owned()),
            Ok(_) => {}
            Err(_) => {
                let log = path.with_extension("log");
                return Ok(format!("[lost the sasha server · see {}]", log.display()));
            }
        }
    }
}

/// Connects to the server, starting one when none is running.
fn connect_or_start(path: &Path) -> Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(path) {
        return Ok(stream);
    }
    server::spawn(path)?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(err) if Instant::now() >= deadline => {
                let log = path.with_extension("log");
                return Err(err).with_context(|| format!("the sasha server didn't start (see {})", log.display()));
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// `sasha update`: the running sasha becomes this one, keeping every
/// terminal and what runs in it.
pub fn update() -> Result<()> {
    crate::control::call(crate::control::Request::Update)?;
    println!("updating sasha in place · your terminals keep running");
    Ok(())
}

/// `sasha --kill-server`: closes every tab and stops the server.
pub fn kill_server() -> Result<()> {
    let path = protocol::socket_path()?;
    let Ok(mut stream) = UnixStream::connect(&path) else {
        println!("no sasha server is running");
        return Ok(());
    };
    protocol::send(&mut stream, &ClientMessage::Kill)?;
    // The connection closes when the server has exited.
    stream.set_read_timeout(Some(START_TIMEOUT))?;
    let _ = stream.read_to_end(&mut Vec::new());
    println!("stopped the sasha server");
    Ok(())
}

fn setup_terminal() -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture, EnableFocusChange)?;
    Ok(())
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        stdout(),
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        Show
    );
}
