use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::future::join_all;
use shardd_types::{CreateEventRequest, CreateEventResponse, HealthResponse};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

#[derive(Parser)]
#[command(name = "shardd-bench", about = "Benchmark sync propagation latency")]
struct Cli {
    /// Number of nodes to spawn
    #[arg(long, default_value = "16")]
    nodes: usize,

    /// Number of benchmark rounds
    #[arg(long, default_value = "5")]
    rounds: usize,

    /// Max convergence time in ms (exit 1 if exceeded)
    #[arg(long, default_value = "5000")]
    threshold_ms: u64,

    /// Starting port number
    #[arg(long, default_value = "4001")]
    base_port: u16,

    /// Path to shardd-node binary
    #[arg(long, default_value = "target/release/shardd-node")]
    node_bin: PathBuf,

    /// Show node output
    #[arg(long)]
    verbose: bool,

    /// Extra warmup seconds after health checks pass
    #[arg(long, default_value = "2")]
    warmup: u64,

    /// Max peers per node (default: node count)
    #[arg(long)]
    max_peers: Option<usize>,
}

struct Cluster {
    children: Vec<Child>,
    _temp_dirs: Vec<tempfile::TempDir>,
}

impl Cluster {
    async fn spawn(cli: &Cli) -> Result<Self> {
        let mut children = Vec::new();
        let mut temp_dirs = Vec::new();

        for i in 0..cli.nodes {
            let port = cli.base_port + i as u16;
            let dir = tempfile::tempdir().context("create temp dir")?;

            let mut cmd = Command::new(&cli.node_bin);
            cmd.arg("--host").arg("0.0.0.0");
            cmd.arg("--port").arg(port.to_string());
            cmd.arg("--advertise-addr")
                .arg(format!("127.0.0.1:{port}"));
            cmd.arg("--config-dir").arg(dir.path());
            cmd.arg("--max-peers")
                .arg(cli.max_peers.unwrap_or(cli.nodes).to_string());
            cmd.arg("--sync-interval-ms").arg("1000");

            // Bootstrap: nodes 2..N bootstrap from node 1
            if i > 0 {
                cmd.arg("--bootstrap")
                    .arg(format!("127.0.0.1:{}", cli.base_port));
            }
            // Also bootstrap from a neighbor for ring connectivity
            if i > 1 {
                cmd.arg("--bootstrap")
                    .arg(format!("127.0.0.1:{}", cli.base_port + i as u16 - 1));
            }

            if cli.verbose {
                cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            } else {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }

            let child = cmd.spawn().with_context(|| {
                format!("spawn node {} on port {}", i + 1, port)
            })?;
            children.push(child);
            temp_dirs.push(dir);

            // Small stagger to avoid port conflicts during startup
            if i == 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            } else {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        Ok(Self {
            children,
            _temp_dirs: temp_dirs,
        })
    }

    fn kill_all(&mut self) {
        for child in &mut self.children {
            let _ = child.start_kill();
        }
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        self.kill_all();
    }
}

async fn wait_healthy(client: &reqwest::Client, urls: &[String], timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!("Timed out waiting for nodes to become healthy");
        }
        let futs: Vec<_> = urls
            .iter()
            .map(|url| {
                let client = client.clone();
                let url = format!("{url}/health");
                async move {
                    client
                        .get(&url)
                        .timeout(Duration::from_millis(500))
                        .send()
                        .await
                        .is_ok()
                }
            })
            .collect();
        let results = join_all(futs).await;
        let healthy = results.iter().filter(|r| **r).count();
        if healthy == urls.len() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn get_event_count(
    client: &reqwest::Client,
    url: &str,
) -> Option<usize> {
    client
        .get(format!("{url}/health"))
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .ok()?
        .json::<HealthResponse>()
        .await
        .ok()
        .map(|h| h.event_count)
}

struct RoundResult {
    convergence_ms: f64,
    per_node_latencies_ms: Vec<f64>,
    converged: usize,
    total: usize,
}

async fn run_round(
    client: &reqwest::Client,
    urls: &[String],
    timeout: Duration,
) -> Result<RoundResult> {
    let n = urls.len();

    // Get baseline event count
    let baseline = get_event_count(client, &urls[0])
        .await
        .context("get baseline event count")?;

    // Create event on node 1
    let resp = client
        .post(format!("{}/events", urls[0]))
        .json(&CreateEventRequest {
            bucket: "bench".into(),
            account: "default".into(),
            amount: 1,
            note: Some("bench".into()),
            max_overdraft: None,
        })
        .timeout(Duration::from_secs(5))
        .send()
        .await?
        .json::<CreateEventResponse>()
        .await?;

    let target = resp.event_count;
    assert!(target > baseline);

    let start = Instant::now();
    let mut first_seen: Vec<Option<f64>> = vec![None; n];
    // Node 0 (creator) already has it
    first_seen[0] = Some(0.0);

    loop {
        let elapsed = start.elapsed();
        if elapsed > timeout {
            break;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Poll all nodes concurrently
        let futs: Vec<_> = urls
            .iter()
            .enumerate()
            .map(|(i, url)| {
                let client = client.clone();
                let url = url.clone();
                async move { (i, get_event_count(&client, &url).await) }
            })
            .collect();
        let results = join_all(futs).await;

        for (i, count) in results {
            if first_seen[i].is_none() {
                if let Some(c) = count {
                    if c >= target {
                        first_seen[i] = Some(elapsed_ms);
                    }
                }
            }
        }

        // Check if all converged
        if first_seen.iter().all(|s| s.is_some()) {
            break;
        }
    }

    let converged = first_seen.iter().filter(|s| s.is_some()).count();
    let per_node: Vec<f64> = first_seen.iter().filter_map(|s| *s).collect();
    let convergence = per_node
        .iter()
        .copied()
        .fold(0.0_f64, f64::max);

    Ok(RoundResult {
        convergence_ms: convergence,
        per_node_latencies_ms: per_node,
        converged,
        total: n,
    })
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    println!(
        "shardd-bench: {} nodes, {} rounds, threshold {}ms\n",
        cli.nodes, cli.rounds, cli.threshold_ms
    );

    // Build URLs
    let urls: Vec<String> = (0..cli.nodes)
        .map(|i| format!("http://127.0.0.1:{}", cli.base_port + i as u16))
        .collect();

    // Spawn cluster
    print!("Spawning {} nodes...", cli.nodes);
    let mut cluster = Cluster::spawn(&cli).await?;
    println!(" done");

    // Wait for health
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(cli.nodes)
        .build()?;

    print!("Waiting for nodes to be healthy...");
    wait_healthy(&client, &urls, Duration::from_secs(30)).await?;
    println!(" done");

    // Warmup: let peer discovery propagate
    print!("Warmup ({}s)...", cli.warmup);
    tokio::time::sleep(Duration::from_secs(cli.warmup)).await;
    println!(" done\n");

    let timeout = Duration::from_millis(cli.threshold_ms * 2);
    let mut round_results = Vec::new();
    let mut all_per_node = Vec::new();
    let mut passed = true;

    for round in 1..=cli.rounds {
        match run_round(&client, &urls, timeout).await {
            Ok(result) => {
                let status = if result.converged == result.total {
                    format!("{:.0}ms", result.convergence_ms)
                } else {
                    format!(
                        "TIMEOUT ({}/{} converged)",
                        result.converged, result.total
                    )
                };
                println!(
                    "Round {}: {} ({}/{} converged)",
                    round, status, result.converged, result.total
                );

                if result.convergence_ms > cli.threshold_ms as f64
                    || result.converged < result.total
                {
                    passed = false;
                }

                all_per_node.extend(result.per_node_latencies_ms.clone());
                round_results.push(result);
            }
            Err(e) => {
                println!("Round {}: ERROR: {}", round, e);
                passed = false;
            }
        }
    }

    // Summary
    println!("\n--- Summary ---");
    println!("  Nodes:        {}", cli.nodes);
    println!("  Rounds:       {}", cli.rounds);

    let mut convergences: Vec<f64> = round_results.iter().map(|r| r.convergence_ms).collect();
    convergences.sort_by(|a, b| a.partial_cmp(b).unwrap());

    if !convergences.is_empty() {
        println!(
            "  Convergence:  min={:.0}ms  p50={:.0}ms  p90={:.0}ms  max={:.0}ms",
            convergences.first().unwrap(),
            percentile(&convergences, 50.0),
            percentile(&convergences, 90.0),
            convergences.last().unwrap(),
        );
    }

    all_per_node.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !all_per_node.is_empty() {
        println!(
            "  Per-node:     p50={:.0}ms  p90={:.0}ms  p99={:.0}ms",
            percentile(&all_per_node, 50.0),
            percentile(&all_per_node, 90.0),
            percentile(&all_per_node, 99.0),
        );
    }

    println!("  Threshold:    {}ms", cli.threshold_ms);
    println!(
        "  Result:       {}",
        if passed { "PASS" } else { "FAIL" }
    );

    // Cleanup
    cluster.kill_all();

    if passed {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
