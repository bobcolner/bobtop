use bobtop_net::{select, SelectOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();
    let attr = select(SelectOptions::default());
    println!("selected tier: {:?} (has_bandwidth={})", attr.tier(), attr.tier().has_bandwidth());
    let sample = attr.sample().await?;
    println!("processes with sockets: {}", sample.len());
    for p in sample.iter().take(5) {
        println!("  pid={} name={:?} conns={} rx={:?} tx={:?}",
            p.pid, p.name, p.connections.len(), p.rx_bytes_per_sec, p.tx_bytes_per_sec);
        for c in p.connections.iter().take(3) {
            println!("    {:?} {:?} -> {:?}", c.state, c.local, c.remote);
        }
    }
    Ok(())
}
