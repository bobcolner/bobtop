//! bobtop daemon — wires CLI parsing, capability detection, collector spawn,
//! TUI render loop, and signal handling.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use bobtop_collectors::{
    CpuCollector, DiskCollector, MemoryCollector, NetworkGlobalCollector, ProcessCollector,
};
use bobtop_core::{Box, BoxesEnabled, Collector, DataBus, MetricEvent};
use clap::{parser::ValueSource, CommandFactory, FromArgMatches};
use tokio::sync::broadcast;

use bobtop_daemon::cli::CornerChoice;
use bobtop_daemon::config::Config;
use bobtop_net::{select as select_attributor, NetworkAttributor, SelectOptions};
use bobtop_tui::{builtin_names, load_theme, LayoutPreset};

use bobtop_daemon::app::App;
use bobtop_daemon::cli::{Cli, LayoutChoice};
use bobtop_daemon::tui;

#[tokio::main]
async fn main() -> Result<()> {
    // Parse CLI via get_matches() so we can ask clap which fields the user
    // actually set on the command line vs. left at the default. That lets
    // us implement strict CLI > config-file > default precedence below.
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("parsed Cli matches");
    init_tracing();

    if cli.list_themes {
        for name in builtin_names() {
            println!("{name}");
        }
        return Ok(());
    }

    let cfg = Config::load_or_default();
    let eff = resolve(&cli, &matches, &cfg);

    let theme = load_theme(&eff.theme);
    tracing::info!(theme = %theme.name, "loaded theme");

    let layout_preset = match eff.layout {
        LayoutChoice::Full => LayoutPreset::Full,
        LayoutChoice::Minimal => LayoutPreset::Minimal,
    };

    // Capability detection — pick the highest-tier network attributor available.
    let attributor: Arc<dyn NetworkAttributor> = Arc::from(select_attributor(SelectOptions {
        allow_ebpf: !eff.no_ebpf,
        allow_pcap: !eff.no_pcap,
    }));
    let net_tier = attributor.tier();
    // Self-diagnosing startup line — always logged at warn so users see it
    // even at default RUST_LOG. Helps explain why per-pid net columns are
    // empty without requiring `RUST_LOG=info`.
    tracing::warn!(
        compiled_features = features_str(),
        selected_tier = net_tier.name(),
        per_pid_bandwidth = net_tier.has_bandwidth(),
        "bobtop start"
    );

    let bus = DataBus::default();
    let tick_ms = Arc::new(AtomicU64::new(eff.tick_ms_clamped()));
    let mut app = App::new(
        theme,
        layout_preset,
        Arc::clone(&tick_ms),
        eff.tty,
        eff.show_virtual_net,
    );
    app.net_tier = net_tier;
    app.corner_style = eff.corners.into();
    let boxes = app.boxes.clone();
    let app = Arc::new(Mutex::new(app));

    // Single tick driver fans out to every collector via a broadcast
    // channel. One sleep instead of five — collectors all wake on the same
    // boundary, eliminating the alignment-window CPU spikes that
    // independent timers produce. Live `+`/`-` adjustments are picked up
    // by the driver on the *next* iteration, just like before.
    let (tick_tx, _) = broadcast::channel::<()>(4);
    spawn_tick_driver(tick_tx.clone(), Arc::clone(&tick_ms));

    // Collectors consult `boxes` and skip the actual `collect()` call when
    // their panel is hidden, which keeps idle CPU low under narrow layouts
    // (matches btop's preset model).
    let cpu = Arc::new(CpuCollector::new());
    let mem = Arc::new(MemoryCollector::new());
    let proc = Arc::new(ProcessCollector::new());
    let net = Arc::new(NetworkGlobalCollector::new());
    let disk = Arc::new(DiskCollector::new());
    spawn_collector(cpu, Box::Cpu, bus.clone(), tick_tx.subscribe(), boxes.clone());
    spawn_collector(mem, Box::Memory, bus.clone(), tick_tx.subscribe(), boxes.clone());
    spawn_collector(proc, Box::Process, bus.clone(), tick_tx.subscribe(), boxes.clone());
    spawn_collector(net, Box::Network, bus.clone(), tick_tx.subscribe(), boxes.clone());
    spawn_collector(disk, Box::Disk, bus.clone(), tick_tx.subscribe(), boxes.clone());

    // Network attribution sampler — separate channel because ProcessNetSample
    // doesn't fit into MetricEvent (cross-crate dep direction). Gated on the
    // PROC box (per-pid net is rendered in the process table) — when PROC is
    // hidden, the eBPF/pcap drain stays parked.
    spawn_attributor_loop(
        Arc::clone(&attributor),
        Arc::clone(&app),
        Arc::clone(&tick_ms),
        boxes.clone(),
    );

    // Input thread (blocking crossterm reads).
    let input_rx = tui::spawn_input_thread();

    // Terminal lifecycle. Always restore on exit, even on error.
    let mut term = tui::init_terminal()?;
    let result = tui::run(&mut term, app, bus, input_rx).await;
    tui::restore_terminal(&mut term)?;

    result.map_err(Into::into)
}

