//! Keeping the computer awake while the tabs work.
//!
//! On macOS the server holds the power assertions itself, through IOKit
//! (what `caffeinate` uses underneath): there's no helper process that
//! could die or be killed on its own, and macOS drops the assertions the
//! moment the server exits, crash included. On Linux a `systemd-inhibit`
//! helper holds the lock, and `check` brings it back if it goes away.

pub use imp::Awake;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CString, c_char, c_void};

    use anyhow::{Result, bail};

    type CFStringRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, text: *const c_char, encoding: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    const UTF8: u32 = 0x0800_0100;
    const LEVEL_ON: u32 = 255;
    /// No display sleep (so the screen doesn't lock), no idle sleep, and no
    /// system sleep while on power.
    const KINDS: [&str; 3] = ["PreventUserIdleDisplaySleep", "PreventUserIdleSystemSleep", "PreventSystemSleep"];

    /// The assertions held; released when dropped.
    pub struct Awake(Vec<u32>);

    impl Awake {
        pub fn start() -> Result<Self> {
            let mut held = Self(Vec::new());
            for kind in KINDS {
                held.0.push(assert(kind, "sasha is keeping the computer awake")?);
            }
            Ok(held)
        }

        /// Nothing can take the assertions away while sasha runs.
        pub fn check(&mut self) -> Result<()> {
            Ok(())
        }
    }

    impl Drop for Awake {
        fn drop(&mut self) {
            for &id in &self.0 {
                // SAFETY: `id` came from IOPMAssertionCreateWithName.
                unsafe {
                    IOPMAssertionRelease(id);
                }
            }
        }
    }

    fn cf_string(text: &str) -> Result<CFStringRef> {
        let text = CString::new(text)?;
        // SAFETY: a valid C string; the result is released by the caller.
        let string = unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) };
        if string.is_null() {
            bail!("can't make a string for IOKit");
        }
        Ok(string)
    }

    fn assert(kind: &str, name: &str) -> Result<u32> {
        let (kind_ref, name_ref) = (cf_string(kind)?, cf_string(name)?);
        let mut id = 0;
        // SAFETY: both strings are valid CFStrings, `id` is a valid out pointer.
        let status = unsafe { IOPMAssertionCreateWithName(kind_ref, LEVEL_ON, name_ref, &mut id) };
        // SAFETY: both strings were created above and aren't used again.
        unsafe {
            CFRelease(kind_ref);
            CFRelease(name_ref);
        }
        if status != 0 {
            bail!("macOS refused the {kind} assertion (error {status:#x})");
        }
        Ok(id)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::process::{Child, Command, Stdio};

    use anyhow::{Context, Result};

    /// The `systemd-inhibit` helper holding the lock.
    pub struct Awake(Child);

    impl Awake {
        pub fn start() -> Result<Self> {
            let pid = std::process::id();
            let child = Command::new("systemd-inhibit")
                .args(["--what=idle:sleep", "--who=sasha", "--why=keeping the computer awake", "tail"])
                .args([format!("--pid={pid}"), "-f".into(), "/dev/null".into()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("can't run systemd-inhibit")?;
            Ok(Self(child))
        }

        /// Starts the helper again if it has gone.
        pub fn check(&mut self) -> Result<()> {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                *self = Self::start()?;
            }
            Ok(())
        }
    }

    impl Drop for Awake {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
