//! shardd-bench — libp2p load testing + convergence verification.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::future::join_all;
use shardd_broadcast::discovery::{
    derive_psk_from_cluster_key, load_psk_file, parse_bootstrap_peers,
};
use shardd_broadcast::mesh_client::{MeshClient, MeshClientConfig};
use shardd_types::{CreateEventRequest, NodeRpcRequest, NodeRpcResponse};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "shardd-bench", about = "libp2p shardd benchmark suite")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Single-node write throughput test.
    Throughput {
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
        /// Stable identity seed for the mesh client's libp2p keypair.
        /// Required only when --peer-cache-file is set.
        #[arg(long, env = "SHARDD_IDENTITY_SEED")]
        identity_seed: Option<String>,
        #[arg(long, default_value = "1000")]
        events: usize,
        #[arg(long, default_value = "50")]
        concurrency: usize,
    },
    /// Cross-region benchmark: per-region load + convergence + latency stats.
    CrossRegion {
        #[arg(long = "bootstrap-peer")]
        bootstrap_peer: Vec<String>,
        #[arg(long)]
        psk_file: Option<String>,
        #[arg(long, env = "SHARDD_CLUSTER_KEY")]
        cluster_key: Option<String>,
        #[arg(long, default_value = "5000")]
        discovery_timeout_ms: u64,
        #[arg(long)]
        peer_cache_file: Option<PathBuf>,
        /// Stable identity seed for the mesh client's libp2p keypair.
        /// Required only when --peer-cache-file is set.
        #[arg(long, env = "SHARDD_IDENTITY_SEED")]
        identity_seed: Option<String>,
        #[arg(long, default_value = "30")]
        duration_secs: u64,
        #[arg(long, default_value = "50")]
        concurrency: usize,
        #[arg(long, default_value = "10")]
        convergence_wait_secs: u64,
        #[arg(long, default_value = "bench")]
        bucket: String,
        #[arg(long, default_value = "global")]
        account: String,
        #[arg(long, default_value = "1000000000")]
        seed_amount: i64,
        /// Expected total number of mesh nodes. If set, the bench refuses
        /// to proceed unless discovery finds exactly this many nodes and
        /// every one of them converges to the same checksum. Catches
        /// silent partial-mesh situations where load succeeds but some
        /// fraction of the cluster never joined.
        #[arg(long)]
        expected_nodes: Option<usize>,
    },
}

#[derive(Clone)]
struct MeshRegionNode {
    name: String,
    peer_id: String,
}

#[derive(Default)]
struct Stats {
    samples: Vec<u64>,
}

impl Stats {
    fn add(&mut self, us: u64) {
        self.samples.push(us);
    }

    fn percentile(&mut self, p: f64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        self.samples.sort_unstable();
        let idx = ((self.samples.len() - 1) as f64 * p / 100.0) as usize;
        self.samples[idx]
    }

    fn mean(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let sum: u64 = self.samples.iter().sum();
        sum / self.samples.len() as u64
    }
}

fn parse_peer_id(peer_id: &str) -> Result<shardd_broadcast::libp2p_crate::PeerId> {
    peer_id
        .parse()
        .with_context(|| format!("invalid peer id: {peer_id}"))
}

fn mesh_region_name(node: &shardd_broadcast::mesh_client::MeshNode) -> String {
    node.advertise_addr
        .clone()
        .unwrap_or_else(|| node.node_id.clone())
}

fn create_event_request(bucket: &str, account: &str, amount: i64) -> CreateEventRequest {
    CreateEventRequest {
        bucket: bucket.to_string(),
        account: account.to_string(),
        amount,
        note: None,
        // Each bench write is a distinct logical operation — fresh UUID.
        idempotency_nonce: uuid::Uuid::new_v4().to_string(),
        max_overdraft: None,
        min_acks: None,
        ack_timeout_ms: None,
        hold_amount: None,
        hold_expires_at_unix_ms: None,
        settle_reservation: None,
        release_reservation: None,
        allow_reserved_bucket: false,
    }
}

