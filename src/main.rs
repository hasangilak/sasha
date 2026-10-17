mod activity;
mod agents;
mod app;
mod awake;
mod bars;
mod client;
mod clipboard;
mod config;
mod control;
mod files;
mod github;
mod header;
mod highlight;
mod hooks;
mod input;
mod layout;
mod links;
mod mcp;
mod mission;
mod notify;
mod panel_view;
mod ports;
mod procinfo;
mod protocol;
mod server;
mod sidebar;
mod store;
mod tab;
mod termcolors;
mod theme;
mod tower;
mod ui;
mod voice;
mod workspace;

use anyhow::Result;

use config::Command;

fn main() -> Result<()> {
    let Some(command) = config::Config::from_args()? else { return Ok(()) };
    match command {
        Command::Attach(config) => client::run(config),
        Command::Server => server::run(),
        Command::KillServer => client::kill_server(),
        Command::Tower(engine) => tower::run(engine),
        Command::Mcp => mcp::run(),
        Command::Event(source, payload) => hooks::event(&source, payload.as_deref()),
        Command::Inbox(prompt) => hooks::inbox(prompt.as_deref()),
        Command::Agent(spec) => hooks::run_agent(&spec),
        Command::Speak => voice::speak_tower(),
        Command::VoiceInstall => voice::install(),
    }
}
