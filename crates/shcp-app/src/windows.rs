//! The few things only Windows itself can do: a message box when there is no window to speak
//! through, and keeping to one running copy of the app.
use std::{ffi::OsStr, os::windows::ffi::OsStrExt, thread, time::Duration, time::Instant};

type Handle = *mut std::ffi::c_void;

const MB_OK: u32 = 0;
const MB_ICONERROR: u32 = 0x10;
const SW_RESTORE: i32 = 9;
const ERROR_ALREADY_EXISTS: u32 = 183;

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(window: Handle, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn FindWindowW(class: *const u16, title: *const u16) -> Handle;
    fn IsIconic(window: Handle) -> i32;
    fn ShowWindow(window: Handle, command: i32) -> i32;
    fn SetForegroundWindow(window: Handle) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const std::ffi::c_void, owner: i32, name: *const u16) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetLastError() -> u32;
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain([0]).collect()
}

/// Says what went wrong when the app has no window, or no longer can use it. Waits for the OK.
pub fn error_box(text: &str) {
    let (text, caption) = (wide(text), wide("shcp"));
    // SAFETY: both strings end in a zero and outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Per Windows session. A development build keeps apart from the installed app.
const INSTANCE_NAME: &str = if cfg!(debug_assertions) {
    r"Local\shcp-single-instance-dev"
} else {
    r"Local\shcp-single-instance"
};
/// How long a new version waits for the one that started it to close.
const UPDATE_WAIT: Duration = Duration::from_secs(15);

/// Holds a name for as long as the process lives, which is what the name is for. Returns whether
/// this process got it.
fn hold_name(name: &str) -> bool {
    let name = wide(name);
    // SAFETY: the name ends in a zero. The handle is meant to stay open until the process ends,
    // unless someone else had the name: then it is closed here.
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() {
            // Whatever failed, it is no reason not to run.
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return false;
        }
    }
    true
}

/// Whether this is the only copy running. Two would overwrite each other's settings and tabs.
/// `after_update`: the copy being replaced is on its way out, so it is given time to leave. If it
/// does not, the update still starts rather than leave the user with nothing.
pub fn first_instance(after_update: bool) -> bool {
    if !after_update {
        return hold_name(INSTANCE_NAME);
    }
    let start = Instant::now();
    while !hold_name(INSTANCE_NAME) {
        if start.elapsed() > UPDATE_WAIT {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Brings the running copy to the front. The class keeps this from finding, say, an Explorer window
/// on a folder called shcp.
pub fn focus_running_window() {
    let (class, title) = (wide("Zed::Window"), wide("shcp"));
    // SAFETY: both strings end in a zero, and a window that closed meanwhile only makes the calls
    // fail.
    unsafe {
        let window = FindWindowW(class.as_ptr(), title.as_ptr());
        if window.is_null() {
            return;
        }
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_can_be_held_only_once() {
        let name = format!(r"Local\shcp-test-instance-{}", std::process::id());
        assert!(hold_name(&name));
        assert!(!hold_name(&name));
        assert!(!hold_name(&name));
    }
}
