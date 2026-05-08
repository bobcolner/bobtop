use bobtop_core::sample::CpuSample;
use gtui::{format_bytes, format_rate, truncate_chars};

use crate::app::{App, KillRequest, ProcessDetail};

pub(super) fn cpu_panel_title(app: &App) -> String {
    let cpu_pct = app
        .latest_cpu
        .as_ref()
        .map(|s| s.aggregate_utilization * 100.0)
        .unwrap_or(0.0);
    format!("¹cpu  CPU {:.1}%", cpu_pct)
}

pub(super) fn cpu_clock_label(tick_ms: u64) -> String {
    let show_seconds = tick_ms <= 1000;
    let fallback = if show_seconds {
        "1970-01-01 00:00:00 utc"
    } else {
        "1970-01-01 00:00 utc"
    };
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return fallback.into(),
    };
    let secs = now.as_secs().min(i64::MAX as u64) as libc::time_t;
    #[cfg(unix)]
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&secs, &mut tm).is_null() {
            return fallback.into();
        }
        if show_seconds {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02} utc",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            )
        } else {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02} utc",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
            )
        }
    }
    #[cfg(not(unix))]
    {
        let _ = secs;
        fallback.into()
    }
}

pub(super) fn cpu_cores_title(app: &App) -> String {
    app.cpu_model
        .as_deref()
        .unwrap_or("cpu")
        .to_string()
}

pub(super) fn cpu_cores_frequency(sample: &CpuSample) -> String {
    avg_frequency_label(sample)
}

pub(super) fn memory_panel_title(app: &App) -> String {
    match app.latest_mem.as_ref() {
        Some(m) if m.total_bytes > 0 => format!("²mem  {} total", format_bytes(m.total_bytes)),
        _ => "²mem".to_string(),
    }
}

pub(super) fn disk_panel_title(app: &App) -> String {
    let has_swap = app
        .latest_mem
        .as_ref()
        .map(|m| m.swap_total_bytes > 0)
        .unwrap_or(false);
    let label = if has_swap { "²disks + swap" } else { "²disks" };
    match app.latest_disk.as_ref() {
        Some(d) if !d.filesystems.is_empty() => {
            let total: u64 = d.filesystems.iter().map(|fs| fs.total_bytes).sum();
            format!("{}  {} total", label, format_bytes(total))
        }
        _ => label.to_string(),
    }
}

pub(super) fn disk_label(label: &str) -> String {
    format!("{:<6}", truncate_chars(label, 6))
}

/// Fixed-width disk I/O value string. Each rate is right-aligned in a
/// 6-cell slot so the surrounding sparkline stays a constant width
/// regardless of whether rates jump from 0 → MiB/s → back. Always
/// renders both directions — "idle" gets `0B/s` rather than a special
/// case, since the special case was the source of width changes.
pub(super) fn disk_io_value(read: Option<f64>, write: Option<f64>) -> String {
    let r = format!("{}/s", format_rate(read.unwrap_or(0.0)));
    let w = format!("{}/s", format_rate(write.unwrap_or(0.0)));
    format!("▼{:>6} ▲{:>6}", r, w)
}

pub(super) fn network_title(app: &App, counted: usize, total: usize) -> String {
    if total == 0 {
        "³net  no interfaces".to_string()
    } else {
        let scope = scope_segment(app);
        let mut t = format!("³net  {scope}  {}", app.net_tier.name());
        if !app.show_virtual_net && counted < total {
            t.push_str(&format!("  {counted}/{total}"));
        }
        t
    }
}

pub(super) fn process_title(app: &App) -> String {
    let arrow = if app.proc_sort_descending { '↓' } else { '↑' };
    let has_bw = app.net_tier.has_bandwidth();
    let build_hint = if !has_bw && !cfg!(any(feature = "ebpf", feature = "pcap")) {
        " (build w/ --features ebpf or pcap)"
    } else {
        ""
    };
    let sticky_tag = if app.sticky_proc_selection {
        " sticky:on"
    } else {
        " sticky:off"
    };
    let filter_tag = if !app.ui.filter_text.is_empty() {
        format!("  filter:\"{}\"", app.ui.filter_text)
    } else {
        String::new()
    };
    format!(
        "⁴proc  {} procs  group:{}  ←{}{}→  rx/tx: {}{}{}{}",
        app.processes_sorted.len(),
        app.group_mode.label(),
        app.proc_sort.label(),
        arrow,
        app.net_tier.name(),
        build_hint,
        sticky_tag,
        filter_tag,
    )
}

pub(super) fn boxes_overlay_title() -> &'static str {
    " boxes — show/hide panels "
}

pub(super) fn options_overlay_title() -> &'static str {
    " options "
}

pub(super) fn detail_title(d: &ProcessDetail) -> String {
    format!(" {} (pid {}) ", d.name, d.pid)
}

pub(super) fn kill_title(req: &KillRequest) -> String {
    match req.signal {
        crate::app::KillSignal::Term => " confirm — SIGTERM ".to_string(),
        crate::app::KillSignal::Kill => " ⚠ confirm — SIGKILL ".to_string(),
    }
}

pub(super) fn help_title() -> &'static str {
    " help — keybinds "
}

fn scope_segment(app: &App) -> String {
    match &app.selected_iface {
        None => "all".to_string(),
        Some(name) => truncate_chars(name, 14),
    }
}

fn avg_frequency_label(sample: &CpuSample) -> String {
    let mut sum_mhz: u64 = 0;
    let mut n: u64 = 0;
    for c in &sample.cores {
        if let Some(f) = c.frequency_mhz {
            if f > 0 {
                sum_mhz += f as u64;
                n += 1;
            }
        }
    }
    if n == 0 {
        return String::new();
    }
    let avg = (sum_mhz / n) as f64;
    if avg >= 1000.0 {
        format!("{:.1} GHz", avg / 1000.0)
    } else {
        format!("{:.0} MHz", avg)
    }
}
