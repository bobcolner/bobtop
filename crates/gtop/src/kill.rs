//! Process-kill modal types + signal sender.
//!
//! `KillRequest` is staged by `App::request_kill` and consumed by the
//! confirm dialog's Enter handler. `send_signal` calls `libc::kill` and
//! returns a human-readable outcome for the status-bar toast — kill
//! failures (perm denied, race with exit) are routine and never
//! propagate up.

/// Pending kill confirmation — when `App.ui.pending_kill` is `Some`,
/// the kill modal is showing and the user is one keypress away from
/// sending the signal. `name` is captured at request time so the modal
/// still reads sensibly if the process disappears before confirm.
#[derive(Debug, Clone)]
pub struct KillRequest {
    pub pid: u32,
    pub name: String,
    pub signal: KillSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    /// SIGTERM — polite "please exit." Process can catch and clean up.
    Term,
    /// SIGKILL — immediate, uncatchable. Last resort.
    Kill,
}

impl KillSignal {
    pub fn label(self) -> &'static str {
        match self {
            KillSignal::Term => "SIGTERM",
            KillSignal::Kill => "SIGKILL",
        }
    }

    pub fn libc_value(self) -> i32 {
        match self {
            KillSignal::Term => libc::SIGTERM,
            KillSignal::Kill => libc::SIGKILL,
        }
    }
}

/// Send `signal` to `pid` via `libc::kill`. Returns a short
/// human-readable outcome for the status-bar toast. Errors are surfaced
/// as a string but never propagated — kill failures are routine and
/// should not crash the TUI.
pub fn send_signal(pid: u32, signal: KillSignal) -> String {
    // SAFETY: libc::kill is async-signal-safe; we pass a valid (positive)
    // pid_t and a known constant signal number. Worst case the kernel
    // returns EINVAL/EPERM/ESRCH, which we surface as a string.
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal.libc_value()) };
    if rc == 0 {
        format!("sent {} to pid {}", signal.label(), pid)
    } else {
        let err = std::io::Error::last_os_error();
        format!("kill(pid={pid}, {sig}) failed: {err}", sig = signal.label())
    }
}
