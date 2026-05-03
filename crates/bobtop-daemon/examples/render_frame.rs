//! Render a fully-populated frame against fake data so we can visually
//! verify the layout, the centered-bloom CPU/Mem graphs, the disk panel,
//! the centered net graph, and that net I/O cells show "0" for idle pids
//! and real numbers for the active one — all without a real terminal.
//!
//! Run with: `cargo run --example render_frame -p bobtop-daemon`

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use bobtop_core::sample::{
    CoreSample, CpuSample, DiskDeviceSample, DiskSample, FilesystemSample, InterfaceSample,
    LoadAverage, MemorySample, NetworkSample, ProcessInfo, ProcessSample, ProcessState,
};
use bobtop_core::MetricEvent;
use bobtop_daemon::app::App;
use bobtop_net::{AttributorTier, ProcessNetSample};
use bobtop_tui::LayoutPreset;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

fn main() -> anyhow::Result<()> {
    let theme = bobtop_tui::load_theme("dracula");
    let tick = Arc::new(AtomicU64::new(500));
    let mut app = App::new(theme, LayoutPreset::Full, tick, false, true);

    // Pretend we're at Tier 3 eBPF. apply_net wires both the tier and the
    // synthetic per-pid samples — only pid 1234 has actual traffic; the
    // synthesize-zero path should make every other process show "0".
    app.net_tier = AttributorTier::EbpfKernel;
    app.apply_net(
        vec![ProcessNetSample {
            pid: 1234,
            name: "active-app".into(),
            rx_bytes_per_sec: Some(125_000.0),
            tx_bytes_per_sec: Some(48_000.0),
            connections: vec![],
            attributor_tier: AttributorTier::EbpfKernel,
        }],
        AttributorTier::EbpfKernel,
    );

    // Feed 200 ticks so all histories are well-populated.
    for i in 0..200 {
        let t = i as f64 / 8.0;
        app.apply_event(MetricEvent::Cpu(CpuSample {
            timestamp: Instant::now(),
            aggregate_utilization: ((t.sin() * 0.4 + 0.5).clamp(0.05, 0.95)) as f32,
            cores: (0..8)
                .map(|c| CoreSample {
                    id: c,
                    utilization: ((t + c as f64 * 0.4).sin() * 0.4 + 0.5).clamp(0.0, 1.0) as f32,
                    frequency_mhz: Some(3200 + (c * 50) as u32),
                    temperature_c: Some(45.0 + c as f32 * 1.5),
                })
                .collect(),
            load_average: Some(LoadAverage { one: 1.4, five: 0.9, fifteen: 0.6 }),
        }));
        let mem_used = ((9.0 + t.sin() * 1.5) * (1u64 << 30) as f64) as u64;
        app.apply_event(MetricEvent::Memory(MemorySample {
            timestamp: Instant::now(),
            total_bytes: 32u64 * (1 << 30),
            used_bytes: mem_used,
            available_bytes: 32u64 * (1 << 30) - mem_used,
            swap_total_bytes: 8 * (1 << 30),
            swap_used_bytes: 1 * (1 << 30),
            huge_pages: None,
        }));
        let rx = ((t * 1.3).sin() * 60_000.0 + 80_000.0).max(1000.0);
        let tx = ((t * 0.9).cos() * 20_000.0 + 30_000.0).max(500.0);
        app.apply_event(MetricEvent::Network(NetworkSample {
            timestamp: Instant::now(),
            interfaces: vec![InterfaceSample {
                name: "eth0".into(),
                rx_bytes_per_sec: rx,
                tx_bytes_per_sec: tx,
                rx_packets_per_sec: rx / 1500.0,
                tx_packets_per_sec: tx / 1500.0,
                rx_errors: 0,
                tx_errors: 0,
            }],
        }));
        app.apply_event(MetricEvent::Disk(DiskSample {
            timestamp: Instant::now(),
            devices: vec![DiskDeviceSample {
                name: "nvme0n1".into(),
                read_bytes_per_sec: 4_500_000.0,
                write_bytes_per_sec: 1_200_000.0,
                read_iops: 120.0,
                write_iops: 45.0,
                utilization: 0.18,
            }],
            filesystems: vec![
                FilesystemSample {
                    label: "root".into(),
                    device: "nvme0n1p2".into(),
                    mount_point: "/".into(),
                    total_bytes: 500u64 * (1 << 30),
                    used_bytes: 287 * (1 << 30),
                    available_bytes: 213 * (1 << 30),
                    io_utilization: Some(0.18),
                    read_bytes_per_sec: Some(4_500_000.0),
                    write_bytes_per_sec: Some(1_200_000.0),
                },
                FilesystemSample {
                    label: "home".into(),
                    device: "nvme0n1p3".into(),
                    mount_point: "/home".into(),
                    total_bytes: 1024u64 * (1 << 30),
                    used_bytes: 512 * (1 << 30),
                    available_bytes: 512 * (1 << 30),
                    io_utilization: Some(0.05),
                    read_bytes_per_sec: Some(64_000.0),
                    write_bytes_per_sec: Some(128_000.0),
                },
            ],
        }));
    }
    let names = ["firefox", "chrome", "node", "cargo", "python", "active-app", "ssh", "tmux", "vim", "code"];
    let procs: Vec<ProcessInfo> = (0..10)
        .map(|i| ProcessInfo {
            pid: if i == 5 { 1234 } else { 1000 + i * 17 },
            parent_pid: None,
            name: names[i as usize].into(),
            cmdline: format!("/usr/bin/{} --some --args", names[i as usize]),
            user: "bob".into(),
            state: ProcessState::Running,
            cpu_fraction: 0.5 - (i as f32) * 0.04,
            mem_rss_bytes: ((10 - i as u64) * 200) * (1 << 20),
            mem_vsz_bytes: ((10 - i as u64) * 400) * (1 << 20),
            threads: 4 + i as u32,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            disk_read_bytes_per_sec: Some((i as f64 + 1.0) * 25_000.0),
            disk_write_bytes_per_sec: Some((i as f64 + 1.0) * 10_000.0),
        })
        .collect();
    app.apply_event(MetricEvent::Process(ProcessSample {
        timestamp: Instant::now(),
        processes: procs,
    }));

    // Switch to NetRx sort to verify the sort fix: pid 1234 (active-app)
    // should now jump to row 0 because it's the only pid with non-zero rx.
    app.proc_sort = bobtop_tui::widgets::ProcessSort::NetRx;
    app.cycle_sort(0); // re-sort with the new key

    let backend = TestBackend::new(180, 50);
    let mut term = Terminal::new(backend)?;
    term.draw(|f| bobtop_daemon::ui::draw(f, &app))?;

    print_buffer(term.backend().buffer());
    eprintln!(
        "\n[verification] tier = {:?}; sample with active pid 1234 should show \
         RX/TX = '125K'/'48K', other rows should show '0' (synthesized).",
        app.net_tier
    );
    Ok(())
}

fn print_buffer(buf: &ratatui::buffer::Buffer) {
    let area = buf.area;
    for y in 0..area.height {
        let mut last_fg = (0u8, 0u8, 0u8);
        let mut last_bg: Option<(u8, u8, u8)> = None;
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let fg = match cell.style().fg.unwrap_or(Color::Reset) {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (200, 200, 200),
            };
            let bg = match cell.style().bg {
                Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
                _ => None,
            };
            if fg != last_fg {
                print!("\x1b[38;2;{};{};{}m", fg.0, fg.1, fg.2);
                last_fg = fg;
            }
            if bg != last_bg {
                match bg {
                    Some((r, g, b)) => print!("\x1b[48;2;{r};{g};{b}m"),
                    None => print!("\x1b[49m"),
                }
                last_bg = bg;
            }
            print!("{}", cell.symbol());
        }
        println!("\x1b[0m");
    }
}
