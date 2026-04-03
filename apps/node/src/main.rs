mod api;
mod batch_writer;
mod orphan_detector;
mod state;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tracing::{info, warn};

use shardd_storage::postgres::PostgresStorage;
use shardd_storage::StorageBackend;
use shardd_types::NodeMeta;

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

    // §13.1 step 1: Connect to database, run migrations
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&cli.database_url)
        .await?;
    let storage = Arc::new(PostgresStorage::new(pool));
    storage.run_migrations().await?;
    info!("database connected, migrations applied");

    // §13.1 step 2: Load or create node identity
    let node_id = {
        let rows = sqlx::query_as::<_, (String,)>("SELECT node_id FROM node_meta LIMIT 1")
            .fetch_optional(storage.pool())
            .await?;

        match rows {
            Some((id,)) => {
                info!(node_id = %id, "loaded existing node");
                id
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let meta = NodeMeta {
                    node_id: id.clone(),
                    host: cli.host.clone(),
                    port: cli.port,
                    current_epoch: 0, // will be incremented next
                    next_seq: 1,
                };
                storage.save_node_meta(&meta).await?;
                info!(node_id = %id, "created new node");
                id
            }
        }
    };

    // §13.1 step 3: Increment epoch (atomic, crash-safe)
    let current_epoch = storage.increment_epoch(&node_id).await?;
    info!(node_id = %node_id, epoch = current_epoch, "epoch incremented");

    // §13.1 steps 4-5: Create BatchWriter channel + build SharedState (rebuilds caches)
    let (batch_tx, batch_rx) = tokio::sync::mpsc::unbounded_channel();

    let shared = state::SharedState::new(
        node_id.clone(),
        advertise_addr.clone(),
        current_epoch,
        (*storage).clone(),
        batch_tx,
    )
    .await;

    info!(events = shared.event_count(), "state rebuilt from database");

    // §13.1 step 8: Start background tasks
    let shared_for_orphan: Arc<dyn orphan_detector::UnpersistedSource> = Arc::new(shared.clone());

    // BatchWriter
    let batch_writer = batch_writer::BatchWriter::new(
        batch_rx,
        storage.clone(),
        cli.batch_flush_interval_ms,
        cli.batch_flush_size,
        cli.matview_refresh_ms,
    );
    tokio::spawn(batch_writer.run());

    // OrphanDetector
    let orphan = orphan_detector::OrphanDetector::new(
        shared_for_orphan,
        storage.clone(),
        cli.orphan_check_interval_ms,
        cli.orphan_age_ms,
    );
    tokio::spawn(orphan.run());

    // §13.1 step 9: Build router + start serving
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
    axum::serve(listener, app).await?;
    Ok(())
}
