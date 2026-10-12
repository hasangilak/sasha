//! Best-effort lookup of a process's name and working directory, used to
//! give tabs useful titles ("nvim · sasha") without any shell integration.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub fn name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes and proc_name writes at most that many.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
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

#[cfg(target_os = "linux")]
pub fn name(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_owned())
}

#[cfg(target_os = "linux")]
pub fn cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn name(_pid: i32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn cwd(_pid: i32) -> Option<PathBuf> {
    None
}
