use anyhow::Result;
use clap::{Parser, Subcommand};

use shardd_types::*;

#[derive(Parser)]
#[command(name = "shardd-cli", about = "CLI client for shardd nodes")]
struct Cli {
    /// Node URL to connect to
    #[arg(long, default_value = "http://127.0.0.1:3001", global = true)]
    node: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show node health
    Health,
    /// Show full node state
    State,
    /// List known peers
    Peers,
    /// Add a peer
    AddPeer {
        /// Peer address (host:port)
        #[arg(long)]
        addr: String,
    },
    /// Create a new event
    #[command(allow_negative_numbers = true)]
    CreateEvent {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Account name
        #[arg(long)]
        account: String,
        /// Event amount
        #[arg(long, allow_hyphen_values = true)]
        amount: i64,
        /// Optional note
        #[arg(long)]
        note: Option<String>,
    },
    /// List all events
    Events,
    /// Show contiguous heads
    Heads,
    /// Show all account balances
    Balances,
    /// Trigger manual sync
    Sync,
    /// Debug info for a specific origin
    DebugOrigin {
        /// Origin node ID
        origin: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();
    let base = &cli.node;

    match cli.command {
        Command::Health => {
            let resp: HealthResponse = client
                .get(format!("{base}/health"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::State => {
            let resp: StateResponse = client
                .get(format!("{base}/state"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::Peers => {
            let resp: Vec<String> = client
                .get(format!("{base}/peers"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::AddPeer { addr } => {
            let resp: serde_json::Value = client
                .post(format!("{base}/peers/add"))
                .json(&AddPeerRequest { addr })
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::CreateEvent {
            bucket,
            account,
            amount,
            note,
        } => {
            let resp: CreateEventResponse = client
                .post(format!("{base}/events"))
                .json(&CreateEventRequest {
                    bucket,
                    account,
                    amount,
                    note,
                    max_overdraft: None,
                })
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::Events => {
            let resp: Vec<Event> = client
                .get(format!("{base}/events"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::Heads => {
            let resp: std::collections::BTreeMap<String, u64> = client
                .get(format!("{base}/heads"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::Balances => {
            let resp: BalancesResponse = client
                .get(format!("{base}/balances"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::Sync => {
            let resp: SyncTriggerResponse = client
                .post(format!("{base}/sync"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Command::DebugOrigin { origin } => {
            let resp: DebugOriginResponse = client
                .get(format!("{base}/debug/origin/{origin}"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
    }

    Ok(())
}
