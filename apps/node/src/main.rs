mod api;
mod error;
mod peer;
mod state;
mod sync;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tokio::sync::Mutex;
use tracing::{info, warn};

use shardd_storage::Storage;
use shardd_types::{JoinResponse, NodeMeta};

use crate::peer::PeerSet;
use crate::state::{NodeState, SharedState};

#[derive(Parser)]
#[command(name = "shardd-node", about = "Multi-writer replicated append-only event system")]
struct Cli {
    /// Listen host
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Listen port
    #[arg(long)]
    port: u16,

    /// Advertised address (host:port) for peer exchange. Defaults to host:port.
    #[arg(long)]
    advertise_addr: Option<String>,

    /// Config / data directory
    #[arg(long)]
    config_dir: PathBuf,

    /// Bootstrap peer addresses (host:port), may be repeated
    #[arg(long)]
    bootstrap: Vec<String>,

    /// Number of peers to contact per sync round
    #[arg(long, default_value = "3")]
    fanout: usize,

    /// Sync interval in milliseconds
    #[arg(long, default_value = "3000")]
    sync_interval_ms: u64,

    /// Maximum number of peers to track
    #[arg(long, default_value = "16")]
    max_peers: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "shardd=debug,tower_http=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let listen_addr = format!("{}:{}", cli.host, cli.port);
    let advertise_addr = cli
        .advertise_addr
        .clone()
        .unwrap_or_else(|| listen_addr.clone());

    // Initialize storage.
    let storage = Storage::new(&cli.config_dir);
    storage.init().await?;

    // Load or create node identity.
    let (node_id, next_seq) = match storage.load_node_meta().await? {
        Some(meta) => {
            info!(node_id = %meta.node_id, next_seq = meta.next_seq, "loaded existing node");
            (meta.node_id, meta.next_seq)
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            let meta = NodeMeta {
                node_id: id.clone(),
                host: cli.host.clone(),
                port: cli.port,
                next_seq: 1,
            };
            storage.save_node_meta(&meta).await?;
            info!(node_id = %id, "created new node");
            (id, 1)
        }
    };

    // Load peers.
    let peers_file = storage.load_peers().await?;
    let mut peers = PeerSet::new(cli.max_peers, advertise_addr.clone());
    peers.merge(&peers_file.peers);

    // Load events and compute heads.
    let events_by_origin = storage.load_all_events().await?;
    let mut contiguous_heads = BTreeMap::new();
    for (origin, seqs) in &events_by_origin {
        let mut head = 0u64;
        while seqs.contains_key(&(head + 1)) {
            head += 1;
        }
        contiguous_heads.insert(origin.clone(), head);
    }

    let loaded_events: usize = events_by_origin.values().map(|m| m.len()).sum();
    info!(events = loaded_events, heads = ?contiguous_heads, "loaded events from disk");

    let node_state = NodeState {
        node_id: node_id.clone(),
        addr: advertise_addr.clone(),
        next_seq,
        peers,
        events_by_origin,
        contiguous_heads,
        storage,
    };
    let shared: SharedState = Arc::new(Mutex::new(node_state));

    // Bootstrap from all provided peers.
    if !cli.bootstrap.is_empty() {
        let client = reqwest::Client::new();
        for bootstrap_addr in &cli.bootstrap {
            info!(bootstrap = %bootstrap_addr, "joining bootstrap peer");
            match client
                .post(format!("http://{bootstrap_addr}/join"))
                .json(&serde_json::json!({
                    "node_id": node_id,
                    "addr": advertise_addr,
                }))
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) => {
                    if let Ok(join_resp) = resp.json::<JoinResponse>().await {
                        let mut st = shared.lock().await;
                        st.peers.add(bootstrap_addr);
                        st.peers.merge(&join_resp.peers);
                        let _ = st.persist_peers().await;
                        info!(
                            bootstrap_node = %join_resp.node_id,
                            peers_received = join_resp.peers.len(),
                            "bootstrap join successful"
                        );
                    }
                }
                Err(e) => warn!(bootstrap = %bootstrap_addr, error = %e, "bootstrap join failed"),
            }
        }
    }

    // Spawn sync loop.
    let sync_state = shared.clone();
    let fanout = cli.fanout;
    let interval_ms = cli.sync_interval_ms;
    tokio::spawn(async move {
        sync::sync_loop(sync_state, interval_ms, fanout).await;
    });

    // Build router.
    let app = Router::new()
        .route("/health", get(api::health))
        .route("/state", get(api::get_state))
        .route("/peers", get(api::get_peers))
        .route("/peers/add", post(api::add_peer))
        .route("/join", post(api::join))
        .route("/events", get(api::list_events).post(api::create_event))
        .route("/events/replicate", post(api::replicate_event))
        .route("/events/range", post(api::events_range))
        .route("/heads", get(api::get_heads))
        .route("/sync", post(api::trigger_sync))
        .route("/debug/origin/{origin_node_id}", get(api::debug_origin))
        .with_state(shared);

    info!(listen = %listen_addr, advertise = %advertise_addr, config_dir = %cli.config_dir.display(), "starting shardd-node");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
