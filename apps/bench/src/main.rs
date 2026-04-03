//! shardd-bench v2 — load testing and convergence verification.

use clap::{Parser, Subcommand};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "shardd-bench")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Single-node write throughput test.
    Throughput {
        #[arg(long, default_value = "http://127.0.0.1:3001")]
        node: String,
        #[arg(long, default_value = "1000")]
        events: usize,
        #[arg(long, default_value = "10")]
        concurrency: usize,
    },
    /// Multi-node convergence test.
    Convergence {
        #[arg(long)]
        nodes: Vec<String>,
        #[arg(long, default_value = "100")]
        events_per_node: usize,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();

    match cli.command {
        Commands::Throughput { node, events, concurrency } => {
            println!("=== Write Throughput Test ===");
            println!("  Target: {node}");
            println!("  Events: {events}, Concurrency: {concurrency}");

            // Seed account
            client.post(format!("{node}/events"))
                .json(&serde_json::json!({"bucket":"bench","account":"test","amount":1_000_000_000i64}))
                .send().await?;

            let start = Instant::now();
            let mut handles: Vec<tokio::task::JoinHandle<bool>> = Vec::new();

            for chunk_start in (0..events).step_by(concurrency) {
                let mut batch = Vec::new();
                for _ in 0..concurrency.min(events - chunk_start) {
                    let c = client.clone();
                    let n = node.clone();
                    batch.push(tokio::spawn(async move {
                        c.post(format!("{n}/events"))
                            .json(&serde_json::json!({"bucket":"bench","account":"test","amount":-1}))
                            .send().await.is_ok()
                    }));
                }
                for h in batch {
                    let _ = h.await;
                }
            }

            let elapsed = start.elapsed();
            let rps = events as f64 / elapsed.as_secs_f64();
            println!("  Completed in {:.2}s ({:.0} events/sec)", elapsed.as_secs_f64(), rps);

            // Final state
            let resp = client.get(format!("{node}/state")).send().await?.text().await?;
            println!("  State: {resp}");
        }

        Commands::Convergence { nodes, events_per_node } => {
            if nodes.is_empty() {
                println!("Error: provide --nodes http://host:port for each node");
                return Ok(());
            }

            println!("=== Convergence Test ===");
            println!("  Nodes: {}", nodes.len());
            println!("  Events per node: {events_per_node}");

            // Seed on first node
            client.post(format!("{}/events", nodes[0]))
                .json(&serde_json::json!({"bucket":"bench","account":"test","amount":1_000_000_000i64}))
                .send().await?;

            // Create events on each node
            for (i, node) in nodes.iter().enumerate() {
                let start = Instant::now();
                for _ in 0..events_per_node {
                    client.post(format!("{node}/events"))
                        .json(&serde_json::json!({"bucket":"bench","account":"test","amount":-1}))
                        .send().await?;
                }
                println!("  Node {i}: {events_per_node} events in {:.2}s", start.elapsed().as_secs_f64());
            }

            // Wait for convergence
            println!("  Waiting 10s for sync...");
            tokio::time::sleep(Duration::from_secs(10)).await;

            // Check states
            let mut checksums = Vec::new();
            for (i, node) in nodes.iter().enumerate() {
                let resp: serde_json::Value = client.get(format!("{node}/state"))
                    .send().await?.json().await?;
                let events = resp["event_count"].as_u64().unwrap_or(0);
                let checksum = resp["checksum"].as_str().unwrap_or("?");
                println!("  Node {i}: events={events} checksum={}", &checksum[..16.min(checksum.len())]);
                checksums.push(checksum.to_string());
            }

            let all_match = checksums.windows(2).all(|w| w[0] == w[1]);
            if all_match {
                println!("  ✓ ALL NODES CONVERGED");
            } else {
                println!("  ✗ CHECKSUMS DIFFER — nodes have not converged");
            }
        }
    }

    Ok(())
}
