//! shardd CLI v2 — HTTP client for all v2 API endpoints.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "shardd-cli", about = "CLI for shardd node v2")]
struct Cli {
    #[arg(long, default_value = "http://127.0.0.1:3001")]
    node: String,
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
        #[arg(long)] bucket: String,
        #[arg(long)] account: String,
        #[arg(long)] amount: i64,
        #[arg(long)] note: Option<String>,
        #[arg(long)] idempotency_nonce: Option<String>,
        #[arg(long, default_value = "0")] max_overdraft: u64,
        #[arg(long, default_value = "0")] min_acks: u32,
        #[arg(long, default_value = "500")] ack_timeout_ms: u64,
    },
    DebugOrigin { origin_id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();
    let base = &cli.node;

    match cli.command {
        Commands::Health => print(client.get(format!("{base}/health")).send().await?).await,
        Commands::State => print(client.get(format!("{base}/state")).send().await?).await,
        Commands::Events => print(client.get(format!("{base}/events")).send().await?).await,
        Commands::Heads => print(client.get(format!("{base}/heads")).send().await?).await,
        Commands::Balances => print(client.get(format!("{base}/balances")).send().await?).await,
        Commands::Collapsed => print(client.get(format!("{base}/collapsed")).send().await?).await,
        Commands::Persistence => print(client.get(format!("{base}/persistence")).send().await?).await,
        Commands::Registry => print(client.get(format!("{base}/registry")).send().await?).await,
        Commands::DebugOrigin { origin_id } =>
            print(client.get(format!("{base}/debug/origin/{origin_id}")).send().await?).await,
        Commands::CreateEvent { bucket, account, amount, note, idempotency_nonce, max_overdraft, min_acks, ack_timeout_ms } =>
            print(client.post(format!("{base}/events"))
                .json(&serde_json::json!({
                    "bucket": bucket, "account": account, "amount": amount,
                    "note": note, "idempotency_nonce": idempotency_nonce,
                    "max_overdraft": max_overdraft, "min_acks": min_acks,
                    "ack_timeout_ms": ack_timeout_ms,
                })).send().await?).await,
    }
    Ok(())
}

async fn print(resp: reqwest::Response) {
    println!("{}", resp.text().await.unwrap_or_else(|e| format!("Error: {e}")));
}
