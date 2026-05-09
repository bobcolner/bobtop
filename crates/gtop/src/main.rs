//! gtop daemon — wires CLI parsing, capability detection, collector spawn,
//! TUI render loop, and signal handling.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::{parser::ValueSource, CommandFactory, FromArgMatches};

use gtop::cli::CornerChoice;
use gtop::config::Config;
use gtop::core::BoxesEnabled;
use gtop::engine::{Engine, EngineConfig};
use gtui::{builtin_names, LayoutPreset};

use gtop::app::App;
use gtop::cli::{Cli, LayoutChoice};
use gtop::monitor_theme;
use gtop::tui;

// Synchronous entry point: handles the `agent` subcommand without
// entering a tokio runtime. The agent client constructs its own
// single-thread runtime for the socket round-trip; wrapping it in an
// outer `#[tokio::main]` runtime would mean dropping the inner
// runtime inside the outer's async context, which tokio panics on:
//
//     "Cannot drop a runtime in a context where blocking is not allowed."
//
// Symptom (historical): the `gtop agent` subcommand built its own
// inner runtime and dropped it inside an outer `#[tokio::main]` async
// context, which tokio panics on. Splitting the entry point keeps
// the daemon path wrapped (it really is async) while letting the
// dispatched agent client run in plain sync context.
fn main() -> Result<()> {
    // Short-circuit: `gtop agent <subcommand>` is a thin Unix-socket
    // client that never starts collectors or the TUI. Detect it before
    // clap parses so we don't have to graft the agent surface into the
    // TUI's argument schema.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.first().map(|s| s.as_str()) == Some("agent") {
        let code = gtop::agent::client::run(&raw_args[1..]);
        std::process::exit(code);
    }

    // Daemon / TUI path is genuinely async — build the runtime here and
    // run the rest of main inside it.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(daemon_main())
}

async fn daemon_main() -> Result<()> {
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

    if cli.help_keys {
        print_help_keys();
        return Ok(());
    }

    let cfg = Config::load_or_default();
    let eff = resolve(&cli, &matches, &cfg);

    let theme = monitor_theme::load(&eff.theme);
    tracing::info!(theme = %theme.name, "loaded theme");

    let layout_preset = match eff.layout {
        LayoutChoice::Full => LayoutPreset::Full,
        LayoutChoice::Minimal => LayoutPreset::Minimal,
    };

    let tick_ms = Arc::new(AtomicU64::new(eff.tick_ms_clamped()));

    if cli.daemon {
        // Headless mode: skip App construction entirely (it's a TUI
        // presentation cache with ~150 KB of history queues + theme
        // data; useless when nothing is rendering). All collectors are
        // box-enabled so the engine samples the full host.
        let boxes = BoxesEnabled::all();
        let (engine, meta) = Engine::start(EngineConfig {
            tick_ms: Arc::clone(&tick_ms),
            boxes,
            allow_ebpf: !eff.no_ebpf,
            allow_pcap: !eff.no_pcap,
        });
        tracing::warn!(
            compiled_features = features_str(),
            selected_tier = meta.net_tier.name(),
            per_pid_bandwidth = meta.net_tier.has_bandwidth(),
            disk_tier = meta.disk_tier.name(),
            "gtop --daemon start"
        );
        let agent_handle = gtop::agent::spawn(
            engine.store.clone(),
            engine.history.clone(),
        );
        tracing::warn!(
            idle_exit_secs = DAEMON_IDLE_EXIT_SECS,
            "running in --daemon mode; press Ctrl-C to exit"
        );
        let activity_for_watch = agent_handle.as_ref().map(|h| h.last_activity.clone());
        tokio::select! {
            _ = wait_for_shutdown() => {
                tracing::info!("daemon: shutdown signal received");
            }
            _ = idle_watchdog(activity_for_watch, DAEMON_IDLE_EXIT_SECS) => {
                tracing::warn!(
                    idle_exit_secs = DAEMON_IDLE_EXIT_SECS,
                    "daemon: idle exit — no agent activity for the configured window"
                );
            }
        }
        if let Some(h) = agent_handle.as_ref() {
            let _ = std::fs::remove_file(&h.socket_path);
        }
        return Ok(());
    }

    // TUI mode: build App first so we can take its `boxes` handle (the
    // TUI mutates panel visibility there; the engine reads it every tick).
    let mut app = App::new(
        theme,
        layout_preset,
        Arc::clone(&tick_ms),
        eff.tty,
        eff.show_virtual_net,
    );
    app.corner_style = eff.corners.into();
    app.theme_background = eff.theme_background;
    app.truecolor = eff.truecolor;
    app.apply_color_options();
    let boxes = app.boxes.clone();

    let (engine, meta) = Engine::start(EngineConfig {
        tick_ms: Arc::clone(&tick_ms),
        boxes: boxes.clone(),
        allow_ebpf: !eff.no_ebpf,
        allow_pcap: !eff.no_pcap,
    });

    app.net_tier = meta.net_tier;
    app.disk_tier = meta.disk_tier;
    app.attribution = Some(engine.attribution.clone());
    let app = Arc::new(Mutex::new(app));

    tracing::warn!(
        compiled_features = features_str(),
        selected_tier = meta.net_tier.name(),
        per_pid_bandwidth = meta.net_tier.has_bandwidth(),
        disk_tier = meta.disk_tier.name(),
        "gtop start"
    );

    let _agent_handle = gtop::agent::spawn(
        engine.store.clone(),
        engine.history.clone(),
    );

    // Input thread (blocking crossterm reads).
    let input_rx = tui::spawn_input_thread();

    // Terminal lifecycle. Always restore on exit, even on error.
    let mut term = tui::init_terminal()?;
    let result = tui::run(&mut term, app, engine.bus.clone(), input_rx).await;
    tui::restore_terminal(&mut term)?;

    result.map_err(Into::into)
}

