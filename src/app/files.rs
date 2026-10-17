//! The files bar: the folder the terminal on screen is in, as a tree,
//! browsed with the mouse or with keys, a file shown over the pane, and
//! opened in the editor in a tab of its own.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::{Browse, TabMode};
use super::{App, AppEvent, Mode};
use crate::files::{self, Preview, Scan, Tree};
use crate::layout;
use crate::tab::Tab;
use crate::workspace;

/// How often the open folders, and what git says, are looked at again.
const FILES_EVERY: Duration = Duration::from_secs(3);
/// Trees of folders no terminal is in anymore are let go past this many.
const TREES_KEPT: usize = 32;

impl App {
    /// The folder the files bar shows: the one the terminal on screen is
    /// in (it follows `cd`), else the workspace's.
    fn files_root(&self) -> Option<&Path> {
        let ws = self.workspaces.get(self.current)?;
        Some(ws.active_tab().and_then(Tab::cwd).unwrap_or(&ws.path))
    }

    /// The folder's name for the bar's heading: inside the workspace, the
    /// way down from it.
    pub fn files_label(&self) -> String {
        let Some(root) = self.files_root() else { return String::new() };
        let ws = self.ws();
        match root.strip_prefix(&ws.path) {
            Ok(rest) if rest.as_os_str().is_empty() => ws.name.clone(),
            Ok(rest) => format!("{}/{}", ws.name, rest.display()),
            Err(_) => workspace::pretty_path(root),
        }
    }

    /// The tree of the folder the bar shows.
    pub fn tree(&self) -> Option<&Tree> {
        self.trees.get(self.files_root()?)
    }

    fn tree_mut(&mut self) -> Option<&mut Tree> {
        let root = self.files_root()?.to_path_buf();
        self.trees.get_mut(&root)
    }

    pub fn file_rows(&self) -> usize {
        self.tree().map_or(0, |tree| tree.rows().len())
    }

    pub fn files_scroll(&self) -> usize {
        self.tree().map_or(0, |tree| tree.scroll)
    }

    /// Whether keys move through the files bar.
    pub fn files_focused(&self) -> bool {
        matches!(self.mode, Mode::Files(_))
    }

    /// The file shown over the pane, if one is.
    pub fn preview(&self) -> Option<&Preview> {
        match &self.mode {
            Mode::Files(browse) => browse.preview.as_ref(),
            _ => None,
        }
    }

    fn preview_mut(&mut self) -> Option<&mut Preview> {
        match &mut self.mode {
            Mode::Files(browse) => browse.preview.as_mut(),
            _ => None,
        }
    }

    /// The highlighted path and whether it's a folder.
    fn selected_file(&self) -> Option<(PathBuf, bool)> {
        let tree = self.tree()?;
        let rows = tree.rows();
        let row = rows.get(tree.selected_row(&rows)?)?;
        Some((row.entry.path.clone(), row.entry.dir))
    }

    /// The rules for the files bar: the folder it shows has a tree,
    /// scrolled into range, and keys only go to a bar that's shown.
    pub(super) fn normalize_files(&mut self) {
        if self.layout.files_bar.is_empty() {
            if self.files_focused() {
                self.leave();
            }
            return;
        }
        let Some(root) = self.files_root().map(Path::to_path_buf) else { return };
        if !self.trees.contains_key(&root) {
            if self.trees.len() >= TREES_KEPT {
                // Keep the trees of folders terminals are in, with what's
                // open in them.
                let in_use: Vec<PathBuf> =
                    self.workspaces.iter().flat_map(|ws| &ws.tabs).filter_map(|t| t.cwd().map(Path::to_path_buf)).collect();
                self.trees.retain(|dir, _| in_use.contains(dir));
            }
            self.trees.insert(root.clone(), Tree::new(root));
            // What git says about it, straight away.
            self.files_scanned = None;
        }
        self.clamp_files_scroll(false);
    }

    fn clamp_files_scroll(&mut self, reveal: bool) {
        let height = self.layout.files_rows();
        let Some(tree) = self.tree_mut() else { return };
        let rows = tree.rows();
        let selected = reveal.then(|| tree.selected_row(&rows)).flatten();
        let count = rows.len();
        tree.scroll = layout::scrolled(tree.scroll, count, height, selected);
    }

    // ── Keeping it fresh ────────────────────────────────────────────────

