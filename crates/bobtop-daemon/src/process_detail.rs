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

/// Replace tabs with a single space and drop other ASCII control bytes.
/// /proc files use literal tabs as key/value separators
/// (e.g. `Name:\tbobtop`), and writing them straight into a terminal cell
/// via `Cell::set_char('\t')` produces unpredictable cursor jumps or
/// weird filler glyphs. Collapse runs of whitespace too so `Name:\t\tbobtop`
/// doesn't render as a wide gap. Allows non-ASCII (UTF-8 in cmdline
/// arguments survives intact).
pub fn sanitize_for_display<S: AsRef<str>>(s: S) -> String {
    let mut out = String::with_capacity(s.as_ref().len());
    let mut prev_was_space = false;
    for ch in s.as_ref().chars() {
        let ch = match ch {
            '\t' => ' ',
            // Strip the rest of the C0 control range (NUL, BEL, BS, ESC, etc.)
            // and DEL. These are the bytes that confuse terminals when set
            // directly into a buffer cell.
            c if c.is_control() => continue,
            c => c,
        };
        if ch == ' ' {
            if prev_was_space {
                continue;
            }
            prev_was_space = true;
        } else {
            prev_was_space = false;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sanitize_for_display;

    #[test]
    fn tabs_become_single_space() {
        assert_eq!(sanitize_for_display("Name:\tbobtop"), "Name: bobtop");
    }

    #[test]
    fn runs_of_whitespace_collapse() {
        assert_eq!(sanitize_for_display("a\t\t  b"), "a b");
    }

    #[test]
    fn other_controls_dropped() {
        assert_eq!(sanitize_for_display("hi\x07\x1b\x00there"), "hithere");
    }

    #[test]
    fn unicode_survives() {
        assert_eq!(sanitize_for_display("café — utf"), "café — utf");
    }
}