/// Resolved settings after merging CLI > config-file > clap defaults.
/// Each field has a single concrete value (no Option) so downstream code
/// doesn't repeat the override-logic. Constructed via `resolve()`.
struct EffectiveConfig {
    theme: String,
    tick_ms: u64,
    layout: LayoutChoice,
    no_ebpf: bool,
    no_pcap: bool,
    tty: bool,
    show_virtual_net: bool,
    corners: CornerChoice,
}

impl EffectiveConfig {
    fn tick_ms_clamped(&self) -> u64 {
        self.tick_ms
            .clamp(bobtop_daemon::cli::MIN_TICK_MS, bobtop_daemon::cli::MAX_TICK_MS)
    }
}

/// Merge CLI > config-file > clap-default per field. Uses clap's
/// `value_source` to detect "user passed this on the command line" vs.
/// "clap filled in the default" — the file should override defaults but
/// not user input.
fn resolve(cli: &Cli, matches: &clap::ArgMatches, cfg: &Config) -> EffectiveConfig {
    let from_cli = |name: &str| {
        matches.value_source(name) == Some(ValueSource::CommandLine)
    };
    EffectiveConfig {
        theme: if from_cli("theme") {
            cli.theme.clone()
        } else {
            cfg.theme.clone().unwrap_or_else(|| cli.theme.clone())
        },
        tick_ms: if from_cli("tick_ms") {
            cli.tick_ms
        } else {
            cfg.tick_ms.unwrap_or(cli.tick_ms)
        },
        layout: if from_cli("layout") {
            cli.layout
        } else {
            cfg.layout.unwrap_or(cli.layout)
        },
        // Bool flags: CLI sets-true. If CLI didn't pass, fall to config; if
        // config silent, fall to false (the clap default for missing flag).
        no_ebpf: if from_cli("no_ebpf") {
            cli.no_ebpf
        } else {
            cfg.no_ebpf.unwrap_or(cli.no_ebpf)
        },
        no_pcap: if from_cli("no_pcap") {
            cli.no_pcap
        } else {
            cfg.no_pcap.unwrap_or(cli.no_pcap)
        },
        tty: if from_cli("tty") {
            cli.tty
        } else {
            cfg.tty.unwrap_or(cli.tty)
        },
        show_virtual_net: if from_cli("show_virtual_net") {
            cli.show_virtual_net
        } else {
            cfg.show_virtual_net.unwrap_or(cli.show_virtual_net)
        },
        corners: if from_cli("corners") {
            cli.corners
        } else {
            cfg.corners.unwrap_or(cli.corners)
        },
    }
}

fn features_str() -> &'static str {
    // Compile-time enumeration of which optional features are baked in.
    match (cfg!(feature = "ebpf"), cfg!(feature = "pcap")) {
        (true, true) => "ebpf,pcap",
        (true, false) => "ebpf",
        (false, true) => "pcap",
        (false, false) => "none (build with --features ebpf,pcap for per-pid net)",
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // We're a TUI: write logs to stderr so they don't corrupt the alt screen.
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn spawn_tick_driver(tx: broadcast::Sender<()>, tick_ms: Arc<AtomicU64>) {
    tokio::spawn(async move {
        loop {
            let dur = Duration::from_millis(tick_ms.load(Ordering::Relaxed));
            tokio::time::sleep(dur).await;
            // No subscribers (e.g. all collectors panicked) — keep ticking
            // anyway so a new subscriber would still get fresh ticks.
            let _ = tx.send(());
        }
    });
}

fn spawn_collector<C>(
    collector: Arc<C>,
    box_kind: Box,
    bus: DataBus,
    mut tick_rx: broadcast::Receiver<()>,
    boxes: BoxesEnabled,
) where
    C: Collector + 'static,
    C::Sample: Into<MetricEvent>,
{
    let name = collector.name();
    tokio::spawn(async move {
        loop {
            match tick_rx.recv().await {
                Ok(()) => {}
                // Channel lag (a slow collector missed ticks): treat as a
                // single wake — we don't want to back-fill skipped samples.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
            // Skip collect entirely when the panel is hidden. The wake
            // still happens so we resume promptly when re-enabled.
            if !boxes.is_enabled(box_kind) {
                continue;
            }
            match collector.collect().await {
                Ok(s) => {
                    bus.publish(s);
                }
                Err(e) => {
                    tracing::warn!(collector = name, error = %e, "collect failed");
                }
            }
        }
    });
}

fn spawn_attributor_loop(
    attr: Arc<dyn NetworkAttributor>,
    app: Arc<Mutex<App>>,
    tick_ms: Arc<AtomicU64>,
    boxes: BoxesEnabled,
) {
    tokio::spawn(async move {
        let tier = attr.tier();
        loop {
            // Attributor sampling is heavier than /proc parsing, so floor it
            // at 250ms even when the global tick is faster.
            let dur =
                Duration::from_millis(tick_ms.load(Ordering::Relaxed).max(250));
            tokio::time::sleep(dur).await;
            // Per-pid net is consumed only by the process table — when PROC
            // is hidden we have nowhere to render it, so skip the sample.
            if !boxes.is_enabled(Box::Process) {
                continue;
            }
            match attr.sample().await {
                Ok(samples) => {
                    let mut g = app.lock().unwrap_or_else(|p| p.into_inner());
                    g.apply_net(samples, tier);
                }
                Err(e) => tracing::warn!(error = %e, "net attributor sample failed"),
            }
        }
    });
}