    /// Looks at the open folders again, and asks git, while the bar shows.
    pub(super) fn refresh_files(&mut self) {
        // The terminal on screen may have gone to another folder.
        self.normalize_files();
        let due = self.files_scanned.is_none_or(|at| at.elapsed() >= FILES_EVERY);
        if self.client.is_some() && !self.layout.files_bar.is_empty() && due {
            self.scan_files();
        }
    }

    fn scan_files(&mut self) {
        if self.files_scanning {
            return;
        }
        let Some(tree) = self.tree() else { return };
        let (root, dirs) = (tree.root.clone(), tree.shown_dirs());
        self.files_scanning = true;
        let events = self.events_tx.clone();
        thread::spawn(move || {
            let _ = events.send(AppEvent::Files(Box::new(files::scan(root, dirs))));
        });
    }

    pub(super) fn files_loaded(&mut self, scan: Scan) {
        self.files_scanning = false;
        self.files_scanned = Some(Instant::now());
        if let Some(tree) = self.trees.get_mut(&scan.root) {
            tree.apply(scan);
        }
        self.clamp_files_scroll(false);
        // The file being read may have changed too.
        if let Some(preview) = self.preview_mut().filter(|preview| preview.stale()) {
            let scroll = preview.scroll;
            *preview = Preview::read(&preview.path);
            preview.scroll_by(scroll as isize);
        }
    }

    // ── Showing and hiding ──────────────────────────────────────────────

    /// Gives the files bar the keys, showing it first if it was hidden.
    pub(super) fn browse_files(&mut self) {
        self.files_shown = true;
        self.relayout_now();
        if self.layout.files_bar.is_empty() {
            self.set_status("the window is too narrow for the files bar");
            self.leave();
            return;
        }
        self.enter(Mode::Files(Browse::default()));
        self.normalize_files();
        if self.selected_file().is_none() {
            self.move_selection(0);
        }
    }

    pub(super) fn hide_files(&mut self) {
        self.files_shown = false;
        self.hover = None;
        self.relayout_now();
        let prefix = self.config.prefix.label();
        self.set_status(format!("files bar hidden · {prefix} F brings it back"));
    }

    pub(super) fn resize_files_bar(&mut self, column: u16) {
        let width = self.layout.files_bar.right().saturating_sub(column);
        self.files_width = width.clamp(layout::FILES_MIN, layout::FILES_MAX);
        self.relayout_now();
        self.clamp_files_scroll(false);
    }

    // ── Moving around ───────────────────────────────────────────────────

    /// Moves the highlight `delta` rows; onto a file, while one is shown,
    /// shows that one instead.
    fn move_selection(&mut self, delta: isize) {
        let Some(tree) = self.tree_mut() else { return };
        let rows = tree.rows();
        if rows.is_empty() {
            return;
        }
        let index = match tree.selected_row(&rows) {
            Some(index) => index.saturating_add_signed(delta).min(rows.len() - 1),
            None => 0,
        };
        let (path, dir) = (rows[index].entry.path.clone(), rows[index].entry.dir);
        tree.selected = Some(path.clone());
        self.clamp_files_scroll(true);
        if !dir && self.preview().is_some() {
            self.show_file(&path);
        }
    }

    /// → and ⏎: opens a folder, or shows a file. → on an open folder goes
    /// into it; ⏎ closes it.
    fn open_selected(&mut self, enter: bool) {
        let Some((path, dir)) = self.selected_file() else { return self.move_selection(0) };
        if !dir {
            return self.show_file(&path);
        }
        let open = self.tree().is_some_and(|tree| tree.is_open(&path));
        match (open, enter) {
            (true, false) => self.move_selection(1),
            _ => {
                if let Some(tree) = self.tree_mut() {
                    tree.toggle(&path);
                }
                self.clamp_files_scroll(true);
            }
        }
    }

    /// ←: closes an open folder, else goes up to the folder it's in.
    fn close_selected(&mut self) {
        let Some((path, dir)) = self.selected_file() else { return };
        let Some(tree) = self.tree_mut() else { return };
        if dir && tree.is_open(&path) {
            tree.close(&path);
        } else if let Some(parent) = path.parent().filter(|parent| *parent != tree.root) {
            tree.selected = Some(parent.to_path_buf());
        }
        self.clamp_files_scroll(true);
    }

    pub(super) fn scroll_files(&mut self, delta: isize) {
        if let Some(tree) = self.tree_mut() {
            tree.scroll = tree.scroll.saturating_add_signed(delta);
        }
        self.clamp_files_scroll(false);
    }

