//! The files bar's tree of a workspace's folder: folders are read as they're
//! opened, git says which paths changed, and files are read for the preview.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::highlight::{self, Lang, Span};

/// Never listed.
const HIDDEN: [&str; 2] = [".git", ".DS_Store"];
/// The most of a file the preview reads.
const PREVIEW_MAX: u64 = 1024 * 1024;

pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
}

/// A row of the tree: an entry, and how many folders deep it is.
#[derive(Clone, Copy)]
pub struct TreeRow<'a> {
    pub entry: &'a Entry,
    pub depth: usize,
}

pub struct Tree {
    pub root: PathBuf,
    /// The folders read so far, the root among them.
    dirs: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    /// The highlighted path, while the bar has the keys.
    pub selected: Option<PathBuf>,
    /// First row shown.
    pub scroll: usize,
    pub marks: Marks,
    /// Why the root couldn't be read, if it couldn't.
    pub error: Option<String>,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            scroll: 0,
            marks: Marks::default(),
            error: None,
        };
        if let Err(err) = tree.read(&root) {
            tree.error = Some(err.to_string());
        }
        tree
    }

    /// What's shown: the root's entries, with each open folder's inside it.
    pub fn rows(&self) -> Vec<TreeRow<'_>> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows<'a>(&'a self, dir: &Path, depth: usize, rows: &mut Vec<TreeRow<'a>>) {
        let Some(entries) = self.dirs.get(dir) else { return };
        for entry in entries {
            rows.push(TreeRow { entry, depth });
            if entry.dir && self.expanded.contains(&entry.path) {
                self.push_rows(&entry.path, depth + 1, rows);
            }
        }
    }

    pub fn is_open(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// Opens a folder (reading it afresh) or closes it.
    pub fn toggle(&mut self, dir: &Path) {
        if self.expanded.remove(dir) {
            return;
        }
        // Unreadable folders open empty.
        let _ = self.read(dir);
        self.expanded.insert(dir.to_path_buf());
    }

    pub fn close(&mut self, dir: &Path) {
        self.expanded.remove(dir);
    }

    fn read(&mut self, dir: &Path) -> io::Result<()> {
        let (entries, result) = match read_dir(dir) {
            Ok(entries) => (entries, Ok(())),
            Err(err) => (Vec::new(), Err(err)),
        };
        self.dirs.insert(dir.to_path_buf(), entries);
        result
    }

    /// The folders a refresh reads again: the root and the open ones.
    pub fn shown_dirs(&self) -> Vec<PathBuf> {
        std::iter::once(self.root.clone()).chain(self.expanded.iter().cloned()).collect()
    }

    /// Takes in what a refresh found. Folders that have gone close.
    pub fn apply(&mut self, scan: Scan) {
        for (dir, entries) in scan.dirs {
            match entries {
                Some(entries) => {
                    self.dirs.insert(dir, entries);
                }
                None if dir == self.root => self.error = Some("can't read this folder".to_owned()),
                None => {
                    self.dirs.remove(&dir);
                    self.expanded.remove(&dir);
                }
            }
        }
        if self.dirs.contains_key(&self.root) && scan.root_ok {
            self.error = None;
        }
        self.marks = scan.marks;
    }

    /// Index of the highlighted row, if it's shown.
    pub fn selected_row(&self, rows: &[TreeRow]) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        rows.iter().position(|row| row.entry.path == selected)
    }
}

/// A folder's entries, folders first, each part sorted by name.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if HIDDEN.contains(&name.as_str()) {
                return None;
            }
            let path = e.path();
            let dir = e.file_type().is_ok_and(|t| t.is_dir() || (t.is_symlink() && path.is_dir()));
            Some(Entry { path, name, dir })
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    Ok(entries)
}

/// A refresh, read in the background: folders afresh (`None` where one
/// can't be read anymore) and what git says.
pub struct Scan {
    pub root: PathBuf,
    root_ok: bool,
    dirs: Vec<(PathBuf, Option<Vec<Entry>>)>,
    marks: Marks,
}