/// Daemon idle-exit threshold. When `--daemon` mode runs for this many
/// seconds with no agent socket activity, the daemon shuts down on its
/// own. Keeps forgotten background daemons from living forever.
const DAEMON_IDLE_EXIT_SECS: u64 = 30 * 60;

/// Watchdog: completes when the agent socket has been idle for
/// `idle_secs` continuously. If `last_activity` is `None` (the socket
/// failed to bind), this future never resolves — the daemon stays
/// alive until SIGTERM, which is the safe default.
async fn idle_watchdog(last_activity: Option<Arc<AtomicU64>>, idle_secs: u64) {
    let activity = match last_activity {
        Some(a) => a,
        None => {
            std::future::pending::<()>().await;
            return;
        }
    };
    // Coarse polling — the deadline is 30 min, so 60s resolution is plenty.
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let last = activity.load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(last) >= idle_secs {
            return;
        }
    }
}

/// Block on Ctrl-C or SIGTERM, whichever arrives first. Used by `--daemon`
/// mode where there's no TUI render loop to provide a natural exit point.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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
    theme_background: bool,
    truecolor: bool,
}

impl EffectiveConfig {
    fn tick_ms_clamped(&self) -> u64 {
        self.tick_ms
            .clamp(gtop::cli::MIN_TICK_MS, gtop::cli::MAX_TICK_MS)
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
        // Theme background + truecolor toggles default to true (btop's
        // defaults). No CLI flag yet — they're config + Options-overlay only.
        theme_background: cfg.theme_background.unwrap_or(true),
        truecolor: cfg.truecolor.unwrap_or(true),
    }
}

fn print_help_keys() {
    // Right-align keys to the longest, separated from descriptions by 2
    // spaces. Same data the `?` overlay reads.
    let key_w = gtop::ui::HELP_LINES
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(8);
    println!("gtop keybinds:");
    for (k, d) in gtop::ui::HELP_LINES {
        println!("  {:>width$}  {}", k, d, width = key_w);
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

