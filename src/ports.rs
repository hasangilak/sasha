//! The TCP ports that programs running in the tabs listen on, and killing
//! those programs.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use crate::tab::TabId;

/// A listening port and the tab whose program opened it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub port: u16,
    /// Where it listens: `*` for every address, else the address.
    pub address: String,
    pub pid: i32,
    pub command: String,
    pub tab: TabId,
}

/// Ports listened on by descendants of the given tab shells
/// (`(shell pid, tab)`), sorted by port.
pub fn listening(shells: &[(i32, TabId)]) -> Result<Vec<Port>, String> {
    let lsof = run("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"])?;
    let parents = parse_ps(&run("ps", &["-axo", "pid=,ppid="])?);
    let shells: HashMap<i32, TabId> = shells.iter().copied().collect();
    let mut ports: Vec<Port> = parse_lsof(&lsof)
        .into_iter()
        .filter_map(|(pid, command, address, port)| {
            let tab = owner(pid, &parents, &shells)?;
            Some(Port { port, address, pid, command, tab })
        })
        .collect();
    ports.sort_by_key(|port| (port.port, port.pid));
    ports.dedup_by(|a, b| a.port == b.port && a.pid == b.pid);
    Ok(ports)
}

/// Kills a program for good (SIGKILL).
pub fn force_kill(pid: i32) -> Result<(), String> {
    // SAFETY: kill has no memory-safety preconditions.
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| format!("can't run {program}: {err}"))?;
    // lsof exits with 1 when nothing listens; its output is what matters.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The tab whose shell `pid` descends from, if any.
fn owner(pid: i32, parents: &HashMap<i32, i32>, shells: &HashMap<i32, TabId>) -> Option<TabId> {
    let mut current = pid;
    for _ in 0..64 {
        if let Some(&tab) = shells.get(&current) {
            return Some(tab);
        }
        current = *parents.get(&current).filter(|&&parent| parent > 1)?;
    }
    None
}

/// `ps -o pid=,ppid=` lines into child → parent.
fn parse_ps(text: &str) -> HashMap<i32, i32> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(str::parse::<i32>);
            Some((fields.next()?.ok()?, fields.next()?.ok()?))
        })
        .collect()
}

/// `lsof -F pcn` output into (pid, command, address, port).
fn parse_lsof(text: &str) -> Vec<(i32, String, String, u16)> {
    let (mut pid, mut command) = (0, String::new());
    let mut found = Vec::new();
    for line in text.lines() {
        let mut chars = line.chars();
        let (tag, value) = (chars.next(), chars.as_str());
        match tag {
            Some('p') => pid = value.parse().unwrap_or(0),
            Some('c') => command = value.to_owned(),
            Some('n') => {
                // "*:5173", "127.0.0.1:3000", "[::1]:8080"
                let Some((address, port)) = value.rsplit_once(':') else { continue };
                let Ok(port) = port.parse() else { continue };
                let address = address.trim_start_matches('[').trim_end_matches(']').to_owned();
                found.push((pid, command.clone(), address, port));
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lsof_and_finds_the_tab() {
        let lsof = "p4821\ncnode\nf23\nn*:5173\nf24\nn[::1]:5173\np900\ncControlCenter\nf9\nn*:7000\n";
        let found = parse_lsof(lsof);
        assert_eq!(found[0], (4821, "node".to_owned(), "*".to_owned(), 5173));
        assert_eq!(found[1].2, "::1");
        // node ← yarn ← zsh (the tab's shell); ControlCenter isn't in a tab.
        let parents = parse_ps("  4821  4800\n 4800  4700\n 4700  1\n  900  1\n");
        let shells = HashMap::from([(4700, 3)]);
        assert_eq!(owner(4821, &parents, &shells), Some(3));
        assert_eq!(owner(900, &parents, &shells), None);
    }
}