pub fn scan(root: PathBuf, dirs: Vec<PathBuf>) -> Scan {
    let dirs: Vec<_> = dirs
        .into_iter()
        .map(|dir| {
            let entries = read_dir(&dir).ok();
            (dir, entries)
        })
        .collect();
    let root_ok = dirs.iter().any(|(dir, entries)| *dir == root && entries.is_some());
    let marks = git_marks(&root);
    Scan { root, root_ok, dirs, marks }
}

// ── What git says ───────────────────────────────────────────────────────

/// A path's state in git, from least to most worth seeing: a folder shows
/// the most worrying of what's inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    Ignored,
    Untracked,
    Added,
    Modified,
    Conflict,
}

impl Mark {
    pub fn letter(self) -> &'static str {
        match self {
            Mark::Ignored => "",
            Mark::Untracked => "U",
            Mark::Added => "A",
            Mark::Modified => "M",
            Mark::Conflict => "C",
        }
    }
}

#[derive(Debug, Default)]
pub struct Marks {
    /// The repository's top folder; the paths below are relative to it.
    top: PathBuf,
    /// Changed paths, and the folders they're in.
    paths: HashMap<PathBuf, Mark>,
    /// Untracked and ignored folders: everything inside them is too.
    folders: Vec<(PathBuf, Mark)>,
}

impl Marks {
    pub fn of(&self, path: &Path) -> Option<Mark> {
        let rel = path.strip_prefix(&self.top).ok()?;
        if let Some(mark) = self.paths.get(rel) {
            return Some(*mark);
        }
        self.folders.iter().find(|(dir, _)| rel.starts_with(dir)).map(|(_, mark)| *mark)
    }
}

/// What git says about the repository `dir` is in; nothing outside one.
fn git_marks(dir: &Path) -> Marks {
    let git = |args: &[&str]| {
        Command::new("git")
            // Polling mustn't take the index lock from the user's own git.
            .args(["--no-optional-locks", "-C"])
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| out.stdout)
    };
    let Some(top) = git(&["rev-parse", "--show-toplevel"]) else { return Marks::default() };
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim_end_matches('\n'));
    let status = git(&["status", "--porcelain=v1", "-z", "--ignored=matching"]).unwrap_or_default();
    parse_status(top, &status)
}

/// `git status --porcelain=v1 -z` output.
fn parse_status(top: PathBuf, status: &[u8]) -> Marks {
    let mut marks = Marks { top, ..Marks::default() };
    let mut fields = status.split(|&b| b == 0);
    while let Some(field) = fields.next() {
        let (Some(&x), Some(&y), Some(path)) = (field.first(), field.get(1), field.get(3..)) else { continue };
        // A rename or copy is followed by where it came from.
        if matches!(x, b'R' | b'C') {
            fields.next();
        }
        let mark = match (x, y) {
            (b'!', b'!') => Mark::Ignored,
            (b'?', b'?') => Mark::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Mark::Conflict,
            (b'A', _) => Mark::Added,
            _ => Mark::Modified,
        };
        let path = String::from_utf8_lossy(path);
        let rel = PathBuf::from(path.trim_end_matches('/'));
        if path.ends_with('/') {
            marks.folders.push((rel.clone(), mark));
        }
        if mark != Mark::Ignored {
            for folder in rel.ancestors().skip(1).filter(|f| !f.as_os_str().is_empty()) {
                upgrade(&mut marks.paths, folder.to_path_buf(), mark);
            }
        }
        upgrade(&mut marks.paths, rel, mark);
    }
    marks
}

fn upgrade(paths: &mut HashMap<PathBuf, Mark>, path: PathBuf, mark: Mark) {
    let old = paths.entry(path).or_insert(mark);
    *old = (*old).max(mark);
}

// ── The preview ─────────────────────────────────────────────────────────

