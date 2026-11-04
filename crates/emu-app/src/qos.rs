//! Tells macOS which threads the player is waiting on.
//!
//! A thread the app starts runs at macOS's default quality of service
//! whatever thread starts it, a class below the window's own. Threads at
//! the default class wait behind any interactive work on the machine, are
//! woken later from a sleep (macOS lets their timers slip to save power),
//! and may run on the slower efficiency cores. The emulation thread must
//! finish each frame before the refresh that shows it, so it asks for the
//! window's class instead.

/// Gives the calling thread the user-interactive quality of service, the
/// class of the window's own thread: for threads whose lateness the player
/// sees or hears.
pub fn interactive() {
    #[cfg(target_os = "macos")]
    // SAFETY: this sets the calling thread's own scheduling class; it
    // touches no memory, and a refusal leaves the thread as it was.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}
