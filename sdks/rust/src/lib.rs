//! # shardd
//!
//! Official Rust client for [shardd](https://shardd.xyz), a globally
//! distributed credit ledger. The client probes the three prod edge
//! regions on first use, sticks to the closest healthy one, and fails
//! over to the next-best on transient errors — all transparently.
//!
//! ## Quickstart
//!
//! ```no_run
//! use shardd::{Client, CreateEventOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let api_key = std::env::var("SHARDD_API_KEY")?;
//!     let client = Client::new(api_key)?;
//!
//!     // Credit $5 to user 42 in the `my-app` bucket.
//!     let result = client
//!         .create_event("my-app", "user:42", 500, Default::default())
//!         .await?;
//!     println!("event {} → balance {}", result.event.event_id, result.balance);
//!
//!     // Retrieve it.
//!     let balances = client.get_balances("my-app").await?;
//!     for row in balances.accounts {
//!         println!("  {} = {}", row.account, row.balance);
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Failover
//!
//! The client defaults to the three prod regions (`use1`, `euc1`,
//! `ape1`). On construction it does nothing; on the first request it
//! parallel-probes `/gateway/health` on all three, picks the lowest-
//! latency healthy one, and sticks with it. If that edge returns 503/
//! 504/timeouts, the client fails over once to the next-best candidate,
//! re-using the same `idempotency_nonce` so a partially-landed first
//! attempt collapses into a single event on the server.
//!
//! See [`Client::builder`] to override edges, timeout, or the HTTP
//! client.

mod client;
mod edges;
mod error;
mod types;

pub use client::{Client, ClientBuilder};
pub use error::ShardError;
pub use types::{
    AccountBalance, AccountDetail, AckInfo, Balances, CreateEventOptions, CreateEventResult,
    EdgeHealth, EdgeInfo, Event, EventList, Reservation,
};
