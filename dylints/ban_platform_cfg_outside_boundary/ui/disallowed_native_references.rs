fn native_helper() {
    let _ = ();
}

// Never compiled on any host, but still scanned before expansion.
#[cfg(any())]
mod never_compiled {
    use std::os::unix::fs::PermissionsExt;
    use windows_sys::Win32::Foundation::HANDLE;

    fn pid() -> i32 {
        unsafe { libc::getpid() }
    }
}

fn main() {}