/// A file shown over the pane.
pub struct Preview {
    pub path: PathBuf,
    /// Its lines, ready to draw, or why there are none to show.
    pub lines: Result<Vec<String>, String>,
    /// Its language, and each line's colours in it.
    pub lang: Option<Lang>,
    pub spans: Vec<Vec<Span>>,
    /// Only the first `PREVIEW_MAX` bytes were read.
    pub truncated: bool,
    /// First line shown.
    pub scroll: usize,
    /// When it was changed, and its size, as read.
    stamp: Option<(SystemTime, u64)>,
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl Preview {
    pub fn read(path: &Path) -> Self {
        let mut preview = Self {
            path: path.to_path_buf(),
            lines: Ok(Vec::new()),
            lang: None,
            spans: Vec::new(),
            truncated: false,
            scroll: 0,
            stamp: stamp(path),
        };
        let mut bytes = Vec::new();
        let read = File::open(path).and_then(|file| file.take(PREVIEW_MAX + 1).read_to_end(&mut bytes));
        if let Err(err) = read {
            preview.lines = Err(format!("can't read it: {err}"));
            return preview;
        }
        if bytes.iter().take(8000).any(|&b| b == 0) {
            let size = fs::metadata(path).map_or(bytes.len() as u64, |m| m.len());
            preview.lines = Err(format!("a binary file · {}", human_size(size)));
            return preview;
        }
        preview.truncated = bytes.len() as u64 > PREVIEW_MAX;
        bytes.truncate(PREVIEW_MAX as usize);
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<String> = text.lines().map(printable).collect();
        preview.lang = Lang::detect(path, lines.first().map_or("", String::as_str));
        if let Some(lang) = preview.lang {
            preview.spans = highlight::highlight(lang, &lines);
        }
        preview.lines = Ok(lines);
        preview
    }

    /// Whether the file has changed since it was read.
    pub fn stale(&self) -> bool {
        stamp(&self.path) != self.stamp
    }

    pub fn line_count(&self) -> usize {
        self.lines.as_ref().map_or(0, Vec::len)
    }

    /// Moves `delta` lines, keeping at least the last line in view.
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta).min(self.line_count().saturating_sub(1));
    }
}

/// A line made safe to draw: tabs become spaces, control characters go.
pub fn printable(line: &str) -> String {
    line.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_marks_files_and_the_folders_they_are_in() {
        let top = PathBuf::from("/repo");
        let status = b"R  moved.txt\0top.txt\0 M sub/a.txt\0?? sub/deep/\0!! target/\0A  sub/b.rs\0UU c.rs\0";
        let marks = parse_status(top, status);
        let of = |p: &str| marks.of(Path::new(p));
        assert_eq!(of("/repo/moved.txt"), Some(Mark::Modified));
        assert_eq!(of("/repo/sub/a.txt"), Some(Mark::Modified));
        assert_eq!(of("/repo/sub/b.rs"), Some(Mark::Added));
        // A folder shows the most worrying of what's in it.
        assert_eq!(of("/repo/sub"), Some(Mark::Modified));
        assert_eq!(of("/repo/sub/deep"), Some(Mark::Untracked));
        assert_eq!(of("/repo/sub/deep/new.txt"), Some(Mark::Untracked));
        assert_eq!(of("/repo/target/debug/sasha"), Some(Mark::Ignored));
        assert_eq!(of("/repo/c.rs"), Some(Mark::Conflict));
        assert_eq!(of("/repo/src/main.rs"), None);
        assert_eq!(of("/elsewhere/x"), None);
    }

    #[test]
    fn folders_open_lazily_and_list_folders_first() {
        let root = std::env::temp_dir().join(format!("sasha-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/app")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("README.md"), "hi\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/b.bin"), [0u8, 1, 2]).unwrap();

        let mut tree = Tree::new(root.clone());
        let names = |tree: &Tree| tree.rows().iter().map(|r| (r.entry.name.clone(), r.depth)).collect::<Vec<_>>();
        assert_eq!(names(&tree), [("src".to_owned(), 0), ("README.md".to_owned(), 0)]);
        tree.toggle(&root.join("src"));
        assert_eq!(names(&tree)[1..4], [("app".to_owned(), 1), ("b.bin".to_owned(), 1), ("main.rs".to_owned(), 1)]);
        tree.toggle(&root.join("src"));
        assert_eq!(tree.rows().len(), 2);

        let text = Preview::read(&root.join("src/main.rs"));
        assert_eq!(text.lines, Ok(vec!["fn main() {}".to_owned()]));
        assert_eq!(Preview::read(&root.join("src/b.bin")).lines, Err("a binary file · 3 bytes".to_owned()));
        fs::remove_dir_all(&root).unwrap();
    }
}
