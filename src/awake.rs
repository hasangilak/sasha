//! Keeping the computer awake while the tabs work: `caffeinate` on macOS,
//! `systemd-inhibit` on Linux.

use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

/// Keeps the computer from sleeping until it's dropped, or until sasha
/// exits, crash included.
pub struct Awake(Child);

impl Awake {
    pub fn start() -> Result<Self> {
        let (program, args) = command(std::process::id());
        let child = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("can't run {program}"))?;
        Ok(Self(child))
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// No idle, disk or system sleep (the display may still turn off); `-w`
/// ends it when sasha does.
#[cfg(target_os = "macos")]
fn command(pid: u32) -> (&'static str, Vec<String>) {
    ("caffeinate", vec!["-ims".into(), "-w".into(), pid.to_string()])
}

/// Holds an idle and sleep inhibitor while `tail` waits for sasha to exit.
#[cfg(not(target_os = "macos"))]
fn command(pid: u32) -> (&'static str, Vec<String>) {
    let args = ["--what=idle:sleep", "--who=sasha", "--why=keeping the computer awake", "tail"];
    let mut args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    args.extend([format!("--pid={pid}"), "-f".into(), "/dev/null".into()]);
    ("systemd-inhibit", args)
}
