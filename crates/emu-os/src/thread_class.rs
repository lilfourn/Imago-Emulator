//! A thread's scheduling class, for threads that work for it to take.
//!
//! On macOS a thread starts at the default quality of service whatever
//! thread starts it. A thread that hands work to helper threads and waits
//! for them, as the DS core's 3D renderer does with its bands, then waits
//! on threads a class below its own: behind other apps' work, woken late
//! and maybe on the efficiency cores. In Pokémon Platinum at 2× 3D on an
//! M5 Max shared with a busy build, a frame then took 26 ms at the median
//! and up to 250 ms, against 5 ms with an idle Mac. So helper threads take
//! the class of the thread that starts them, as GCD's queues do.
//!
//! Elsewhere there is nothing to take: Linux gives unprivileged threads no
//! higher class, and on Windows a new thread starts at normal priority,
//! which the app's own threads only raise one step.

/// The scheduling class of the thread that asked ([`ThreadClass::current`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadClass {
    /// macOS's quality of service, as its `qos_class_t` number.
    #[cfg(target_os = "macos")]
    qos: u32,
}

impl ThreadClass {
    /// The calling thread's class.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            let mut qos = 0_u32;
            // SAFETY: asks for the calling thread's own class, written as
            // the 32-bit number `qos_class_t` is into `qos`; the priority
            // is not wanted.
            let got = unsafe {
                libc::pthread_get_qos_class_np(
                    libc::pthread_self(),
                    (&raw mut qos).cast::<libc::qos_class_t>(),
                    std::ptr::null_mut(),
                )
            };
            Self {
                qos: if got == 0 { qos } else { 0 },
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self {}
    }

    /// Gives the calling thread this class, as a thread started to work
    /// for the one it came from. An unspecified class leaves it as it is.
    pub fn apply(self) {
        #[cfg(target_os = "macos")]
        {
            use libc::qos_class_t::{
                QOS_CLASS_BACKGROUND, QOS_CLASS_DEFAULT, QOS_CLASS_USER_INITIATED,
                QOS_CLASS_USER_INTERACTIVE, QOS_CLASS_UTILITY,
            };
            let class = [
                QOS_CLASS_USER_INTERACTIVE,
                QOS_CLASS_USER_INITIATED,
                QOS_CLASS_DEFAULT,
                QOS_CLASS_UTILITY,
                QOS_CLASS_BACKGROUND,
            ]
            .into_iter()
            .find(|class| *class as u32 == self.qos);
            if let Some(class) = class {
                // SAFETY: sets the calling thread's own class; it touches
                // no memory, and a refusal leaves the thread as it was.
                unsafe {
                    libc::pthread_set_qos_class_self_np(class, 0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_takes_the_class_of_the_one_that_started_it() {
        let started = std::thread::spawn(|| {
            #[cfg(target_os = "macos")]
            // SAFETY: sets this test thread's own class.
            unsafe {
                libc::pthread_set_qos_class_self_np(
                    libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
                    0,
                );
            }
            let class = ThreadClass::current();
            let helper = std::thread::spawn(move || {
                let before = ThreadClass::current();
                class.apply();
                (before, ThreadClass::current())
            });
            (class, helper.join().expect("the helper ran"))
        });
        let (class, (before, after)) = started.join().expect("the thread ran");
        assert_eq!(after, class);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                class.qos,
                libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32
            );
            // What it would have run at otherwise.
            assert_ne!(before, class, "a new thread does not inherit it");
        }
        let _ = before;
    }
}
