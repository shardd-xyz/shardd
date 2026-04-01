mod api;
mod batch_writer;
mod error;
mod orphan_detector;
mod peer;
mod state;
mod sync;

#[cfg(test)]
mod test_overdraft;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tracing::{info, warn};

use shardd_broadcast::http::HttpBroadcaster;
use shardd_broadcast::Broadcaster;
use shardd_storage::postgres::PostgresStorage;
use shardd_storage::StorageBackend;
use shardd_types::{JoinResponse, NodeMeta};

use crate::peer::PeerSet;
use crate::state::SharedState;

#[derive(Parser)]
#[command(name = "shardd-node")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long)]
    port: u16,

    #[arg(long)]
    advertise_addr: Option<String>,

    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    #[arg(long)]
    bootstrap: Vec<String>,

    #[arg(long, default_value = "16")]
    max_peers: usize,

    /// Catch-up sync interval (ms). Safety net, not primary sync.
    #[arg(long, default_value = "30000")]
    catchup_interval_ms: u64,

    /// BatchWriter flush interval (ms).
    #[arg(long, default_value = "100")]
    batch_flush_interval_ms: u64,

    /// BatchWriter flush size threshold.
    #[arg(long, default_value = "1000")]
    batch_flush_size: usize,

    /// Materialized view refresh interval (ms).
    #[arg(long, default_value = "5000")]
    matview_refresh_ms: u64,

    /// OrphanDetector check interval (ms).
    #[arg(long, default_value = "500")]
    orphan_check_interval_ms: u64,

    /// OrphanDetector age threshold (ms).
    #[arg(long, default_value = "500")]
    orphan_age_ms: u64,
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
    let advertise_addr = cli.advertise_addr.clone().unwrap_or_else(|| listen_addr.clone());

    // Connect to Postgres and run migrations
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cli.database_url)
        .await?;
    let storage = Arc::new(PostgresStorage::new(pool));
    storage.run_migrations().await?;
    info!("database connected, migrations applied");

    // Load or create node identity
    let (node_id, next_seq) = {
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
                let meta = NodeMeta { node_id: id.clone(), host: cli.host.clone(), port: cli.port, next_seq: 1 };
                storage.save_node_meta(&meta).await?;
                info!(node_id = %id, "created new node");
                (id, 1)
            }
        }
    };

    // Load persisted peers
    let persisted_peers = storage.load_peers().await?;
    let mut peers = PeerSet::new(cli.max_peers, advertise_addr.clone());
    peers.merge(&persisted_peers);
    for b in &cli.bootstrap {
        peers.add(b);
    }

    // Create broadcaster (HTTP-based, pushes to known peers)
    let broadcaster: Arc<dyn Broadcaster> = Arc::new(HttpBroadcaster::new(
        peers.to_vec(),
    ));

    // Create BatchWriter channel
    let (batch_tx, batch_rx) = tokio::sync::mpsc::unbounded_channel();

    // Build shared state — rebuilds caches from Postgres
    let shared = SharedState::new(
        node_id.clone(),
        advertise_addr.clone(),
        next_seq,
        peers,
        (*storage).clone(),
        batch_tx,
        broadcaster.clone(),
    )
    .await;

    info!(events = shared.event_count(), "state rebuilt from database");

    // Bootstrap from peers (trustless: pulls ALL events)
    if !cli.bootstrap.is_empty() {
        // First do HTTP join for peer discovery
        let client = reqwest::Client::new();
        for bootstrap_addr in &cli.bootstrap {
            info!(bootstrap = %bootstrap_addr, "joining bootstrap peer");
            match client
                .post(format!("http://{bootstrap_addr}/join"))
                .json(&serde_json::json!({ "node_id": node_id, "addr": advertise_addr }))
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) => {
                    if let Ok(join_resp) = resp.json::<JoinResponse>().await {
                        let mut p = shared.peers.lock().await;
                        p.add(bootstrap_addr);
                        p.merge(&join_resp.peers);
                        drop(p);
                        shared.persist_peers().await;
                        info!(bootstrap_node = %join_resp.node_id, "bootstrap join successful");
                    }
                }
                Err(e) => warn!(bootstrap = %bootstrap_addr, error = %e, "bootstrap join failed"),
            }
        }

        // Then do trustless catch-up
        sync::bootstrap_from_peers(&shared).await;
    }

    // Spawn BatchWriter
    let batch_writer = batch_writer::BatchWriter::new(
        batch_rx,
        storage.clone(),
        broadcaster.clone(),
        cli.batch_flush_interval_ms,
        cli.batch_flush_size,
        cli.matview_refresh_ms,
    );
    tokio::spawn(batch_writer.run());

    // Spawn OrphanDetector
    let orphan_state: Arc<dyn state::SharedStateAny> = Arc::new(shared.clone());
    let orphan_detector = orphan_detector::OrphanDetector::new(
        orphan_state,
        storage.clone(),
        broadcaster.clone(),
        cli.orphan_check_interval_ms,
        cli.orphan_age_ms,
    );
    tokio::spawn(orphan_detector.run());

    // Spawn catch-up sync loop (slow safety net)
    let sync_state = shared.clone();
    let catchup_ms = cli.catchup_interval_ms;
    tokio::spawn(async move {
        sync::catchup_loop(sync_state, catchup_ms).await;
    });

    // Build router
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
        .route("/debug/origin/{origin_node_id}", get(api::debug_origin))
        .route("/collapsed", get(api::get_collapsed))
        .route("/collapsed/{bucket}/{account}", get(api::get_collapsed_account))
        .route("/persistence", get(api::get_persistence))
        .layer(cors)
        .with_state(shared);

    info!(listen = %listen_addr, advertise = %advertise_addr, "starting shardd-node");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