async fn run_throughput_mesh(
    client: Arc<MeshClient>,
    node: MeshRegionNode,
    events: usize,
    concurrency: usize,
) -> Result<()> {
    println!("=== Single-node throughput ===");
    println!("  target: {}", node.name);
    println!("  events: {events}, concurrency: {concurrency}");

    rpc_expect_create(
        mesh_create_event(
            &client,
            &node.peer_id,
            create_event_request("bench", "test", 1_000_000_000),
        )
        .await?,
    )?;

    let start = Instant::now();
    let counter = Arc::new(AtomicU64::new(0));
    let mut tasks = Vec::new();

    let events_per_worker = events / concurrency;
    for _ in 0..concurrency {
        let client = client.clone();
        let peer_id = node.peer_id.clone();
        let counter = counter.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..events_per_worker {
                if let Ok(result) =
                    mesh_create_event(&client, &peer_id, create_event_request("bench", "test", -1))
                        .await
                    && rpc_is_create_ok(&result)
                {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    join_all(tasks).await;

    let elapsed = start.elapsed();
    let ok = counter.load(Ordering::Relaxed);
    let rps = ok as f64 / elapsed.as_secs_f64();
    println!(
        "  {ok} ok in {:.2}s ({:.0} events/sec)",
        elapsed.as_secs_f64(),
        rps
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_cross_region_mesh(
    client: Arc<MeshClient>,
    nodes: Vec<MeshRegionNode>,
    duration_secs: u64,
    concurrency: usize,
    convergence_wait_secs: u64,
    bucket: String,
    account: String,
    seed_amount: i64,
    expected_nodes: Option<usize>,
) -> Result<()> {
    println!("=== Cross-region benchmark ===");
    println!("  nodes:    {}", nodes.len());
    for node in &nodes {
        println!("            {:20} {}", node.name, node.peer_id);
    }
    if let Some(expected) = expected_nodes
        && nodes.len() != expected
    {
        anyhow::bail!(
            "expected {} mesh nodes, got {}: partial mesh, aborting before load",
            expected,
            nodes.len()
        );
    }
    println!("  duration: {duration_secs}s");
    println!("  concurrency per region: {concurrency}");
    println!();

    println!("--- Waiting for nodes to be ready ---");
    for node in &nodes {
        mesh_wait_healthy(&client, &node.peer_id).await?;
        println!("  {} UP", node.name);
    }
    println!();

    println!(
        "--- Seeding {} credit on {} ---",
        seed_amount, nodes[0].name
    );
    rpc_expect_create(
        mesh_create_event(
            &client,
            &nodes[0].peer_id,
            create_event_request(&bucket, &account, seed_amount),
        )
        .await?,
    )?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    println!();
    println!("--- Load phase ({duration_secs}s, {concurrency} concurrent writes/region) ---");

    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let mut region_tasks = Vec::new();
    for node in nodes.clone() {
        let client = client.clone();
        let bucket = bucket.clone();
        let account = account.clone();
        region_tasks.push(tokio::spawn(async move {
            let stats = Arc::new(tokio::sync::Mutex::new(Stats::default()));
            let counter = Arc::new(AtomicU64::new(0));
            let errors = Arc::new(AtomicU64::new(0));
            let mut workers = Vec::new();
            for _ in 0..concurrency {
                let client = client.clone();
                let peer_id = node.peer_id.clone();
                let bucket = bucket.clone();
                let account = account.clone();
                let stats = stats.clone();
                let counter = counter.clone();
                let errors = errors.clone();
                workers.push(tokio::spawn(async move {
                    while Instant::now() < deadline {
                        let t0 = Instant::now();
                        let result = mesh_create_event(
                            &client,
                            &peer_id,
                            create_event_request(&bucket, &account, -1),
                        )
                        .await;
                        let elapsed_us = t0.elapsed().as_micros() as u64;
                        match result {
                            Ok(response) if rpc_is_create_ok(&response) => {
                                counter.fetch_add(1, Ordering::Relaxed);
                                stats.lock().await.add(elapsed_us);
                            }
                            _ => {
                                errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }));
            }
            join_all(workers).await;
            let stats = Arc::try_unwrap(stats)
                .ok()
                .expect("stats still has refs")
                .into_inner();
            (
                node.name,
                counter.load(Ordering::Relaxed),
                errors.load(Ordering::Relaxed),
                stats,
            )
        }));
    }

    let load_start = Instant::now();
    let region_results: Vec<(String, u64, u64, Stats)> = join_all(region_tasks)
        .await
        .into_iter()
        .filter_map(|r| r.ok())
        .collect();
    let load_elapsed = load_start.elapsed();

    println!();
    println!("--- Per-region results ---");
    println!(
        "  {:20} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Region", "ok", "err", "rps", "mean(ms)", "p50(ms)", "p99(ms)"
    );
    println!("  {}", "-".repeat(90));
    let mut total_ok = 0u64;
    let mut total_err = 0u64;
    for (name, ok, err, mut s) in region_results {
        let rps = ok as f64 / load_elapsed.as_secs_f64();
        let mean_ms = s.mean() as f64 / 1000.0;
        let p50_ms = s.percentile(50.0) as f64 / 1000.0;
        let p99_ms = s.percentile(99.0) as f64 / 1000.0;
        println!(
            "  {:20} {:>10} {:>10} {:>10.0} {:>10.2} {:>10.2} {:>10.2}",
            name, ok, err, rps, mean_ms, p50_ms, p99_ms
        );
        total_ok += ok;
        total_err += err;
    }
    println!("  {}", "-".repeat(90));
    let total_rps = total_ok as f64 / load_elapsed.as_secs_f64();
    println!(
        "  {:20} {:>10} {:>10} {:>10.0}",
        "TOTAL", total_ok, total_err, total_rps
    );

    println!();
    println!("--- Convergence phase ({convergence_wait_secs}s wait) ---");
    tokio::time::sleep(Duration::from_secs(convergence_wait_secs)).await;

    let mut checksums: BTreeMap<String, String> = BTreeMap::new();
    let mut event_counts: BTreeMap<String, u64> = BTreeMap::new();
    for node in &nodes {
        match mesh_get_state(&client, &node.peer_id).await {
            Ok((count, checksum)) => {
                checksums.insert(node.name.clone(), checksum);
                event_counts.insert(node.name.clone(), count);
            }
            Err(error) => {
                println!("  {}: ERROR {error}", node.name);
            }
        }
    }

    println!();
    println!("--- Final state per node ---");
    println!(
        "  {:20} {:>12} {:>20}",
        "Region", "events", "checksum[..16]"
    );
    println!("  {}", "-".repeat(56));
    for node in &nodes {
        let count = event_counts.get(&node.name).copied().unwrap_or(0);
        let cs = checksums
            .get(&node.name)
            .map(|s| s.chars().take(16).collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        println!("  {:20} {:>12} {:>20}", node.name, count, cs);
    }

    let unique_checksums: std::collections::HashSet<_> = checksums.values().collect();
    let converged = unique_checksums.len() == 1;
    println!();
    if converged {
        println!("  ✓ CONVERGED: all nodes have identical checksum");
    } else {
        println!(
            "  ✗ DIVERGED: {} distinct checksums across {} nodes",
            unique_checksums.len(),
            nodes.len()
        );
    }

    let max_events = event_counts.values().max().copied().unwrap_or(0);
    let min_events = event_counts.values().min().copied().unwrap_or(0);
    let gap = max_events.saturating_sub(min_events);
    println!("  Event count: min={min_events} max={max_events} gap={gap}");

    // Hard-assert the outcome so the bench actually FAILS on divergence.
    // A printed "✗ DIVERGED" with a successful exit was load-bearing bug
    // cover: regressions in libp2p membership handling could pass the
    // bench with silent divergence. Node-count mismatch is also fatal —
    // missing nodes from `nodes` means discovery didn't find everyone.
    if checksums.len() != nodes.len() {
        anyhow::bail!(
            "node discovery incomplete: {} of {} nodes responded to state query",
            checksums.len(),
            nodes.len()
        );
    }
    if !converged {
        anyhow::bail!(
            "mesh did not converge: {} distinct checksums across {} nodes",
            unique_checksums.len(),
            nodes.len()
        );
    }

    println!();
    println!("=== DONE ===");
    Ok(())
}

fn rpc_is_create_ok(result: &shardd_types::NodeRpcResult) -> bool {
    matches!(result, Ok(NodeRpcResponse::CreateEvent(_)))
}

fn rpc_expect_create(result: shardd_types::NodeRpcResult) -> Result<()> {
    match result {
        Ok(NodeRpcResponse::CreateEvent(_)) => Ok(()),
        Ok(other) => anyhow::bail!("unexpected RPC response: {other:?}"),
        Err(error) => anyhow::bail!("RPC error: {}", error.message),
    }
}

async fn mesh_wait_healthy(client: &MeshClient, peer_id: &str) -> Result<()> {
    let peer_id = parse_peer_id(peer_id)?;
    for _ in 0..60 {
        if let Ok(result) = client.request_to(peer_id, NodeRpcRequest::Health).await
            && matches!(result, Ok(NodeRpcResponse::Health(_)))
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("node {peer_id} did not become healthy within 30s")
}

async fn mesh_get_state(client: &MeshClient, peer_id: &str) -> Result<(u64, String)> {
    let peer_id = parse_peer_id(peer_id)?;
    match client.request_to(peer_id, NodeRpcRequest::State).await? {
        Ok(NodeRpcResponse::State(state)) => Ok((state.event_count as u64, state.checksum)),
        Ok(other) => anyhow::bail!("unexpected RPC response: {other:?}"),
        Err(error) => anyhow::bail!("RPC error: {}", error.message),
    }
}

async fn mesh_create_event(
    client: &MeshClient,
    peer_id: &str,
    request: CreateEventRequest,
) -> Result<shardd_types::NodeRpcResult> {
    let peer_id = parse_peer_id(peer_id)?;
    client
        .request_to(peer_id, NodeRpcRequest::CreateEvent(request))
        .await
}

async fn discover_region_nodes_with_cache(
    bootstrap_peer: Vec<String>,
    psk_file: Option<String>,
    cluster_key: Option<String>,
    discovery_timeout_ms: u64,
    peer_cache_file: Option<PathBuf>,
    identity_seed: Option<String>,
    expected_nodes: Option<usize>,
) -> Result<(Arc<MeshClient>, Vec<MeshRegionNode>)> {
    if bootstrap_peer.is_empty() {
        anyhow::bail!("at least one --bootstrap-peer is required");
    }

    let mut config = MeshClientConfig::new(parse_bootstrap_peers(&bootstrap_peer)?);
    config.request_timeout = Duration::from_millis(discovery_timeout_ms);
    // Cache only if the user explicitly asked: bench runs are short-lived
    // and benefit from cold-start determinism. If the caller opts into a
    // cache they must also supply --identity-seed — MeshClient::start
    // enforces that invariant.
    config.cache_path = peer_cache_file;
    if let Some(seed) = identity_seed {
        config.identity_seed = seed;
    }
    config.psk = match (cluster_key, psk_file) {
        (Some(key), _) => Some(derive_psk_from_cluster_key(&key)?),
        (None, Some(path)) => Some(load_psk_file(path)?),
        (None, None) => None,
    };

    let client = Arc::new(MeshClient::start(config)?);
    // If the caller told us the expected size, wait for all of them —
    // otherwise just wait for a sane minimum and use whatever we get.
    let min_to_wait = expected_nodes.unwrap_or_else(|| bootstrap_peer.len().max(3));
    client
        .wait_for_min_candidates(min_to_wait, Duration::from_millis(discovery_timeout_ms * 2))
        .await?;

    let nodes = client
        .all_nodes()
        .into_iter()
        .map(|node| MeshRegionNode {
            name: mesh_region_name(&node),
            peer_id: node.peer_id,
        })
        .collect::<Vec<_>>();

    if nodes.is_empty() {
        anyhow::bail!("no libp2p-capable nodes discovered");
    }

    if let Some(expected) = expected_nodes
        && nodes.len() != expected
    {
        anyhow::bail!(
            "expected {} mesh nodes, discovered only {}: partial mesh, not \
             running bench (discovered: {:?})",
            expected,
            nodes.len(),
            nodes.iter().map(|n| &n.name).collect::<Vec<_>>(),
        );
    }

    Ok((client, nodes))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Throughput {
            bootstrap_peer,
            psk_file,
            cluster_key,
            discovery_timeout_ms,
            peer_cache_file,
            identity_seed,
            events,
            concurrency,
        } => {
            let (client, nodes) = discover_region_nodes_with_cache(
                bootstrap_peer,
                psk_file,
                cluster_key,
                discovery_timeout_ms,
                peer_cache_file,
                identity_seed,
                None,
            )
            .await?;
            let node = nodes
                .first()
                .cloned()
                .context("no node selected after discovery")?;
            run_throughput_mesh(client, node, events, concurrency).await
        }
        Commands::CrossRegion {
            bootstrap_peer,
            psk_file,
            cluster_key,
            discovery_timeout_ms,
            peer_cache_file,
            identity_seed,
            duration_secs,
            concurrency,
            convergence_wait_secs,
            bucket,
            account,
            seed_amount,
            expected_nodes,
        } => {
            let (client, nodes) = discover_region_nodes_with_cache(
                bootstrap_peer,
                psk_file,
                cluster_key,
                discovery_timeout_ms,
                peer_cache_file,
                identity_seed,
                expected_nodes,
            )
            .await?;
            run_cross_region_mesh(
                client,
                nodes,
                duration_secs,
                concurrency,
                convergence_wait_secs,
                bucket,
                account,
                seed_amount,
                expected_nodes,
            )
            .await
        }
    }
}
