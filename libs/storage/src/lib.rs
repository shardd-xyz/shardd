//! shardd-storage v2 — Persistence layer per protocol.md v1.7.
//!
//! Defines the `StorageBackend` trait and provides:
//! - `PostgresStorage` — production implementation using sqlx
//! - `InMemoryStorage` — for unit tests (task 0004)

pub mod postgres;

use anyhow::Result;
use std::collections::BTreeMap;
use std::future::Future;

use shardd_types::{Event, NodeMeta, NodeRegistryEntry};

/// Result of attempting to insert an event.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertResult {
    /// Event was newly inserted.
    Inserted,
    /// Event already existed with same dedup key and same event_id.
    Duplicate,
    /// Dedup key collision with a DIFFERENT event_id (data corruption).
    Conflict { details: String },
}

/// Async storage backend trait per protocol §6.
pub trait StorageBackend: Send + Sync + 'static {
    // ── Event writes ─────────────────────────────────────────────────
    fn insert_event(&self, event: &Event) -> impl Future<Output = Result<InsertResult>> + Send;
    fn insert_events_bulk(&self, events: &[Event]) -> impl Future<Output = Result<usize>> + Send;

    // ── Event reads ──────────────────────────────────────────────────
    fn query_events_range(
        &self, origin: &str, epoch: u32, from_seq: u64, to_seq: u64,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn query_all_events_sorted(&self) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn event_count(&self) -> impl Future<Output = Result<usize>> + Send;

    // ── Balance/head rebuild ─────────────────────────────────────────
    fn aggregate_balances(&self) -> impl Future<Output = Result<Vec<(String, String, i64)>>> + Send;
    fn sequences_by_origin_epoch(&self) -> impl Future<Output = Result<BTreeMap<(String, u32), Vec<u64>>>> + Send;
    /// Returns (origin_node_id, origin_epoch, bucket, account) tuples.
    fn origin_account_epoch_mapping(&self) -> impl Future<Output = Result<Vec<(String, u32, String, String)>>> + Send;

    // ── Idempotency ──────────────────────────────────────────────────
    fn find_by_idempotency_key(
        &self, nonce: &str, bucket: &str, account: &str, amount: i64,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;

    // ── Holds ────────────────────────────────────────────────────────
    fn active_holds(&self, now_ms: u64) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn released_hold_refs(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

    // ── Checksum ─────────────────────────────────────────────────────
    fn checksum_data(&self) -> impl Future<Output = Result<String>> + Send;

    // ── Node meta ────────────────────────────────────────────────────
    fn load_node_meta(&self, node_id: &str) -> impl Future<Output = Result<Option<NodeMeta>>> + Send;
    fn save_node_meta(&self, meta: &NodeMeta) -> impl Future<Output = Result<()>> + Send;
    fn increment_epoch(&self, node_id: &str) -> impl Future<Output = Result<u32>> + Send;
    fn derive_next_seq(&self, node_id: &str, epoch: u32) -> impl Future<Output = Result<u64>> + Send;

    // ── Node registry ────────────────────────────────────────────────
    fn upsert_registry_entry(&self, entry: &NodeRegistryEntry) -> impl Future<Output = Result<()>> + Send;
    fn load_registry(&self) -> impl Future<Output = Result<Vec<NodeRegistryEntry>>> + Send;
    fn decommission_node(&self, node_id: &str) -> impl Future<Output = Result<()>> + Send;

    // ── Matview ──────────────────────────────────────────────────────
    fn refresh_balance_summary(&self) -> impl Future<Output = Result<()>> + Send;
    fn read_balance_summary(&self) -> impl Future<Output = Result<Vec<(String, String, i64)>>> + Send;

    // ── Migrations ───────────────────────────────────────────────────
    fn run_migrations(&self) -> impl Future<Output = Result<()>> + Send;
}
