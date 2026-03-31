mod api;
mod error;
mod peer;
mod state;
mod sync;

#[cfg(test)]
mod test_overdraft;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tracing::{info, warn};

use shardd_storage::postgres::PostgresStorage;
use shardd_storage::StorageBackend;
use shardd_types::{JoinResponse, NodeMeta};

use crate::peer::PeerSet;
use crate::state::SharedState;

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

    /// PostgreSQL connection URL
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

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

    // Connect to PostgreSQL and run migrations.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cli.database_url)
        .await?;
    let storage = PostgresStorage::new(pool);
    storage.run_migrations().await?;
    info!("database connected, migrations applied");

    // Load or create node identity.
    let (node_id, next_seq) = match storage.load_node_meta_by_id("").await? {
        // Try to find any existing node_meta row for this node.
        // If none, create a new one.
        _ => {
            // Check if we have a persisted identity
            let rows = sqlx::query_as::<_, (String, i64)>(
                "SELECT node_id, next_seq FROM node_meta LIMIT 1",
            )
            .fetch_optional(storage.pool())
            .await?;

            match rows {
                Some((id, seq)) => {
                    info!(node_id = %id, next_seq = seq, "loaded existing node");
                    (id, seq as u64)
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
            }
        }
    };

    // Load persisted peers.
    let persisted_peers = storage.load_peers().await?;
    let mut peers = PeerSet::new(cli.max_peers, advertise_addr.clone());
    peers.merge(&persisted_peers);

    // Build shared state — rebuilds balance/head caches from Postgres.
    let shared = SharedState::new(
        node_id.clone(),
        advertise_addr.clone(),
        next_seq,
        peers,
        storage,
    )
    .await;

    info!(
        events = shared.event_count(),
        "state rebuilt from database"
    );

    // Bootstrap from provided peers.
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
                        {
                            let mut p = shared.peers.lock().await;
                            p.add(bootstrap_addr);
                            p.merge(&join_resp.peers);
                        }
                        shared.persist_peers().await;
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
    let cors = tower_http::cors::CorsLayer::permissive();
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
        .route("/balances", get(api::get_balances))
        .route("/sync", post(api::trigger_sync))
        .route("/debug/origin/{origin_node_id}", get(api::debug_origin))
        .route("/collapsed", get(api::get_collapsed))
        .route("/collapsed/{bucket}/{account}", get(api::get_collapsed_account))
        .layer(cors)
        .with_state(shared);

    info!(listen = %listen_addr, advertise = %advertise_addr, "starting shardd-node");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
