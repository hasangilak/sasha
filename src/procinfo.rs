//! Best-effort lookup of a process's name and working directory, used to
//! give tabs useful titles ("nvim · sasha") without any shell integration.

use std::path::{Path, PathBuf};

/// A process's name. Installers that keep each version as its own file
/// (Claude Code's `~/.local/share/claude/versions/2.1.296`) leave it
/// named after the version: then it's named after its folder instead.
pub fn name(pid: i32) -> Option<String> {
    let name = comm(pid)?;
    if is_version(&name)
        && let Some(named) = exe(pid).as_deref().and_then(name_from_path)
    {
        return Some(named);
    }
    Some(name)
}

fn is_version(name: &str) -> bool {
    name.contains('.') && name.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The first folder up from a program that isn't a version or `versions`.
fn name_from_path(path: &Path) -> Option<String> {
    path.ancestors()
        .skip(1)
        .filter_map(|dir| dir.file_name()?.to_str())
        .find(|dir| !is_version(dir) && !matches!(*dir, "versions" | "bin"))
        .map(str::to_owned)
}

#[cfg(target_os = "macos")]
pub fn exe(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for `buf.len()` bytes and proc_pidpath writes at most that many.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n as usize])))
}

#[cfg(target_os = "macos")]
fn comm(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes and proc_name writes at most that many.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n > 0 {
        return Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned());
    }

    // proc_name is refused for other users' processes (e.g. setuid `top`),
    // but the short BSD info is readable for any process.
    // SAFETY: proc_bsdshortinfo is plain old data, all-zero is a valid value.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: we pass a pointer to a correctly sized, writable struct.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    let comm = info.pbsi_comm.map(|c| c as u8);
    let len = comm.iter().position(|&b| b == 0).unwrap_or(comm.len());
    (len > 0).then(|| String::from_utf8_lossy(&comm[..len]).into_owned())
}

#[cfg(target_os = "macos")]
pub fn cwd(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: proc_vnodepathinfo is plain old data, all-zero is a valid value.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: we pass a pointer to a correctly sized, writable struct.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // vip_path is a NUL-terminated MAXPATHLEN buffer split into 32x32 chunks.
    // SAFETY: the nested array is contiguous, so viewing it as bytes is sound.
    let raw: &[u8] = unsafe {
        std::slice::from_raw_parts(
            info.pvi_cdir.vip_path.as_ptr().cast::<u8>(),
            std::mem::size_of_val(&info.pvi_cdir.vip_path),
        )
    };
    let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    (len > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&raw[..len])))
}

/// The arguments a process was started with, its name first.
#[cfg(target_os = "macos")]
pub fn args(pid: i32) -> Option<Vec<String>> {
    // As much as arguments and environment can take.
    let mut max: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: mib names a valid sysctl and `max` is writable for `size` bytes.
    let n = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, (&mut max as *mut libc::c_int).cast(), &mut size, std::ptr::null_mut(), 0) };
    if n != 0 || max <= 0 {
        return None;
    }
    // argc, the program's path, NULs up to the arguments, then each one.
    let mut buf = vec![0u8; max as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: mib names a valid sysctl and buf is writable for `size` bytes.
    let n = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    if n != 0 || size < 4 {
        return None;
    }
    let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?) as usize;
    let rest = &buf[4..size];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let start = path_end + rest[path_end..].iter().position(|&b| b != 0)?;
    let args: Vec<String> = rest[start..].split(|&b| b == 0).take(argc).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    (args.len() == argc).then_some(args)
}

/// The process group `pid` is in.
#[cfg(target_os = "macos")]
pub fn group(pid: i32) -> Option<i32> {
    // SAFETY: proc_bsdshortinfo is plain old data, all-zero is a valid value.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: we pass a pointer to a correctly sized, writable struct.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    (n == size).then_some(info.pbsi_pgid as i32)
}

#[cfg(not(target_os = "macos"))]
pub fn group(pid: i32) -> Option<i32> {
    // SAFETY: getpgid has no memory-safety preconditions.
    let group = unsafe { libc::getpgid(pid) };
    (group > 0).then_some(group)
}

#[cfg(target_os = "linux")]
fn comm(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_owned())
}

#[cfg(target_os = "linux")]
pub fn exe(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "linux")]
pub fn args(pid: i32) -> Option<Vec<String>> {
    let line = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let line = line.strip_suffix(&[0]).unwrap_or(&line);
    Some(line.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
}

#[cfg(target_os = "linux")]
pub fn cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn comm(_pid: i32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn exe(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn args(_pid: i32) -> Option<Vec<String>> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn cwd(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn reads_a_process_arguments() {
        let own: Vec<String> = std::env::args().collect();
        assert_eq!(args(std::process::id() as i32), Some(own));
    }

    #[test]
    fn names_versioned_programs_after_their_folder() {
        assert!(is_version("2.1.296"));
        assert!(!is_version("claude") && !is_version("2to3") && !is_version("python3.12"));
        let claude = Path::new("/Users/me/.local/share/claude/versions/2.1.296");
        assert_eq!(name_from_path(claude).as_deref(), Some("claude"));
        let nested = Path::new("/opt/tool/1.2.0/bin/1.2.0");
        assert_eq!(name_from_path(nested).as_deref(), Some("tool"));
    }
}
