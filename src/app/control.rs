//! Answering other programs, the tower's tools above all: the terminals
//! open, what they show, and typing into, opening, showing and closing
//! them.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::App;
use crate::control::{Reply, Request, Terminal};
use crate::tab::{Tab, TabId};
use crate::{input, protocol};

/// The most lines a read returns.
const MAX_LINES: usize = 5_000;

impl App {
    pub(super) fn answer(&mut self, request: Request, mut stream: UnixStream) {
        let reply = self.control(request).unwrap_or_else(Reply::Error);
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        if let Ok(json) = serde_json::to_vec(&reply) {
            let _ = protocol::write_frame(&mut stream, protocol::REPLY, &json);
        }
    }

    fn control(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::List => Ok(Reply::Terminals(self.terminals())),
            Request::Read { id, lines } => Ok(Reply::Text(self.control_tab(id)?.text(lines.clamp(1, MAX_LINES)))),
            Request::Paste { id, text } => {
                self.typing_tab(id)?.paste(&text);
                Ok(Reply::Done)
            }
            Request::Keys { id, keys } => {
                let tab = self.typing_tab(id)?;
                let mut bytes = Vec::new();
                for name in &keys {
                    let key = input::parse_key(name).ok_or_else(|| format!("no key called `{name}`"))?;
                    bytes.extend(input::encode_key(&key, tab.screen().application_cursor()));
                }
                tab.write(bytes);
                Ok(Reply::Done)
            }
            Request::Open { cwd, name, command } => {
                if !cwd.is_dir() {
                    return Err(format!("{} isn't a folder", cwd.display()));
                }
                let w = match self.workspace_for(&cwd) {
                    Some(w) => w,
                    None => self.add_workspace(cwd.clone()),
                };
                let mut tab = self.spawn_tab(&cwd).map_err(|err| format!("can't start a shell: {err}"))?;
                tab.custom_prefix = name.filter(|n| !n.trim().is_empty());
                if let Some(command) = command.filter(|c| !c.trim().is_empty()) {
                    // The shell reads it once it's up.
                    tab.write(format!("{command}\r"));
                }
                let id = tab.id;
                // In the background: the screen stays with what's on it.
                let ws = &mut self.workspaces[w];
                ws.section_hidden = false;
                ws.tabs.push(tab);
                Ok(Reply::Opened(id))
            }
            Request::Show { id } => {
                let (w, t) = self.find_tab(id).ok_or_else(|| gone(id))?;
                self.go_to(w, t);
                Ok(Reply::Done)
            }
            Request::Close { id } => {
                let (w, t) = self.find_tab(id).ok_or_else(|| gone(id))?;
                if self.workspaces[w].tabs[t].tower {
                    return Err("that's the tower's own terminal".to_owned());
                }
                self.remove_tab(w, t);
                Ok(Reply::Done)
            }
        }
    }

    /// Every terminal running, workspace by workspace.
    fn terminals(&self) -> Vec<Terminal> {
        let on_screen = self.client.is_some().then(|| self.active_tab().map(|t| t.id)).flatten();
        let mut terminals = Vec::new();
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                terminals.push(Terminal {
                    id: tab.id,
                    name: tab.prefix(),
                    workspace: ws.name.clone(),
                    group: tab.group.and_then(|g| self.group(g)).map(|g| g.name.clone()),
                    program: if tab.is_running_command() { tab.process_name().to_owned() } else { String::new() },
                    title: tab.running().filter(|title| title != tab.process_name()),
                    cwd: tab.program_cwd(),
                    quiet_ms: u64::try_from(tab.last_activity.elapsed().as_millis()).unwrap_or(u64::MAX),
                    on_screen: on_screen == Some(tab.id),
                    unseen: tab.unseen,
                    bell: tab.bell,
                    tower: tab.tower,
                });
            }
        }
        terminals
    }

    fn control_tab(&mut self, id: TabId) -> Result<&mut Tab, String> {
        let (w, t) = self.find_tab(id).ok_or_else(|| gone(id))?;
        Ok(&mut self.workspaces[w].tabs[t])
    }

    /// A terminal to type into: any but the tower's own.
    fn typing_tab(&mut self, id: TabId) -> Result<&mut Tab, String> {
        let tab = self.control_tab(id)?;
        if tab.tower {
            return Err("that's the tower's own terminal".to_owned());
        }
        Ok(tab)
    }
}

fn gone(id: TabId) -> String {
    format!("no terminal {id}: list them again")
}
