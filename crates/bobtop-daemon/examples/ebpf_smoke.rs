//! Smoke test the live eBPF attributor: load it, sleep, sample, print
//! per-pid byte rates. Generates its own traffic against 127.0.0.1 so the
//! test is self-contained.
//!
//!   sudo ./target/release/examples/ebpf_smoke
//! or with caps:
//!   sudo setcap 'cap_bpf,cap_perfmon,cap_net_admin=ep' ./target/release/examples/ebpf_smoke

use std::time::Duration;

use bobtop_net::{select, SelectOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    let attr = select(SelectOptions::default());
    println!("selected tier: {:?}", attr.tier());

    // Generate some local traffic so the kprobes have something to count.
    tokio::spawn(async {
        loop {
            // Open and close a few TCP connections to localhost.
            for _ in 0..5 {
                let _ = tokio::net::TcpStream::connect("127.0.0.1:22").await;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    // Prime the baseline.
    let _ = attr.sample().await?;

    for tick in 0..3 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let samples = attr.sample().await?;
        let mut nonzero: Vec<_> = samples
            .into_iter()
            .filter(|s| {
                s.rx_bytes_per_sec.unwrap_or(0.0) > 0.0
                    || s.tx_bytes_per_sec.unwrap_or(0.0) > 0.0
            })
            .collect();
        nonzero.sort_by(|a, b| {
            let aa = a.rx_bytes_per_sec.unwrap_or(0.0) + a.tx_bytes_per_sec.unwrap_or(0.0);
            let bb = b.rx_bytes_per_sec.unwrap_or(0.0) + b.tx_bytes_per_sec.unwrap_or(0.0);
            bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
        });
        println!("\n=== tick {} — {} pids with traffic ===", tick, nonzero.len());
        for p in nonzero.iter().take(10) {
            println!(
                "  pid={:>7} {:<24} rx={:>10.1} B/s  tx={:>10.1} B/s",
                p.pid,
                p.name,
                p.rx_bytes_per_sec.unwrap_or(0.0),
                p.tx_bytes_per_sec.unwrap_or(0.0),
            );
        }
    }

    Ok(())
}
