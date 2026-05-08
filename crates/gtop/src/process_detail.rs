//! `ProcessDetail` — per-pid /proc snapshot rendered by the detail modal.
//!
//! Fired once when the user presses Enter on a process row. There is no
//! live refresh — the modal is for inspection, not monitoring. Fields
//! that fail to read (perm denied, process gone, kthread) get a
//! placeholder string instead of erroring the whole modal.

/// Snapshot of /proc data shown in the detail modal. All strings are
/// scrubbed of control bytes (especially the literal tabs in
/// `/proc/[pid]/status` and `io`) so the renderer can `set_char` on every
/// byte without producing terminal artifacts.
#[derive(Debug, Clone)]
pub struct ProcessDetail {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    pub status_lines: Vec<String>,
    pub fd_count: Result<usize, String>,
    pub io_lines: Vec<String>,
}

impl ProcessDetail {
    /// Read the relevant /proc entries for `pid`. All errors are folded
    /// into placeholders inside the returned struct — the modal opens
    /// even when individual fields are unavailable (kthreads, perms).
    pub fn read(pid: u32, name: &str) -> Self {
        let base = format!("/proc/{pid}");
        let cmdline = std::fs::read(format!("{base}/cmdline"))
            .map(|bytes| {
                // /proc cmdline uses NUL separators between argv entries.
                bytes
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map(|s| sanitize_for_display(&s))
            .unwrap_or_else(|e| format!("(cmdline unavailable: {e})"));

        let status_lines = std::fs::read_to_string(format!("{base}/status"))
            .map(|s| {
                // Pull the rows users actually care about; full status is 50+ lines.
                s.lines()
                    .filter(|l| {
                        let key = l.split(':').next().unwrap_or("");
                        matches!(
                            key,
                            "Name"
                                | "State"
                                | "Tgid"
                                | "Pid"
                                | "PPid"
                                | "Uid"
                                | "Gid"
                                | "Threads"
                                | "VmRSS"
                                | "VmSize"
                                | "voluntary_ctxt_switches"
                                | "nonvoluntary_ctxt_switches"
                        )
                    })
                    .map(sanitize_for_display)
                    .collect()
            })
            .unwrap_or_else(|e| vec![format!("(status unavailable: {e})")]);

        let fd_count = std::fs::read_dir(format!("{base}/fd"))
            .map(|it| it.count())
            .map_err(|e| e.to_string());

        let io_lines = std::fs::read_to_string(format!("{base}/io"))
            .map(|s| s.lines().map(sanitize_for_display).collect())
            .unwrap_or_else(|e| vec![format!("(io unavailable: {e})")]);

        Self {
            pid,
            name: name.to_string(),
            cmdline,
            status_lines,
            fd_count,
            io_lines,
        }
    }
}

pub use gtui::sanitize_for_display;
