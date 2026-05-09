//! The gtop sampling engine — bus + collectors + per-pid attribution +
//! latest-value store + retrospective ring buffer, packaged behind one
//! handle so callers don't have to spawn each task individually.
//!
//! Two consumers today: the TUI (subscribes to the bus, renders frames)
//! and the agent socket server (queries `store` / `history`). A third
//! could plug in trivially — that's the architectural property the engine
//! exists to provide.
//!
//! `Engine::start` is fire-and-forget: it spawns every required task and
//! returns a cheap-to-clone handle whose drops don't tear anything down.
//! The tasks live until the tokio runtime stops, which is the expected
//! lifecycle for both TUI and `--daemon` modes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::collectors::{
    CpuCollector, DiskCollector, MemoryCollector, NetworkGlobalCollector, ProcessCollector,
};
use crate::core::{BoxesEnabled, Collector, DataBus, History, MetricEvent, SampleStore};
use crate::pid_attr::{
    select as select_attributor, select_disk, AttributionStore, AttributorTier,
    DiskAttributor, DiskAttributorTier, NetworkAttributor, SelectOptions,
};
use tokio::sync::broadcast;

/// Inputs needed to start the engine. Box visibility is shared with the
/// TUI (the TUI mutates these to gate per-tick collection) — the engine
/// reads them every tick to decide whether to skip a `collect()` call.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub tick_ms: Arc<AtomicU64>,
    pub boxes: BoxesEnabled,
    pub allow_ebpf: bool,
    pub allow_pcap: bool,
}

/// Capability metadata observed at engine startup. The TUI uses these to
/// label panels (e.g. "rx/tx: ebpf"); CLI logging echoes them too.
#[derive(Debug, Clone, Copy)]
pub struct EngineMeta {
    pub net_tier: AttributorTier,
    pub disk_tier: DiskAttributorTier,
}

/// Cheap-to-clone handle to the running engine.
#[derive(Debug, Clone)]
pub struct Engine {
    pub bus: DataBus,
    pub store: SampleStore,
    pub history: History,
    pub attribution: AttributionStore,
    pub tick_ms: Arc<AtomicU64>,
}

impl Engine {
    /// Spawn the full engine: tick driver, five collectors, both
    /// attributor loops, sample store, retrospective ring. Returns the
    /// engine handle plus startup metadata for logging / TUI labeling.
    pub fn start(cfg: EngineConfig) -> (Self, EngineMeta) {
        // Capability detection — attributor selection is synchronous and
        // self-logging.
        let attributor: Arc<dyn NetworkAttributor> = Arc::from(select_attributor(SelectOptions {
            allow_ebpf: cfg.allow_ebpf,
            allow_pcap: cfg.allow_pcap,
        }));
        let net_tier = attributor.tier();
        let disk_attributor: Option<Arc<dyn DiskAttributor>> = select_disk(SelectOptions {
            allow_ebpf: cfg.allow_ebpf,
            allow_pcap: cfg.allow_pcap,
        })
        .map(Arc::from);
        let disk_tier = disk_attributor
            .as_ref()
            .map(|a| a.tier())
            .unwrap_or(DiskAttributorTier::Unavailable);

        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let attribution = AttributionStore::new();

        // Single tick driver fans out to every collector via a broadcast
        // channel. One sleep instead of five — collectors all wake on
        // the same boundary, eliminating the alignment-window CPU spikes
        // that independent timers would produce. Live `+`/`-`
        // adjustments are picked up on the next iteration.
        let (tick_tx, _) = broadcast::channel::<()>(4);
        spawn_tick_driver(tick_tx.clone(), Arc::clone(&cfg.tick_ms));

        let cpu = Arc::new(CpuCollector::new());
        let mem = Arc::new(MemoryCollector::new());
        // Process collector joins per-pid net/disk attribution from
        // `AttributionStore` before publishing — the data is authoritative
        // on the bus by the time anyone subscribes.
        let proc = Arc::new(ProcessCollector::new().with_attribution(attribution.clone()));
        let net = Arc::new(NetworkGlobalCollector::new());
        let disk = Arc::new(DiskCollector::new());
        spawn_collector(
            cpu,
            bus.clone(),
            tick_tx.subscribe(),
        );
        spawn_collector(
            mem,
            bus.clone(),
            tick_tx.subscribe(),
        );
        spawn_collector(
            proc,
            bus.clone(),
            tick_tx.subscribe(),
        );
        spawn_collector(
            net,
            bus.clone(),
            tick_tx.subscribe(),
        );
        spawn_collector(
            disk,
            bus.clone(),
            tick_tx.subscribe(),
        );

        // Per-pid attribution samplers — separate cadence (≥250ms)
        // because each tier has different cost. Box-gated on PROC: when
        // the process panel is hidden the eBPF/pcap drain parks.
        spawn_attributor_loop(
            attributor,
            attribution.clone(),
            Arc::clone(&cfg.tick_ms),
        );
        if let Some(da) = disk_attributor {
            spawn_disk_attributor_loop(
                da,
                attribution.clone(),
                Arc::clone(&cfg.tick_ms),
            );
        }
        // Flow enumerator — always parses /proc/net/{tcp,tcp6} +
        // /proc/[pid]/fd to populate the connection list, independent
        // of which byte attributor is active. eBPF and pcap track
        // bytes per pid but don't enumerate connections, so without
        // this the flow panel would be empty whenever those tiers are
        // selected. Skipped when the active byte tier *is* proc_inode
        // (which already populates flows in the byte loop) and on
        // non-Linux hosts where proc_inode isn't available.
        if net_tier != AttributorTier::ProcInode {
            spawn_flow_enumerator_loop(
                attribution.clone(),
                Arc::clone(&cfg.tick_ms),
            );
        }

        (
            Self {
                bus,
                store,
                history,
                attribution,
                tick_ms: cfg.tick_ms,
            },
            EngineMeta {
                net_tier,
                disk_tier,
            },
        )
    }
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
    bus: DataBus,
    mut tick_rx: broadcast::Receiver<()>,
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
                // single wake — we don't back-fill skipped samples.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
            match collector.collect().await {
                Ok(s) => {
                    bus.publish(s);
                }
                Err(e) => {
                    tracing::warn!(collector = %name, error = %e, "collector tick failed");
                }
            }
        }
    });
}

