//! Latest-value aggregator over the [`crate::core::DataBus`].
//!
//! [`SampleStore`] subscribes to the bus once and folds each [`MetricEvent`]
//! into a [`HostSample`] — the unified snapshot of "what's the host doing
//! right now" across every subsystem. Read-only consumers (the agent socket
//! server, exporters, future probes) get a single point of access without
//! re-implementing the per-subsystem fold or contending on the TUI's `App`.
//!
//! The store is additive: existing consumers (TUI) keep subscribing to the
//! bus directly. The store is just a parallel subscriber that maintains its
//! own cached `HostSample` for ad-hoc queries.

use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::watch;

use crate::core::sample::{
    CpuSample, DiskSample, GpuSample, MemorySample, NetworkSample, ProcessSample,
};
use crate::core::{DataBus, MetricEvent};

/// Latest snapshot of every subsystem the bus carries.
///
/// Fields are `Option` because samples arrive asynchronously and the store
/// is observable before every collector has produced its first sample. A
/// `None` simply means "no data yet" — agents should treat that as a
/// not-yet-available signal rather than zero.
#[derive(Debug, Clone, Default)]
pub struct HostSample {
    /// Wall-clock time of the most recent update to *any* field. Useful for
    /// agents to detect a stalled daemon (no updates in N seconds).
    pub captured_at: Option<SystemTime>,
    pub cpu: Option<CpuSample>,
    pub memory: Option<MemorySample>,
    pub network: Option<NetworkSample>,
    pub disk: Option<DiskSample>,
    pub processes: Option<ProcessSample>,
    pub gpu: Option<GpuSample>,
}

/// Latest-value aggregator. Cheap to clone (holds an `Arc`).
///
/// Spawn one alongside the `DataBus` at startup and call [`SampleStore::latest`]
/// for ad-hoc reads or [`SampleStore::subscribe`] for change notifications.
#[derive(Debug, Clone)]
pub struct SampleStore {
    rx: watch::Receiver<Arc<HostSample>>,
}

impl SampleStore {
    /// Spawn a background task that subscribes to `bus` and updates the
    /// store on each event. The returned handle stays alive as long as the
    /// caller holds it; dropping all clones lets the updater task exit on
    /// the next bus message (or stay parked if the bus is also dropped).
    pub fn spawn(bus: &DataBus) -> Self {
        let (tx, rx) = watch::channel(Arc::new(HostSample::default()));
        let mut bus_rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                match bus_rx.recv().await {
                    Ok(ev) => {
                        let mut next = (**tx.borrow()).clone();
                        apply(&mut next, ev);
                        next.captured_at = Some(SystemTime::now());
                        // `send` only fails if every receiver is dropped — at
                        // which point the store is unobservable and we exit.
                        if tx.send(Arc::new(next)).is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Self { rx }
    }

    /// Latest aggregated snapshot. Cheap (clones an `Arc`).
    pub fn latest(&self) -> Arc<HostSample> {
        self.rx.borrow().clone()
    }

    /// Watch receiver for consumers that want to react to updates. The
    /// initial value is observable immediately; subsequent updates fire on
    /// every bus event.
    pub fn subscribe(&self) -> watch::Receiver<Arc<HostSample>> {
        self.rx.clone()
    }
}

fn apply(snap: &mut HostSample, ev: MetricEvent) {
    match ev {
        MetricEvent::Cpu(s) => snap.cpu = Some(s),
        MetricEvent::Memory(s) => snap.memory = Some(s),
        MetricEvent::Network(s) => snap.network = Some(s),
        MetricEvent::Disk(s) => snap.disk = Some(s),
        MetricEvent::Process(s) => snap.processes = Some(s),
        MetricEvent::Gpu(s) => snap.gpu = Some(s),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::core::sample::MemorySample;

    fn dummy_mem(used: u64) -> MemorySample {
        MemorySample {
            timestamp: Instant::now(),
            total_bytes: 1024,
            used_bytes: used,
            available_bytes: 1024 - used,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            huge_pages: None,
            cached_bytes: 0,
            buffers_bytes: 0,
            free_bytes: 1024 - used,
            pressure: None,
            cpu_pressure: None,
            io_pressure: None,
        }
    }

    #[tokio::test]
    async fn store_starts_empty() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let snap = store.latest();
        assert!(snap.cpu.is_none());
        assert!(snap.memory.is_none());
        assert!(snap.captured_at.is_none());
    }

    #[tokio::test]
    async fn store_folds_published_events() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        // Subscribe before publishing so the updater task has a chance to
        // attach; otherwise the broadcast send may race the subscribe.
        let mut rx = store.subscribe();
        bus.publish(dummy_mem(123));
        // Wait for the store to process the event.
        rx.changed().await.expect("watch update");
        let snap = store.latest();
        assert_eq!(snap.memory.as_ref().unwrap().used_bytes, 123);
        assert!(snap.captured_at.is_some());
        assert!(snap.cpu.is_none());
    }

    #[tokio::test]
    async fn later_events_overwrite_earlier() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let mut rx = store.subscribe();
        bus.publish(dummy_mem(100));
        rx.changed().await.unwrap();
        bus.publish(dummy_mem(200));
        rx.changed().await.unwrap();
        let snap = store.latest();
        assert_eq!(snap.memory.as_ref().unwrap().used_bytes, 200);
    }
}
