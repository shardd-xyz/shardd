mod api;
mod batch_writer;
mod orphan_detector;
mod state;
mod sync;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use shardd_storage::postgres::PostgresStorage;
use shardd_storage::StorageBackend;
use shardd_types::NodeMeta;

// ── Node phase (§13.2) ──────────────────────────────────────────────

/// Node lifecycle phase for readiness gate and graceful shutdown.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodePhase {
    Warming = 0,
    Ready = 1,
    ShuttingDown = 2,
}

impl NodePhase {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Ready,
            2 => Self::ShuttingDown,
            _ => Self::Warming,
        }
    }
}

/// Shared phase state accessible from API handlers.
pub type PhaseRef = Arc<AtomicU8>;

pub fn get_phase(phase: &PhaseRef) -> NodePhase {
    NodePhase::from_u8(phase.load(Ordering::Relaxed))
}

pub fn set_phase(phase: &PhaseRef, p: NodePhase) {
    phase.store(p as u8, Ordering::Relaxed);
}

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "shardd-node", about = "Distributed append-only ledger node (v2)")]
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
    #[arg(long, default_value = "30000")]
    catchup_interval_ms: u64,
}

// ── Main ─────────────────────────────────────────────────────────────

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

    // §13.1 step 1: Database
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cli.database_url)
        .await?;
    let storage = Arc::new(PostgresStorage::new(pool));
    storage.run_migrations().await?;
    info!("database connected, migrations applied");

    // §13.1 step 2: Node identity
    let node_id = {
        let rows = sqlx::query_as::<_, (String,)>("SELECT node_id FROM node_meta LIMIT 1")
            .fetch_optional(storage.pool()).await?;
        match rows {
            Some((id,)) => { info!(node_id = %id, "loaded existing node"); id }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                storage.save_node_meta(&NodeMeta {
                    node_id: id.clone(), host: cli.host.clone(), port: cli.port,
                    current_epoch: 0, next_seq: 1,
                }).await?;
                info!(node_id = %id, "created new node"); id
            }
        }
    };

    // §13.1 step 3: Increment epoch
    let current_epoch = storage.increment_epoch(&node_id).await?;
    info!(epoch = current_epoch, "epoch incremented");

    // Node phase: start in Warming
    let phase: PhaseRef = Arc::new(AtomicU8::new(NodePhase::Warming as u8));

    // §13.1 steps 4-5: Build state
    let (batch_tx, batch_rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = state::SharedState::new(
        node_id.clone(), advertise_addr.clone(), current_epoch,
        (*storage).clone(), batch_tx,
    ).await;
    info!(events = shared.event_count(), "state rebuilt from database");

    // §13.1 step 8: Background tasks with JoinSet supervision
    let mut tasks = JoinSet::new();

    // BatchWriter
    let bw = batch_writer::BatchWriter::new(
        batch_rx, storage.clone(),
        cli.batch_flush_interval_ms, cli.batch_flush_size, cli.matview_refresh_ms,
    );
    tasks.spawn(bw.run());

    // OrphanDetector
    let shared_for_orphan: Arc<dyn orphan_detector::UnpersistedSource> = Arc::new(shared.clone());
    let od = orphan_detector::OrphanDetector::new(
        shared_for_orphan, storage.clone(),
        cli.orphan_check_interval_ms, cli.orphan_age_ms,
    );
    tasks.spawn(od.run());

    // Catch-up sync
    let sync_state = shared.clone();
    let catchup_ms = cli.catchup_interval_ms;
    tasks.spawn(async move {
        sync::catchup_loop(sync_state, catchup_ms).await;
    });

    // §13.2: Mark ready (in production, check head lag first)
    set_phase(&phase, NodePhase::Ready);
    info!("node ready");

    // Build router
    let cors = tower_http::cors::CorsLayer::permissive();
    let app = Router::new()
        .route("/health", get(api::health::<PostgresStorage>))
        .route("/state", get(api::get_state::<PostgresStorage>))
        .route("/events", get(api::list_events::<PostgresStorage>).post(api::create_event::<PostgresStorage>))
        .route("/events/replicate", post(api::replicate_event::<PostgresStorage>))
        .route("/events/range", post(api::events_range::<PostgresStorage>))
        .route("/heads", get(api::get_heads::<PostgresStorage>))
        .route("/balances", get(api::get_balances::<PostgresStorage>))
        .route("/collapsed", get(api::get_collapsed::<PostgresStorage>))
        .route("/collapsed/{bucket}/{account}", get(api::get_collapsed_account::<PostgresStorage>))
        .route("/persistence", get(api::get_persistence::<PostgresStorage>))
        .route("/join", post(api::join::<PostgresStorage>))
        .route("/registry", get(api::get_registry::<PostgresStorage>))
        .layer(cors)
        .with_state(shared);

    info!(listen = %listen_addr, advertise = %advertise_addr, epoch = current_epoch, "starting shardd-node v2");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;

    // Graceful shutdown: Ctrl+C → drain → flush → exit
    let shutdown_phase = phase.clone();
    let shutdown_signal = async move {
        tokio::signal::ctrl_c().await.ok();
        info!("shutdown signal received");
        set_phase(&shutdown_phase, NodePhase::ShuttingDown);
    };

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    // After server stops: wait for background tasks to finish
    info!("server stopped, draining background tasks...");
    tasks.shutdown().await;
    info!("all tasks drained, goodbye");

    Ok(())
}
