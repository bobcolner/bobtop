//! Render-loop microbenches.
//!
//! Measures the two costs that A1 (render-on-change) is meant to optimize:
//!
//! 1. `apply_event` — what every collector sample costs (cheap, sets dirty).
//! 2. `ui::draw` against a TestBackend — what *a single full frame* costs.
//!
//! Pre-A1 we paid (1) + (2) on every 16ms tick whether anything changed
//! or not (~60 frames/sec idle). Post-A1 we only pay (2) when (1) was
//! called, i.e. at the collector cadence (~1 frame/sec at default tick).
//! These numbers let us reason about that delta concretely.
//!
//! Run: `cargo bench -p bobtop-daemon`

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use bobtop_core::sample::{
    CoreSample, CpuSample, DiskDeviceSample, DiskSample, FilesystemSample, InterfaceSample,
    LoadAverage, MemorySample, NetworkSample, ProcessInfo, ProcessSample, ProcessState,
};
use bobtop_core::MetricEvent;
use bobtop_daemon::app::App;
use bobtop_daemon::monitor_theme;
use bobtop_daemon::ui;
use bobtop_pid_attr::AttributorTier;
use bobtop_tui::LayoutPreset;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn populated_app() -> App {
    let theme = monitor_theme::load("dracula");
    let tick = Arc::new(AtomicU64::new(1500));
    let mut app = App::new(theme, LayoutPreset::Full, tick, false, true);
    app.net_tier = AttributorTier::EbpfKernel;

    // Fill histories with enough samples to exercise the same code path
    // a long-running session would hit (graph rolling, sort, net join).
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
        app.apply_event(MetricEvent::Memory(MemorySample {
            timestamp: Instant::now(),
            total_bytes: 32u64 * (1 << 30),
            used_bytes: ((9.0 + t.sin() * 1.5) * (1u64 << 30) as f64) as u64,
            available_bytes: 16u64 * (1 << 30),
            swap_total_bytes: 8 * (1 << 30),
            swap_used_bytes: 1 * (1 << 30),
            huge_pages: None,
            cached_bytes: 8u64 * (1 << 30),
            buffers_bytes: 1u64 * (1 << 30),
            free_bytes: 7u64 * (1 << 30),
            pressure: None,
            cpu_pressure: None,
            io_pressure: None,
        }));
        app.apply_event(MetricEvent::Network(NetworkSample {
            timestamp: Instant::now(),
            interfaces: vec![InterfaceSample {
                name: "eth0".into(),
                rx_bytes_per_sec: 80_000.0 + (t * 1.3).sin() * 60_000.0,
                tx_bytes_per_sec: 30_000.0 + (t * 0.9).cos() * 20_000.0,
                rx_packets_per_sec: 53.0,
                tx_packets_per_sec: 20.0,
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
            filesystems: vec![FilesystemSample {
                label: "root".into(),
                device: "nvme0n1p2".into(),
                mount_point: "/".into(),
                total_bytes: 500u64 * (1 << 30),
                used_bytes: 287 * (1 << 30),
                available_bytes: 213 * (1 << 30),
                io_utilization: Some(0.18),
                read_bytes_per_sec: Some(4_500_000.0),
                write_bytes_per_sec: Some(1_200_000.0),
            }],
        }));
    }

    let names = [
        "firefox", "chrome", "node", "cargo", "python", "active-app", "ssh", "tmux", "vim",
        "code",
    ];
    let procs: Vec<ProcessInfo> = (0..50)
        .map(|i| ProcessInfo {
            pid: 1000 + i as u32 * 17,
            parent_pid: None,
            name: names[i as usize % names.len()].into(),
            cmdline: format!("/usr/bin/{} --some --args", names[i as usize % names.len()]),
            user: "bob".into(),
            state: ProcessState::Running,
            cpu_fraction: (50.0 - i as f32) * 0.01,
            mem_rss_bytes: ((50 - i as u64) * 50) * (1 << 20),
            mem_vsz_bytes: ((50 - i as u64) * 100) * (1 << 20),
            threads: 4 + i as u32 % 8,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            disk_read_bytes_per_sec: Some((i as f64 + 1.0) * 25_000.0),
            disk_write_bytes_per_sec: Some((i as f64 + 1.0) * 10_000.0),
            cgroup: Some(if i % 3 == 0 { "user.slice" } else { "system.slice" }.into()),
            container: None,
        })
        .collect();
    app.apply_event(MetricEvent::Process(ProcessSample {
        timestamp: Instant::now(),
        processes: procs,
    }));
    // Per-pid net rates flow through `AttributionStore` now, not a
    // direct `apply_net` on App — and the render-loop bench measures
    // draw-side hot paths, not attribution plumbing, so synthetic net
    // is no longer needed.
    app
}

fn bench_apply_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_event");
    group.bench_function("cpu", |b| {
        let mut app = populated_app();
        let sample = CpuSample {
            timestamp: Instant::now(),
            aggregate_utilization: 0.42,
            cores: (0..8)
                .map(|c| CoreSample {
                    id: c,
                    utilization: 0.42,
                    frequency_mhz: Some(3200),
                    temperature_c: Some(50.0),
                })
                .collect(),
            load_average: None,
        };
        b.iter(|| {
            app.apply_event(MetricEvent::Cpu(black_box(sample.clone())));
            // Drain dirty so the loop measures steady state, not buildup.
            let _ = app.take_dirty();
        });
    });
    group.finish();
}

fn bench_full_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("ui_draw");
    group.bench_function("full_180x50", |b| {
        let app = populated_app();
        let backend = TestBackend::new(180, 50);
        let mut term = Terminal::new(backend).expect("terminal");
        b.iter(|| {
            term.draw(|f| ui::draw(f, &app)).expect("draw");
        });
    });
    group.bench_function("full_120x40", |b| {
        let app = populated_app();
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).expect("terminal");
        b.iter(|| {
            term.draw(|f| ui::draw(f, &app)).expect("draw");
        });
    });
    group.finish();
}

criterion_group!(benches, bench_apply_event, bench_full_frame);
criterion_main!(benches);
