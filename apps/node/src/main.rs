use clap::Parser;
use shardd_node::{NodeConfig, server};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "shardd-node",
    about = "Distributed append-only ledger node (libp2p)"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Multiaddrs this node advertises to peers. Repeatable; each address becomes
    /// a dial candidate. Typical set: public IP/DNS, AWS private IP (same-region
    /// peers prefer this via libp2p's happy-eyeballs dial race), Tailscale IP.
    #[arg(long, action = clap::ArgAction::Append)]
    advertise_addr: Vec<String>,
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    /// libp2p bootstrap peer multiaddrs (e.g., /ip4/1.2.3.4/tcp/9000).
    #[arg(long)]
    bootstrap: Vec<String>,
    #[arg(long, default_value = "100")]
    batch_flush_interval_ms: u64,
    #[arg(long, default_value = "1000")]
    batch_flush_size: usize,
    #[arg(long, default_value = "5000")]
    matview_refresh_ms: u64,
    #[arg(long, default_value = "500")]
    orphan_check_interval_ms: u64,
    #[arg(long, default_value = "500")]
    orphan_age_ms: u64,
    #[arg(long, default_value = "5")]
    hold_multiplier: u64,
    #[arg(long, default_value = "600000")]
    hold_duration_ms: u64,
    /// libp2p TCP port for the node's mesh listener.
    #[arg(long, default_value_t = 9000)]
    libp2p_port: u16,
    /// Path to 32-byte PSK file for libp2p private mesh encryption.
    #[arg(long)]
    psk_file: Option<String>,
    /// Arbitrary shared cluster key. Derived into the mesh PSK.
    #[arg(long, env = "SHARDD_CLUSTER_KEY")]
    cluster_key: Option<String>,
    /// Parallel workers for gossipsub event ingestion (JSON decode + state insert).
    #[arg(long, default_value = "4")]
    event_worker_count: usize,
}

impl From<Cli> for NodeConfig {
    fn from(cli: Cli) -> Self {
        NodeConfig {
            host: cli.host,
            advertise_addrs: cli.advertise_addr,
            database_url: cli.database_url,
            bootstrap: cli.bootstrap,
            batch_flush_interval_ms: cli.batch_flush_interval_ms,
            batch_flush_size: cli.batch_flush_size,
            matview_refresh_ms: cli.matview_refresh_ms,
            orphan_check_interval_ms: cli.orphan_check_interval_ms,
            orphan_age_ms: cli.orphan_age_ms,
            hold_multiplier: cli.hold_multiplier,
            hold_duration_ms: cli.hold_duration_ms,
            libp2p_port: cli.libp2p_port,
            psk_file: cli.psk_file,
            cluster_key: cli.cluster_key,
            event_worker_count: cli.event_worker_count,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "shardd=debug".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let config: NodeConfig = cli.into();
    let handle = server::start_node(config).await?;

    tokio::signal::ctrl_c().await.ok();
    let join = handle.shutdown();
    join.await??;

    Ok(())
}