fn spawn_attributor_loop(
    attr: Arc<dyn NetworkAttributor>,
    store: AttributionStore,
    tick_ms: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        let tier = attr.tier();
        loop {
            // Attributor sampling is heavier than /proc parsing, so floor
            // it at 250ms even when the global tick is faster.
            let dur =
                Duration::from_millis(tick_ms.load(Ordering::Relaxed).max(250));
            tokio::time::sleep(dur).await;
            match attr.sample().await {
                Ok(samples) => store.set_net(samples, tier),
                Err(e) => tracing::warn!(error = %e, "net attributor sample failed"),
            }
        }
    });
}

/// Independent loop that walks `/proc/net/{tcp,tcp6}` + `/proc/[pid]/fd`
/// to enumerate active connections, regardless of which byte attributor
/// is active. eBPF and pcap give us per-pid byte rates but no
/// connection list — this loop is what makes the flow panel work on
/// those tiers. Cheap (<1 ms per cycle on a typical host) and runs at
/// a slower cadence (floor 1 s) since flow churn is much lower than
/// byte-rate variation.
fn spawn_flow_enumerator_loop(store: AttributionStore, tick_ms: Arc<AtomicU64>) {
    #[cfg(target_os = "linux")]
    {
        use crate::pid_attr::proc_inode::ProcInodeAttributor;
        if !ProcInodeAttributor::available() {
            tracing::debug!("flow enumerator: /proc/net/tcp not present");
            return;
        }
        let attr: Arc<dyn NetworkAttributor> = Arc::new(ProcInodeAttributor::new());
        tokio::spawn(async move {
            loop {
                let dur = Duration::from_millis(
                    tick_ms.load(Ordering::Relaxed).max(1000),
                );
                tokio::time::sleep(dur).await;
                match attr.sample().await {
                    Ok(samples) => store.set_net_flows(samples),
                    Err(e) => tracing::debug!(error = %e, "flow enumerator sample failed"),
                }
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (store, tick_ms);
    }
}

fn spawn_disk_attributor_loop(
    attr: Arc<dyn DiskAttributor>,
    store: AttributionStore,
    tick_ms: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        let tier = attr.tier();
        loop {
            let dur =
                Duration::from_millis(tick_ms.load(Ordering::Relaxed).max(250));
            tokio::time::sleep(dur).await;
            match attr.sample().await {
                Ok(samples) => store.set_disk(samples, tier),
                Err(e) => tracing::warn!(error = %e, "disk attributor sample failed"),
            }
        }
    });
}
