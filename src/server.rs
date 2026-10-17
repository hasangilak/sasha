//! The background server: it owns every tab, so they keep running while no
//! sasha window is open, and draws them for whichever client is attached.

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::app::{App, AppEvent};
use crate::protocol::{self, ClientMessage};

pub type ClientId = u64;

/// Set by SIGTERM, SIGHUP or SIGINT: save and stop.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// How long a new server waits for the client that started it.
const FIRST_CLIENT_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that stops reading is dropped rather than freezing every tab.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts `sasha --server` in a session of its own, so closing the terminal
/// window that started it doesn't hang it up.
pub fn spawn(socket: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("can't find the sasha executable")?;
    let log = File::create(socket.with_extension("log")).map_or_else(|_| Stdio::null(), Stdio::from);
    let mut command = Command::new(exe);
    command.arg("--server").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    if let Some(home) = std::env::home_dir() {
        command.current_dir(home);
    }
    // SAFETY: setsid is async-signal-safe, so it may run between fork and exec.
    unsafe {
        command.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    command.spawn().context("can't start the sasha server")?;
    Ok(())
}

pub fn run() -> Result<()> {
    // Logging out or shutting down sends SIGTERM: stop cleanly, so the tabs
    // are saved and come back next time.
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(signal, request_stop as *const () as libc::sighandler_t);
        }
    }
    let path = protocol::socket_path()?;
    let listener = bind(&path)?;
    let (events_tx, events) = mpsc::channel();
    let accept_tx = events_tx.clone();
    thread::spawn(move || accept(&listener, &accept_tx));

    // The client that started the server connects right away.
    let (id, hello, stream) = loop {
        match events.recv_timeout(FIRST_CLIENT_TIMEOUT) {
            Ok(AppEvent::Attach(id, hello, stream)) => break (id, hello, stream),
            Ok(AppEvent::Kill) | Err(_) => {
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            Ok(_) => {}
        }
    };
    let (cols, rows) = hello.size;
    let mut app = App::new(hello.config.clone(), Size::new(cols, rows), events_tx, events);
    app.attach(id, *hello, stream);
    let result = app.run();
    // Stop answering before the client goes, so a sasha started right away
    // starts a new server instead of reaching this one on its way out.
    let _ = std::fs::remove_file(&path);
    app.dismiss_client();
    result
}

/// Listens on `path`, clearing away a socket left by a server that died.
fn bind(path: &Path) -> Result<UnixListener> {
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                bail!("a sasha server is already running at {}", path.display());
            }
            std::fs::remove_file(path)?;
            UnixListener::bind(path)?
        }
        Err(err) => return Err(err).with_context(|| format!("can't listen on {}", path.display())),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn accept(listener: &UnixListener, events: &Sender<AppEvent>) {
    for (id, stream) in (1..).zip(listener.incoming()) {
        let Ok(stream) = stream else { continue };
        let events = events.clone();
        thread::spawn(move || read_client(id, stream, &events));
    }
}

/// Turns one client's messages into app events, until it hangs up.
fn read_client(id: ClientId, stream: UnixStream, events: &Sender<AppEvent>) {
    let Ok(input) = stream.try_clone() else { return };
    let mut input = BufReader::new(input);
    let mut stream = Some(stream);
    loop {
        let event = match protocol::receive(&mut input) {
            Ok(ClientMessage::Attach(hello)) => match stream.take() {
                Some(stream) => AppEvent::Attach(id, Box::new(hello), stream),
                None => continue,
            },
            Ok(ClientMessage::Input(event)) => AppEvent::Input(id, event),
            Ok(ClientMessage::Kill) => AppEvent::Kill,
            Ok(ClientMessage::Control(request)) => match stream.take() {
                Some(stream) => AppEvent::Control(request, stream),
                None => continue,
            },
            Err(_) => {
                let _ = events.send(AppEvent::Gone(id));
                return;
            }
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

/// The client whose terminal shows the tabs.
pub struct Attached {
    pub id: ClientId,
    pub size: Size,
    pub terminal: Terminal<CrosstermBackend<ClientWriter>>,
    /// The same socket, for the goodbye.
    stream: UnixStream,
    /// Set once the client is let go, so that ratatui's last words (showing
    /// the cursor) aren't sent to a socket that may be closed.
    gone: Arc<AtomicBool>,
}

impl Attached {
    pub fn new(id: ClientId, stream: UnixStream, size: Size) -> io::Result<Self> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let gone = Arc::new(AtomicBool::new(false));
        let terminal = blank_terminal(&stream, size, &gone)?;
        Ok(Self { id, size, terminal, stream, gone })
    }

    pub fn resize(&mut self, size: Size) -> io::Result<()> {
        self.size = size;
        // The client's terminal may have rewrapped what was on screen.
        self.terminal = blank_terminal(&self.stream, size, &self.gone)?;
        Ok(())
    }

    /// Sends bytes straight to the client's terminal.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let backend = self.terminal.backend_mut();
        backend.write_all(bytes)?;
        backend.flush()
    }

    /// Tells the client to exit, printing `message`.
    pub fn exit(mut self, message: &str) {
        let _ = self.terminal.backend_mut().flush();
        let _ = protocol::write_frame(&mut self.stream, protocol::EXIT, message.as_bytes());
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.gone.store(true, Ordering::Relaxed);
    }
}

/// A ratatui terminal of `size` drawing into `stream`, whose screen has
/// just been cleared. Fixed rather than fullscreen, and never asked to
/// `clear` or `resize`: those would ask the terminal for its size and
/// cursor, and the server has no terminal of its own to ask.
fn blank_terminal(
    stream: &UnixStream,
    size: Size,
    gone: &Arc<AtomicBool>,
) -> io::Result<Terminal<CrosstermBackend<ClientWriter>>> {
    let mut writer = ClientWriter { stream: stream.try_clone()?, buf: Vec::new(), gone: Arc::clone(gone) };
    writer.write_all(b"\x1b[H\x1b[2J")?;
    writer.flush()?;
    let viewport = Viewport::Fixed(Rect::from((Position::ORIGIN, size)));
    Terminal::with_options(CrosstermBackend::new(writer), TerminalOptions { viewport })
}

/// Collects what ratatui draws and sends it to the client as one frame
/// per flush.
pub struct ClientWriter {
    stream: UnixStream,
    buf: Vec<u8>,
    gone: Arc<AtomicBool>,
}

impl Write for ClientWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.gone.load(Ordering::Relaxed) {
            self.buf.clear();
        }
        if self.buf.is_empty() {
            return Ok(());
        }
        let result = protocol::write_frame(&mut self.stream, protocol::OUTPUT, &self.buf);
        self.buf.clear();
        result
    }
}
