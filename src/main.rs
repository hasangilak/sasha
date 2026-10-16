mod activity;
mod agents;
mod app;
mod awake;
mod bars;
mod client;
mod clipboard;
mod config;
mod files;
mod github;
mod header;
mod highlight;
mod input;
mod layout;
mod links;
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
mod ui;
mod workspace;

use anyhow::Result;

use config::Command;

fn main() -> Result<()> {
    let Some(command) = config::Config::from_args()? else { return Ok(()) };
    match command {
        Command::Attach(config) => client::run(config),
        Command::Server => server::run(),
        Command::KillServer => client::kill_server(),
    }
}
