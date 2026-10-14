//! Desktop notifications, for tabs that need you while you look elsewhere.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::agents::Agent;

fn enabled() -> bool {
    std::env::var_os("SASHA_NOTIFY").is_none_or(|value| value != "0")
}

/// Shows a notification, unless `SASHA_NOTIFY=0`.
pub fn send(title: &str, body: &str) {
    if !enabled() {
        return;
    }
    let mut command = command(title, body);
    // Starting osascript takes a moment; don't hold up the tabs.
    std::thread::spawn(move || show(&mut command));
}

/// A notification about the turn an agent just finished (or is stuck in)
/// in `cwd`, told from its session log, in the tab called `place`; the
/// plain `title` and `body` when the log has nothing to say.
pub fn send_agent(agent: Agent, cwd: PathBuf, place: String, title: String, body: String) {
    if !enabled() {
        return;
    }
    std::thread::spawn(move || {
        let (title, body) = agent.report(&cwd).map_or((title, body), |report| agent.message(&report, &place));
        show(&mut command(&title, &body));
    });
}

fn show(command: &mut Command) {
    let _ = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(target_os = "macos")]
fn command(title: &str, body: &str) -> Command {
    // The text goes in as arguments, never into the script itself.
    let mut command = Command::new("osascript");
    command.args([
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title \"sasha\" subtitle (item 1 of argv)",
        "-e",
        "end run",
        title,
        body,
    ]);
    command
}

#[cfg(not(target_os = "macos"))]
fn command(title: &str, body: &str) -> Command {
    let mut command = Command::new("notify-send");
    command.args(["--app-name=sasha", title, body]);
    command
}

/// "45s", "3m 10s", "1h 5m".
pub fn duration(took: Duration) -> String {
    let secs = took.as_secs();
    match (secs / 3600, secs / 60 % 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(Duration::from_secs(45)), "45s");
        assert_eq!(duration(Duration::from_secs(190)), "3m 10s");
        assert_eq!(duration(Duration::from_secs(3900)), "1h 5m");
    }
}
