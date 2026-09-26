//! `SIGUSR1` (the relay's poke while the supervisor runs): look again at the
//! next stopping point. Only a flag is set; the run reads it at its next
//! step or turn boundary. Elsewhere than Unix nothing can send it.

use std::sync::atomic::{AtomicBool, Ordering};

static POKED: AtomicBool = AtomicBool::new(false);

/// Installs the handler (Unix; a no-op elsewhere).
pub fn install() {
    #[cfg(unix)]
    {
        extern "C" fn on_usr1(_: libc::c_int) {
            POKED.store(true, Ordering::SeqCst);
        }
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe {
            libc::signal(libc::SIGUSR1, on_usr1 as *const () as libc::sighandler_t);
        }
    }
}

/// Whether a poke arrived since the last call (and clears it).
pub fn take_poke() -> bool {
    POKED.swap(false, Ordering::SeqCst)
}

/// Marks a poke, as the signal would (tests; other platforms).
pub fn poke() {
    POKED.store(true, Ordering::SeqCst);
}
