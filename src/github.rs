//! Open pull requests and what they change, through the GitHub CLI (`gh`),
//! which has to be installed and signed in.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub author: Author,
    pub additions: u64,
    pub deletions: u64,
    #[serde(rename = "changedFiles")]
    pub changed_files: u64,
    #[serde(rename = "isDraft")]
    pub draft: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Author {
    pub login: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl Change {
    pub fn letter(self) -> &'static str {
        match self {
            Change::Added => "A",
            Change::Modified => "M",
            Change::Deleted => "D",
            Change::Renamed => "R",
        }
    }
}

/// One file's part of a pull request's diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// Where a renamed file came from.
    pub old_path: Option<String>,
    pub change: Change,
    pub added: usize,
    pub removed: usize,
    /// The hunks, from the first `@@` on.
    pub lines: Vec<String>,
}

/// The repository's open pull requests, newest first.
pub fn open_prs(dir: &Path) -> Result<Vec<Pr>, String> {
    let fields = "number,title,author,additions,deletions,changedFiles,isDraft";
    let json = gh(dir, &["pr", "list", "--state", "open", "--limit", "100", "--json", fields])?;
    serde_json::from_str(&json).map_err(|err| format!("unexpected answer from gh: {err}"))
}

/// The files pull request `number` touches, with their diffs.
pub fn pr_files(dir: &Path, number: u64) -> Result<Vec<FileDiff>, String> {
    let diff = gh(dir, &["pr", "diff", &number.to_string(), "--color", "never"])?;
    Ok(parse_diff(&diff))
}

pub fn open_in_browser(dir: &Path, number: u64) -> Result<(), String> {
    gh(dir, &["pr", "view", &number.to_string(), "--web"]).map(|_| ())
}

fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "gh isn't installed · see https://cli.github.com".to_owned(),
            _ => format!("can't run gh: {err}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("gh failed");
        return Err(message.to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Splits a unified diff (as `git diff` writes it) into files.
pub fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, path)| path).to_owned();
            files.push(FileDiff {
                path,
                old_path: None,
                change: Change::Modified,
                added: 0,
                removed: 0,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        // Before the first hunk: what happened to the file.
        if file.lines.is_empty() && !line.starts_with("@@") {
            if line.starts_with("new file mode") {
                file.change = Change::Added;
            } else if line.starts_with("deleted file mode") {
                file.change = Change::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file.change = Change::Renamed;
                file.old_path = Some(from.to_owned());
            } else if line.starts_with("Binary files") {
                file.lines.push("(binary file)".to_owned());
            }
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => file.added += 1,
            Some(b'-') => file.removed += 1,
            _ => {}
        }
        file.lines.push(line.to_owned());
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_diff_into_files() {
        let diff = "\
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,2 @@
+fn main() {}
+// --- not a header
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/a b.md b/a b.md
--- a/a b.md
+++ b/a b.md
@@ -1,2 +1,2 @@
 same
-old
+new
diff --git a/x.rs b/y.rs
similarity index 90%
rename from x.rs
rename to y.rs
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
";
        let files = parse_diff(diff);
        let summary: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.change, f.added, f.removed)).collect();
        assert_eq!(summary, [
            ("src/new.rs", Change::Added, 2, 0),
            ("old.txt", Change::Deleted, 0, 1),
            ("a b.md", Change::Modified, 1, 1),
            ("y.rs", Change::Renamed, 0, 0),
            ("logo.png", Change::Modified, 0, 0),
        ]);
        assert_eq!(files[3].old_path.as_deref(), Some("x.rs"));
        assert_eq!(files[2].lines, ["@@ -1,2 +1,2 @@", " same", "-old", "+new"]);
        assert_eq!(files[4].lines, ["(binary file)"]);
    }
}
