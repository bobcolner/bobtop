//! Live smoke test for the CPU and memory collectors.
//!
//! Spins up both, publishes their samples onto a `DataBus`, and prints what a
//! subscriber sees over a couple of ticks. Useful for eyeballing values
//! without the full TUI.

use std::time::Duration;

use bobtop_collectors::{CpuCollector, MemoryCollector};
use bobtop_core::{Collector, DataBus, MetricEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let bus = DataBus::default();
    let mut rx = bus.subscribe();

    let cpu = CpuCollector::new();
    let mem = MemoryCollector::new();

    // Prime CPU baseline (first sample is always 0%).
    let _ = cpu.collect().await?;

    for tick in 0..3u32 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        bus.publish(cpu.collect().await?);
        bus.publish(mem.collect().await?);

        for _ in 0..2 {
            match rx.recv().await? {
                MetricEvent::Cpu(s) => println!(
                    "tick {tick}  cpu agg={:>5.1}%  cores={}  load1={:?}",
                    s.aggregate_utilization * 100.0,
                    s.cores.len(),
                    s.load_average.map(|l| l.one),
                ),
                MetricEvent::Memory(m) => println!(
                    "tick {tick}  mem used={:>6.2} GiB / {:>6.2} GiB  swap_used={:>5.2} GiB",
                    bytes_to_gib(m.used_bytes),
                    bytes_to_gib(m.total_bytes),
                    bytes_to_gib(m.swap_used_bytes),
                ),
                other => println!("tick {tick}  unexpected event kind: {}", other.kind()),
            }
        }
    }

    Ok(())
}

fn bytes_to_gib(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0 * 1024.0)
}
