//! In-place sort for `Vec<ProcessInfo>` by every column the table
//! supports. PID is always the tiebreaker so equal-valued rows have a
//! stable order across samples (sysinfo emits in HashMap order, which
//! varies between snapshots).

use std::cmp::Ordering;

use bobtop_core::sample::ProcessInfo;
use bobtop_tui::widgets::ProcessTableSort as TableSort;

pub fn sort_processes(rows: &mut [ProcessInfo], sort: TableSort, descending: bool) {
    rows.sort_by(|a, b| {
        let primary = match sort {
            TableSort::Pid => a.pid.cmp(&b.pid),
            TableSort::Name => a.name.cmp(&b.name),
            TableSort::User => a.user.cmp(&b.user),
            TableSort::Threads => a.threads.cmp(&b.threads),
            TableSort::Mem => a.mem_rss_bytes.cmp(&b.mem_rss_bytes),
            TableSort::Cpu => a
                .cpu_fraction
                .partial_cmp(&b.cpu_fraction)
                .unwrap_or(Ordering::Equal),
            TableSort::NetRx => a
                .net_rx_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.net_rx_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::NetTx => a
                .net_tx_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.net_tx_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::DiskRead => a
                .disk_read_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.disk_read_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::DiskWrite => a
                .disk_write_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.disk_write_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
        };
        // Reverse the primary key for descending mode, but keep the PID
        // tiebreaker ascending so equal-valued rows have a deterministic
        // order across samples.
        let primary = if descending { primary.reverse() } else { primary };
        primary.then_with(|| a.pid.cmp(&b.pid))
    });
}
