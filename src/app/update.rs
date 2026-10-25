//! Updating sasha in place: prefix, then U, `sasha update`, or by itself
//! when a window opens on a sasha that has been rebuilt since it started.
//! The new sasha takes this process over (see `handoff`): every terminal
//! keeps running with what's on its screen, and the window stays open.

use std::collections::HashMap;
use std::fs;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result, bail};
use ratatui::layout::Size;

use super::{App, AppEvent};
use crate::config::Config;
use crate::handoff::{self, HandedClient, HandedGroup, HandedWorkspace, Handoff};
use crate::server::Attached;
use crate::tab::{Tab, TabId};
use crate::workspace::{Group, GroupId};
use crate::{protocol, theme};

impl App {
    /// The sasha on disk has been rebuilt since this one started.
    pub(super) fn rebuilt(&self) -> bool {
        let build = protocol::build_id();
        build != 0 && build != self.build
    }

    /// Hands everything to the sasha on disk, which carries on in this
    /// process. When it can't, says why and carries on as it was.
    pub(super) fn update(&mut self) {
        if let Err(err) = self.hand_over() {
            self.set_status(format!("couldn't update sasha: {err:#}"));
        }
    }

    fn hand_over(&mut self) -> Result<()> {
        let exe = std::env::current_exe().context("can't find the sasha executable")?;
        let listener = self.listener.context("no socket to hand over")?;
        // What the terminals have said goes along on their screens. Other
        // news waits for the new sasha, or is asked again.
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, AppEvent::Output(..) | AppEvent::Exited(_)) {
                self.handle(event);
            }
        }
        self.save();
        let groups: Vec<GroupId> = self.groups.iter().map(|g| g.id).collect();
        let mut screens = Vec::new();
        let mut hand = |tab: &mut Tab| {
            let handed = tab.handed(&groups).context("a terminal without its pty")?;
            screens.push((tab.id, tab.dump()));
            anyhow::Ok(handed)
        };
        let mut workspaces = Vec::new();
        for ws in &mut self.workspaces {
            workspaces.push(HandedWorkspace {
                path: ws.path.clone(),
                name: ws.custom_name.clone(),
                tabs: ws.tabs.iter_mut().map(&mut hand).collect::<Result<_>>()?,
                active: ws.active,
                previous: ws.previous,
                dormant: ws.dormant.clone(),
                section_hidden: ws.section_hidden,
            });
        }
        let tower = self.tower.as_mut().map(&mut hand).transpose()?;
        let client = self.client.as_ref().map(|client| HandedClient {
            id: client.id,
            fd: client.fd(),
            size: (client.size.width, client.size.height),
            config: self.config.clone(),
            colors: self.colors,
        });
        let handoff = Handoff {
            version: handoff::VERSION,
            listener,
            client,
            layout: self.layout_state(),
            groups: self
                .groups
                .iter()
                .map(|g| HandedGroup { name: g.name.clone(), color: g.color, collapsed: g.collapsed, last_active: g.last_active })
                .collect(),
            workspaces,
            current: self.current,
            tower,
            tower_focus: self.tower_focus,
            tower_inbox: self.tower_inbox.clone(),
            ended_missions: self.ended_missions.clone(),
            keep_awake: self.awake.is_some(),
            next_tab_id: self.next_tab_id,
        };
        let dir = protocol::socket_path()?.with_extension("handoff");
        let path = handoff.write(&dir, &screens)?;
        // The new sasha must be able to read it: it's asked first, so one
        // that can't leaves everything running here.
        let check = Command::new(&exe).arg("--can-take-over").arg(&path).output().context("can't run the new sasha")?;
        if !check.status.success() {
            let _ = fs::remove_dir_all(&dir);
            bail!("the new sasha can't take over: {}", String::from_utf8_lossy(&check.stderr).trim());
        }
        let fds = handoff.fds();
        for &fd in &fds {
            handoff::keep_on_exec(fd, true)?;
        }
        // The new sasha keeps the computer awake itself.
        self.awake = None;
        let err = Command::new(&exe).arg("--server").arg("--take-over").arg(&path).exec();
        // Still here: it didn't start.
        for &fd in &fds {
            let _ = handoff::keep_on_exec(fd, false);
        }
        if handoff.keep_awake {
            self.toggle_awake();
        }
        let _ = fs::remove_dir_all(&dir);
        Err(err).context("can't start the new sasha")
    }

    /// Carries on from the sasha that handed this process over: its
    /// terminals, still running, its window, and its socket.
    pub fn taken_over(
        handoff: Handoff,
        mut screens: HashMap<TabId, Vec<u8>>,
        client: Option<(HandedClient, UnixStream)>,
        events_tx: Sender<AppEvent>,
        events: Receiver<AppEvent>,
    ) -> Self {
        let (config, size) = match &client {
            Some((handed, _)) => (handed.config.clone(), Size::new(handed.size.0, handed.size.1)),
            None => (Config::default(), Size::new(80, 24)),
        };
        let mut app = App::blank(config, size, events_tx.clone(), events);
        app.listener = Some(handoff.listener);
        app.attached_before = true;
        app.restore_layout(handoff.layout);
        let mut groups = Vec::new();
        for group in handoff.groups {
            let id = app.next_group_id;
            app.next_group_id += 1;
            groups.push(id);
            let (name, color, collapsed, last_active) = (group.name, group.color, group.collapsed, group.last_active);
            app.groups.push(Group { id, name, color, collapsed, last_active });
        }
        let mut lost = 0;
        let mut adopt = |tab: handoff::HandedTab| {
            let screen = screens.remove(&tab.id).unwrap_or_default();
            let adopted = Tab::adopt(tab, &screen, &groups, events_tx.clone()).ok();
            lost += usize::from(adopted.is_none());
            adopted
        };
        for handed in handoff.workspaces {
            let index = app.add_workspace(handed.path);
            let tabs: Vec<Tab> = handed.tabs.into_iter().filter_map(&mut adopt).collect();
            let ws = &mut app.workspaces[index];
            ws.tabs.extend(tabs);
            ws.rename(handed.name);
            ws.active = handed.active.min(ws.tabs.len().saturating_sub(1));
            ws.previous = handed.previous;
            ws.dormant = handed.dormant;
            ws.section_hidden = handed.section_hidden;
        }
        app.tower = handoff.tower.and_then(&mut adopt);
        // The tower's voice does what this sasha's does, once it's quiet.
        if app.tower.is_some() {
            std::thread::spawn(crate::voice::renew);
        }
        app.tower_focus = handoff.tower_focus && app.tower.is_some();
        app.next_tab_id = handoff.next_tab_id;
        app.current = handoff.current.min(app.workspaces.len().saturating_sub(1));
        app.tower_inbox = handoff.tower_inbox;
        app.ended_missions = handoff.ended_missions;
        if handoff.keep_awake {
            app.toggle_awake();
        }
        if let Some((handed, stream)) = client {
            theme::init(handed.colors.0, handed.colors.1);
            app.colors = handed.colors;
            app.client = Attached::new(handed.id, stream, size).ok();
        }
        app.relayout(size);
        app.set_status(match lost {
            0 => "sasha updated · every terminal kept running".to_owned(),
            lost => format!("sasha updated · {lost} terminals couldn't be kept"),
        });
        app
    }
}