    /// A click on row `index`: a folder opens or closes, a file is shown
    /// over the pane (and the keys move through the bar from there).
    pub(super) fn click_file(&mut self, index: usize) {
        let Some(tree) = self.tree_mut() else { return };
        let rows = tree.rows();
        let Some(row) = rows.get(index) else { return };
        let (path, dir) = (row.entry.path.clone(), row.entry.dir);
        tree.selected = Some(path.clone());
        if dir {
            tree.toggle(&path);
            self.clamp_files_scroll(false);
        } else {
            self.show_file(&path);
        }
    }

    /// Shows a file over the pane.
    fn show_file(&mut self, path: &Path) {
        let preview = Some(Preview::read(path));
        match &mut self.mode {
            Mode::Files(browse) => browse.preview = preview,
            _ => self.enter(Mode::Files(Browse { preview })),
        }
    }

    pub(super) fn scroll_preview(&mut self, delta: isize) {
        if let Some(preview) = self.preview_mut() {
            preview.scroll_by(delta);
        }
    }

    // ── Keys ────────────────────────────────────────────────────────────

    pub(super) fn files_key(&mut self, key: KeyEvent) {
        if self.config.prefix.matches(&key) {
            self.enter(Mode::Tab(TabMode::new(Instant::now())));
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = isize::try_from(self.layout.pane.height.saturating_sub(4).max(2)).unwrap_or(2);
        let rows = isize::try_from(self.layout.files_rows().max(1)).unwrap_or(1);
        let reading = self.preview().is_some();
        match key.code {
            // Esc puts the file away first, then leaves the bar.
            KeyCode::Esc if reading => {
                if let Mode::Files(browse) = &mut self.mode {
                    browse.preview = None;
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.leave(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => self.move_selection(isize::MIN / 2),
            KeyCode::End => self.move_selection(isize::MAX / 2),
            KeyCode::Right | KeyCode::Char('l') => self.open_selected(false),
            KeyCode::Enter => self.open_selected(true),
            KeyCode::Left | KeyCode::Char('h') => self.close_selected(),
            // The file shown scrolls; without one, the tree moves a page.
            KeyCode::PageDown | KeyCode::Char(' ') if reading => self.scroll_preview(page),
            KeyCode::PageUp if reading => self.scroll_preview(-page),
            KeyCode::Char('d') if ctrl && reading => self.scroll_preview(page / 2),
            KeyCode::Char('u') if ctrl && reading => self.scroll_preview(-page / 2),
            KeyCode::Char('g') if reading => self.scroll_preview(isize::MIN / 2),
            KeyCode::Char('G') if reading => self.scroll_preview(isize::MAX / 2),
            KeyCode::PageDown => self.move_selection(rows),
            KeyCode::PageUp => self.move_selection(-rows),
            KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Char('e') => self.edit_selected(),
            KeyCode::Char('y') => self.copy_selected_path(),
            KeyCode::Char('r') => self.scan_files(),
            _ => {}
        }
    }

    /// Opens the highlighted file in `$VISUAL` or `$EDITOR` (or vi), in a
    /// tab of its own that closes with the editor.
    fn edit_selected(&mut self) {
        let Some((path, false)) = self.selected_file() else {
            self.set_status("pick a file to edit");
            return;
        };
        self.leave();
        self.open_in_editor(path);
    }

    /// Opens `path` in your `$VISUAL` or `$EDITOR`, in a new tab beside the
    /// one on screen.
    pub(super) fn open_in_editor(&mut self, path: PathBuf) {
        let dir = path.parent().map_or_else(|| self.ws().path.clone(), Path::to_path_buf);
        // The shell splits an editor given with arguments ("code -w").
        let script = r#"exec ${VISUAL:-${EDITOR:-vi}} "$1""#;
        let program = ["/bin/sh", "-c", script, "sasha"].map(OsString::from).into_iter().chain([path.into_os_string()]);
        match self.spawn_program(&dir, Some(program.collect())) {
            Ok(tab) => {
                let group = self.active_tab().and_then(|t| t.group);
                self.add_tab(tab, group);
            }
            Err(err) => self.set_status(format!("can't open the editor: {err}")),
        }
    }

    /// Copies the highlighted path, relative to the terminal's folder.
    fn copy_selected_path(&mut self) {
        let Some((path, _)) = self.selected_file() else { return };
        let root = self.files_root().unwrap_or(Path::new(""));
        let text = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
        self.copy_to_clipboard(&text);
        self.set_status(format!("copied {text}"));
    }
}
