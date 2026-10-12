mod app;
mod clipboard;
mod config;
mod input;
mod procinfo;
mod sidebar;
mod tab;
mod theme;
mod ui;

use std::io::stdout;

use anyhow::Result;
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::DefaultTerminal;
use ratatui::backend::CrosstermBackend;

fn main() -> Result<()> {
    let Some(config) = config::Config::from_args()? else { return Ok(()) };

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let result = app::App::new(config, terminal.size()?).and_then(|mut app| app.run(&mut terminal));
    restore_terminal();
    result
}

fn setup_terminal() -> Result<DefaultTerminal> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    Ok(DefaultTerminal::new(CrosstermBackend::new(stdout()))?)
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        Show
    );
}
