//! The console, for a Windows release build, which has none of its own.
//!
//! A release build for Windows is a GUI program (`windows_subsystem` in
//! main.rs), so no console window opens with it, and its standard output
//! and error go nowhere unless whoever started it redirected them to a
//! pipe or a file. Started from a command prompt with `--script`, or with
//! arguments it can't read, the app attaches to that prompt's console
//! instead, so the script's lines and the error show there. A prompt
//! doesn't wait for a GUI program, so they print over it: `start /wait`, or
//! a pipe, keeps them in order.
//!
//! While attached, Ctrl+C in that console is ignored, so a key pressed at
//! the prompt can't end the game unsaved; the window's close button ends
//! the app. Closing the console still ends it, as Windows ends every
//! program attached to a console that closes.
//!
//! A debug build is a console program, whose output already has
//! somewhere to go, so this does nothing there. Nor does the 3DS helper
//! ever get here: it is the same executable, started with pipes for its
//! standard handles, and `main` hands it over before anything else.

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::os::windows::io::IntoRawHandle as _;

type Handle = *mut c_void;

/// `AttachConsole`'s "the parent process" (`(DWORD)-1`).
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
/// `STD_OUTPUT_HANDLE`, `(DWORD)-11`.
const STD_OUTPUT_HANDLE: u32 = -11_i32 as u32;
/// `STD_ERROR_HANDLE`, `(DWORD)-12`.
const STD_ERROR_HANDLE: u32 = -12_i32 as u32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn AttachConsole(process_id: u32) -> i32;
    fn GetStdHandle(which: u32) -> Handle;
    fn SetStdHandle(which: u32, handle: Handle) -> i32;
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

/// Whether the standard handle `which` goes nowhere.
fn unset(which: u32) -> bool {
    // SAFETY: GetStdHandle only reads the process's standard handles.
    let handle = unsafe { GetStdHandle(which) };
    // INVALID_HANDLE_VALUE is -1.
    handle.is_null() || handle as isize == -1
}

/// Attaches to the console of the program that started this one, if it
/// has one and this one's output or errors go nowhere, and points each of
/// them that goes nowhere at that console. One redirected stays where it
/// was sent.
pub fn attach_to_parent() {
    let handles = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    if !handles.into_iter().any(unset) {
        return;
    }
    // SAFETY: AttachConsole takes a process ID and changes no memory; it
    // fails if the parent has no console or this process has one.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    // SAFETY: with no handler, this only sets whether the process ignores
    // Ctrl+C.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
    }
    for which in handles {
        if !unset(which) {
            continue;
        }
        let Ok(console) = OpenOptions::new().read(true).write(true).open("CONOUT$") else {
            return;
        };
        // SAFETY: the handle is the console's, just opened, and is kept
        // open for the life of the process by being given away here. Rust's
        // printing reads the standard handles each time it writes.
        unsafe {
            SetStdHandle(which, console.into_raw_handle());
        }
    }
}
