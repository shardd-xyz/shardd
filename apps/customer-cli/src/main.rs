//! `shardd` — customer-facing CLI for shardd.
//!
//! Authenticates via a browser-based device flow against the dashboard at
//! app.shardd.xyz, then calls the public Rust SDK (shardd::Client) for the
//! data plane and the dashboard's /api/developer/* endpoints for the
//! control plane (buckets, keys, billing, profile).
//!
//! This file is a Milestone A scaffold. The full subcommand surface lands
//! in Milestone D — see `.claude/plans/let-s-make-sure-landing-validated-harbor.md`.

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "shardd", about = "Customer-facing CLI for shardd", version)]
struct Cli {}

fn main() -> Result<()> {
    let _ = Cli::parse();
    eprintln!(
        "shardd-cli v{} — scaffold only. Full CLI lands in the next milestone.",
        env!("CARGO_PKG_VERSION")
    );
    Ok(())
}
