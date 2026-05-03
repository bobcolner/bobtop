//! bobtop daemon — wires CLI parsing, capability detection, collector spawn,
//! TUI render loop, and signal handling.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use bobtop_collectors::{
    CpuCollector, DiskCollector, MemoryCollector, NetworkGlobalCollector, ProcessCollector,
};
use bobtop_core::{Collector, DataBus, MetricEvent};
use bobtop_net::{select as select_attributor, NetworkAttributor, SelectOptions};
use bobtop_tui::{builtin_names, load_theme, LayoutPreset};
use clap::Parser;

use bobtop_daemon::app::App;
use bobtop_daemon::cli::{Cli, LayoutChoice};
use bobtop_daemon::tui;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();

    if cli.list_themes {
        for name in builtin_names() {
            println!("{name}");
        }
        return Ok(());
    }

    let theme = load_theme(&cli.theme);
    tracing::info!(theme = %theme.name, "loaded theme");

    let layout_preset = match cli.layout {
        LayoutChoice::Full => LayoutPreset::Full,
        LayoutChoice::Minimal => LayoutPreset::Minimal,
    };

    // Capability detection — pick the highest-tier network attributor available.
    let attributor: Arc<dyn NetworkAttributor> = Arc::from(select_attributor(SelectOptions {
        allow_ebpf: !cli.no_ebpf,
        allow_pcap: !cli.no_pcap,
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
    let tick_ms = Arc::new(AtomicU64::new(cli.tick().as_millis() as u64));
    let mut app = App::new(
        theme,
        layout_preset,
        Arc::clone(&tick_ms),
        cli.tty,
        cli.show_virtual_net,
    );
    app.net_tier = net_tier;
    let app = Arc::new(Mutex::new(app));

    // Collectors share the global tick. They re-read it on each iteration of
    // the spawn_collector loop so live `+`/`-` adjustments take effect on the
    // next sample.
    let cpu = Arc::new(CpuCollector::new());
    let mem = Arc::new(MemoryCollector::new());
    let proc = Arc::new(ProcessCollector::new());
    let net = Arc::new(NetworkGlobalCollector::new());
    let disk = Arc::new(DiskCollector::new());
    spawn_collector(cpu, bus.clone(), Arc::clone(&tick_ms));
    spawn_collector(mem, bus.clone(), Arc::clone(&tick_ms));
    spawn_collector(proc, bus.clone(), Arc::clone(&tick_ms));
    spawn_collector(net, bus.clone(), Arc::clone(&tick_ms));
    spawn_collector(disk, bus.clone(), Arc::clone(&tick_ms));

    // Network attribution sampler — separate channel because ProcessNetSample
    // doesn't fit into MetricEvent (cross-crate dep direction).
    spawn_attributor_loop(Arc::clone(&attributor), Arc::clone(&app), Arc::clone(&tick_ms));

    // Input thread (blocking crossterm reads).
    let input_rx = tui::spawn_input_thread();

    // Terminal lifecycle. Always restore on exit, even on error.
    let mut term = tui::init_terminal()?;
    let result = tui::run(&mut term, app, bus, input_rx).await;
    tui::restore_terminal(&mut term)?;

    result.map_err(Into::into)
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

fn spawn_collector<C>(collector: Arc<C>, bus: DataBus, tick_ms: Arc<AtomicU64>)
where
    C: Collector + 'static,
    C::Sample: Into<MetricEvent>,
{
    let name = collector.name();
    tokio::spawn(async move {
        loop {
            // Re-read the tick each iteration so `+` / `-` keys take effect
            // on the next sample without needing to restart the task.
            let dur = Duration::from_millis(tick_ms.load(Ordering::Relaxed));
            tokio::time::sleep(dur).await;
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
) {
    tokio::spawn(async move {
        let tier = attr.tier();
        loop {
            // Attributor sampling is heavier than /proc parsing, so floor it
            // at 250ms even when the global tick is faster.
            let dur =
                Duration::from_millis(tick_ms.load(Ordering::Relaxed).max(250));
            tokio::time::sleep(dur).await;
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
