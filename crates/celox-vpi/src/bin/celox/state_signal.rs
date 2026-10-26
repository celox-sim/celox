//! Process-local SIGUSR1 requests. Only an atomic flag is touched by the signal
//! handler; the CLI services it between complete simulation time steps.

#[cfg(unix)]
mod unix {
    use std::sync::atomic::{AtomicBool, Ordering};

    static REQUESTED: AtomicBool = AtomicBool::new(false);
    static INSTALLED: AtomicBool = AtomicBool::new(false);

    extern "C" fn request_state(_signal: libc::c_int) {
        REQUESTED.store(true, Ordering::Relaxed);
    }

    pub(super) struct Registration {
        previous: libc::sigaction,
    }

    impl Registration {
        pub(super) fn install() -> Result<Self, String> {
            if INSTALLED.swap(true, Ordering::Relaxed) {
                return Err("SIGUSR1 state capture is already installed".into());
            }
            REQUESTED.store(false, Ordering::Relaxed);
            // Safety: a zeroed sigaction is initialized below before use. The
            // handler has the C signal ABI, lives for the process lifetime, and
            // performs only a lock-free atomic store (no allocation or I/O).
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = request_state as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(libc::SIGUSR1, &action, &mut previous) != 0 {
                    INSTALLED.store(false, Ordering::Relaxed);
                    return Err(format!(
                        "cannot install SIGUSR1 state capture: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(Self { previous })
            }
        }

        pub(super) fn take_request(&self) -> bool {
            REQUESTED.swap(false, Ordering::Relaxed)
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // Safety: previous is the action returned by successful installation.
            // Restore the caller's action rather than imposing a default handler.
            unsafe {
                libc::sigaction(libc::SIGUSR1, &self.previous, std::ptr::null_mut());
            }
            INSTALLED.store(false, Ordering::Relaxed);
        }
    }
}

pub(super) struct StateSignal {
    #[cfg(unix)]
    registration: unix::Registration,
}

impl StateSignal {
    pub(super) fn install() -> Result<Self, String> {
        #[cfg(unix)]
        {
            Ok(Self {
                registration: unix::Registration::install()?,
            })
        }
        #[cfg(not(unix))]
        {
            Err("SIGUSR1 checkpoint handling requires Unix".into())
        }
    }

    pub(super) fn take_request(&self) -> bool {
        #[cfg(unix)]
        {
            self.registration.take_request()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}
