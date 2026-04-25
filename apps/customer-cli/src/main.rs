//! `shardd` — customer-facing CLI for shardd.
//!
//! Authenticates via a browser-based device flow against the dashboard at
//! app.shardd.xyz, stores the issued API key at
//! ~/.config/shardd/credentials.toml, and uses it for both:
//!
//! - the data plane via `shardd::Client` (the published Rust SDK), for
//!   events / balances / accounts / edges / health, and
//! - the dashboard control plane via raw HTTPS, for buckets / keys /
//!   profile / billing.
//!
//! The two planes share one API key and one user identity; the
//! dashboard's `Authenticated` extractor accepts the same Bearer
//! header that the SDK already sends.

mod cmd;
mod credentials;
mod http;
mod output;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "shardd",
    about = "Customer-facing CLI for shardd",
    version,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Override the dashboard URL for this invocation. Default
    /// resolution order: --dashboard-url > $SHARDD_DASHBOARD_URL >
    /// the URL stored in ~/.config/shardd/credentials.toml >
    /// https://app.shardd.xyz.
    #[arg(long, global = true, env = "SHARDD_DASHBOARD_URL")]
    dashboard_url: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Browser device-flow login, logout, whoami.
    Auth {
        #[command(subcommand)]
        cmd: cmd::auth::AuthCmd,
    },
    /// Credit, debit, hold, list events.
    Events {
        #[command(subcommand)]
        cmd: cmd::events::EventsCmd,
    },
    /// All balances in a bucket.
    Balances {
        #[command(subcommand)]
        cmd: cmd::balances::BalancesCmd,
    },
    /// One account's balance + holds.
    Accounts {
        #[command(subcommand)]
        cmd: cmd::accounts::AccountsCmd,
    },
    /// List, create, archive, permanuke buckets.
    Buckets {
        #[command(subcommand)]
        cmd: cmd::buckets::BucketsCmd,
    },
    /// Manage developer API keys and their scopes.
    Keys {
        #[command(subcommand)]
        cmd: cmd::keys::KeysCmd,
    },
    /// Read or update your developer profile; export account data.
    Profile {
        #[command(subcommand)]
        cmd: cmd::profile::ProfileCmd,
    },
    /// Plan, usage, hosted Stripe portal.
    Billing {
        #[command(subcommand)]
        cmd: cmd::billing::BillingCmd,
    },
    /// Print the regional edge directory the SDK would use.
    Edges,
    /// Probe a single edge's `/gateway/health`.
    Health {
        #[arg(long)]
        edge: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dashboard_url_override = cli.dashboard_url.as_deref();

    match cli.command {
        Commands::Auth { cmd } => cmd::auth::run(cmd, dashboard_url_override).await,
        Commands::Events { cmd } => cmd::events::run(cmd, dashboard_url_override).await,
        Commands::Balances { cmd } => cmd::balances::run(cmd, dashboard_url_override).await,
        Commands::Accounts { cmd } => cmd::accounts::run(cmd, dashboard_url_override).await,
        Commands::Buckets { cmd } => cmd::buckets::run(cmd, dashboard_url_override).await,
        Commands::Keys { cmd } => cmd::keys::run(cmd, dashboard_url_override).await,
        Commands::Profile { cmd } => cmd::profile::run(cmd, dashboard_url_override).await,
        Commands::Billing { cmd } => cmd::billing::run(cmd, dashboard_url_override).await,
        Commands::Edges => cmd::edges::run(dashboard_url_override).await,
        Commands::Health { edge } => cmd::health::run(edge, dashboard_url_override).await,
    }
}
