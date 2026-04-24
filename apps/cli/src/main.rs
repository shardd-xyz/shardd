//! shardd CLI v2 — libp2p mesh client for shardd endpoints.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use shardd_broadcast::discovery::{
    derive_psk_from_cluster_key, load_psk_file, parse_bootstrap_peers,
};
use shardd_broadcast::mesh_client::{MeshClient, MeshClientConfig};
use shardd_types::{NodeRpcError, NodeRpcRequest, NodeRpcResponse};

#[derive(Parser)]
#[command(name = "shardd-cli", about = "CLI for shardd mesh v2")]
struct Cli {
    #[arg(long = "bootstrap-peer")]
    bootstrap_peer: Vec<String>,
    #[arg(long)]
    psk_file: Option<String>,
    #[arg(long, env = "SHARDD_CLUSTER_KEY")]
    cluster_key: Option<String>,
    #[arg(long, default_value = "3000")]
    discovery_timeout_ms: u64,
    #[arg(long)]
    peer_cache_file: Option<PathBuf>,
    /// Stable identity seed for the mesh client's libp2p keypair. Required
    /// when --peer-cache-file is set, because cached peer entries are keyed
    /// by PeerId and only remain valid if the PeerId is stable across runs.
    /// Without a cache, this can be left unset and a random keypair is used.
    #[arg(long, env = "SHARDD_IDENTITY_SEED")]
    identity_seed: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Health,
    State,
    Events,
    Heads,
    Balances,
    Collapsed,
    Persistence,
    Registry,
    CreateEvent {
        #[arg(long)]
        bucket: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        amount: i64,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        idempotency_nonce: Option<String>,
        #[arg(long, default_value = "0")]
        max_overdraft: u64,
        #[arg(long, default_value = "0")]
        min_acks: u32,
        #[arg(long, default_value = "500")]
        ack_timeout_ms: u64,
    },
    DebugOrigin {
        origin_id: String,
    },
    Digests,
    /// Emit a `BucketDelete` meta event (§3.5). Hard-deletes the bucket
    /// cluster-wide. There is no undo. Intended for ops / integration
    /// tests — the dashboard's permanuke flow is the normal UX.
    DeleteBucket {
        #[arg(long)]
        bucket: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = build_mesh_client(&cli).await?;
    run_mesh(cli.command, client).await
}

async fn build_mesh_client(cli: &Cli) -> Result<MeshClient> {
    if cli.bootstrap_peer.is_empty() {
        bail!("at least one --bootstrap-peer is required");
    }

    let mut config = MeshClientConfig::new(parse_bootstrap_peers(&cli.bootstrap_peer)?);
    config.request_timeout = Duration::from_millis(cli.discovery_timeout_ms);
    // Cache only if the user explicitly asked: short-lived CLI invocations
    // don't benefit from a persistent peer cache, and enabling one would
    // force the caller to also provide a stable identity_seed.
    config.cache_path = cli.peer_cache_file.clone();
    if let Some(seed) = cli.identity_seed.as_ref() {
        config.identity_seed = seed.clone();
    }
    config.psk = match (&cli.cluster_key, &cli.psk_file) {
        (Some(key), _) => Some(derive_psk_from_cluster_key(key)?),
        (None, Some(path)) => Some(load_psk_file(path)?),
        (None, None) => None,
    };

    let client = MeshClient::start(config)?;
    client
        .wait_for_min_candidates(1, Duration::from_millis(cli.discovery_timeout_ms * 2))
        .await?;
    client
        .best_node()
        .context("no libp2p-capable nodes discovered")?;
    Ok(client)
}

async fn run_mesh(command: Commands, client: MeshClient) -> Result<()> {
    let response = match command {
        Commands::Health => client.request_best(NodeRpcRequest::Health).await?,
        Commands::State => client.request_best(NodeRpcRequest::State).await?,
        Commands::Events => client.request_best(NodeRpcRequest::Events).await?,
        Commands::Heads => client.request_best(NodeRpcRequest::Heads).await?,
        Commands::Balances => client.request_best(NodeRpcRequest::Balances).await?,
        Commands::Collapsed => client.request_best(NodeRpcRequest::Collapsed).await?,
        Commands::Persistence => client.request_best(NodeRpcRequest::Persistence).await?,
        Commands::Registry => client.request_best(NodeRpcRequest::Registry).await?,
        Commands::Digests => client.request_best(NodeRpcRequest::Digests).await?,
        Commands::DebugOrigin { origin_id } => {
            client
                .request_best(NodeRpcRequest::DebugOrigin { origin_id })
                .await?
        }
        Commands::CreateEvent {
            bucket,
            account,
            amount,
            note,
            idempotency_nonce,
            max_overdraft,
            min_acks,
            ack_timeout_ms,
        } => {
            client
                .request_best(NodeRpcRequest::CreateEvent(
                    shardd_types::CreateEventRequest {
                        bucket,
                        account,
                        amount,
                        note,
                        // CLI invocations are one-shot. If the user didn't
                        // supply a nonce, generate a fresh UUID so the
                        // command still satisfies the "every event has a
                        // nonce" invariant — retries require passing
                        // --idempotency-nonce explicitly.
                        idempotency_nonce: idempotency_nonce
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        max_overdraft: Some(max_overdraft),
                        min_acks: Some(min_acks),
                        ack_timeout_ms: Some(ack_timeout_ms),
                    },
                ))
                .await?
        }
        Commands::DeleteBucket { bucket, reason } => {
            client
                .request_best(NodeRpcRequest::DeleteBucket { bucket, reason })
                .await?
        }
    };
    print_rpc_result(response)
}

fn print_rpc_result(result: shardd_types::NodeRpcResult) -> Result<()> {
    match result {
        Ok(response) => match response {
            NodeRpcResponse::CreateEvent(body) => print_json(&body),
            NodeRpcResponse::Health(body) => print_json(&body),
            NodeRpcResponse::State(body) => print_json(&body),
            NodeRpcResponse::Events(body) => print_json(&body),
            NodeRpcResponse::Heads(body) => print_json(&body),
            NodeRpcResponse::Balances(body) => print_json(&body),
            NodeRpcResponse::Collapsed(body) => print_json(&body),
            NodeRpcResponse::CollapsedAccount(body) => print_json(&body),
            NodeRpcResponse::Persistence(body) => print_json(&body),
            NodeRpcResponse::Digests(body) => print_json(&body),
            NodeRpcResponse::DebugOrigin(body) => print_json(&body),
            NodeRpcResponse::Registry(body) => print_json(&body),
            NodeRpcResponse::DeleteBucket(body) => print_json(&body),
            NodeRpcResponse::EventsFilter(body) => print_json(&body),
            NodeRpcResponse::DeletedBuckets(body) => print_json(&body),
        },
        Err(error) => print_json(&rpc_error_body(error)),
    }
}

fn rpc_error_body(error: NodeRpcError) -> serde_json::Value {
    match error.code {
        shardd_types::NodeRpcErrorCode::InsufficientFunds => serde_json::to_value(
            error
                .insufficient_funds
                .expect("insufficient_funds payload must be present"),
        )
        .expect("insufficient funds json"),
        _ => serde_json::json!({ "error": error.message }),
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
