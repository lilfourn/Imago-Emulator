//! Tells the system which threads the player is waiting on.
//!
//! A thread the app starts runs at macOS's default quality of service
//! whatever thread starts it, a class below the window's own. Threads at
//! the default class wait behind any interactive work on the machine, are
//! woken later from a sleep (macOS lets their timers slip to save power),
//! and may run on the slower efficiency cores. The emulation thread must
//! finish each frame before the refresh that shows it, so it asks for the
//! window's class instead.
//!
//! Windows has the same three ways to be late, so the emulation thread
//! asks to go before other apps' threads, to be left out of power
//! throttling (EcoQoS) and to have the system's timers tick every
//! millisecond while a game runs ([`Timers`]). Linux gives an unprivileged
//! app nothing to ask for: a thread may lower its priority but not raise
//! it, and the kernel's timers are already precise to 50 µs.

/// Gives the calling thread the user-interactive quality of service, the
/// class of the window's own thread: for threads whose lateness the player
/// sees or hears.
///
/// On Windows the thread goes one step above normal priority, ahead of
/// other apps' threads but not of the system's, and it and the app are
/// left out of power throttling. Multimedia scheduling (MMCSS) would
/// raise it further, into the real-time range, but turns on the network
/// throttling that MMCSS gives sound and video while any such thread runs,
/// which would slow the store's and the 3DS core's downloads.
///
/// On Linux it does nothing: raising a thread's priority (a negative nice
/// value) needs privileges a desktop session doesn't give, and real-time
/// scheduling through rtkit is for sound, not for a thread that may run
/// flat out at unlimited speed.
pub fn interactive() {
    #[cfg(target_os = "macos")]
    // SAFETY: this sets the calling thread's own scheduling class; it
    // touches no memory, and a refusal leaves the thread as it was.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
    #[cfg(windows)]
    win32::interactive();
}

/// Has the system's timers tick every millisecond while it is held: on
/// Windows, where they tick every 15.6 ms otherwise, so a wait for a
/// channel, a lock or a gamepad's next event could end up to that late.
/// Rust's own sleeps already use high-resolution timers there, from
/// Windows 10's 1803 update on, and so does [`crate::speed::sleep_until`].
/// Elsewhere it does nothing, as timers are already that fine.
#[derive(Debug)]
pub struct Timers {
    #[cfg(windows)]
    held: bool,
}

impl Timers {
    /// Asks for millisecond timers until the returned value is dropped.
    pub fn fine() -> Self {
        Self {
            #[cfg(windows)]
            held: win32::begin_fine_timers(),
        }
    }
}

impl Drop for Timers {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.held {
            win32::end_fine_timers();
        }
    }
}

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    use windows::Win32::Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
        SetThreadInformation, SetThreadPriority, THREAD_POWER_THROTTLING_CURRENT_VERSION,
        THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
        THREAD_PRIORITY_ABOVE_NORMAL, ThreadPowerThrottling,
    };

    /// The timer period asked for, in milliseconds.
    const PERIOD: u32 = 1;

    pub fn interactive() {
        // SAFETY: the pseudo-handle names the calling thread, and setting
        // its priority touches no memory; a refusal leaves it as it was.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) }.ok();
        // A control bit with its state bit clear turns that throttling off.
        let thread = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        // SAFETY: the pointer and size are those of `thread`, a
        // THREAD_POWER_THROTTLING_STATE, which the call only reads.
        unsafe {
            SetThreadInformation(
                GetCurrentThread(),
                ThreadPowerThrottling,
                (&raw const thread).cast::<c_void>(),
                size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
            )
        }
        .ok();
        // The whole app: Windows 11 throttles an app whose window is
        // covered or minimised, and stops honouring its timer period. Each
        // on its own, as Windows 10 knows only the first.
        for control in [
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        ] {
            let process = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: control,
                StateMask: 0,
            };
            // SAFETY: the pointer and size are those of `process`, a
            // PROCESS_POWER_THROTTLING_STATE, which the call only reads.
            unsafe {
                SetProcessInformation(
                    GetCurrentProcess(),
                    ProcessPowerThrottling,
                    (&raw const process).cast::<c_void>(),
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                )
            }
            .ok();
        }
    }

    /// Asks for millisecond timers; whether Windows took the request, which
    /// must then be matched by [`end_fine_timers`].
    pub fn begin_fine_timers() -> bool {
        // SAFETY: this only changes the app's timer period; requests are
        // counted, and each one taken is ended on drop.
        unsafe { timeBeginPeriod(PERIOD) == TIMERR_NOERROR }
    }

    /// Ends one request [`begin_fine_timers`] made.
    pub fn end_fine_timers() {
        // SAFETY: as above, for a request that was taken.
        unsafe { timeEndPeriod(PERIOD) };
    }
}
