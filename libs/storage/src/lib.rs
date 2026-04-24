//! shardd-storage v3 — Persistence layer per protocol.md v1.8.
//!
//! Defines the `StorageBackend` trait and provides:
//! - `PostgresStorage` — production implementation using sqlx
//! - `InMemoryStorage` — for unit tests

pub mod memory;
pub mod postgres;

use anyhow::Result;
use std::collections::BTreeMap;
use std::future::Future;

use shardd_types::{EpochKey, Event, NodeMeta, NodeRegistryEntry};

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

/// Durable state for a per-`(bucket, node_id)` seq/epoch allocator.
#[derive(Debug, Clone)]
pub struct BucketAllocatorRow {
    pub bucket: String,
    pub node_id: String,
    pub current_epoch: u32,
    pub next_seq: u64,
    pub needs_bump: bool,
}

/// Optional filter predicates for `query_events_filtered`. `None` means
/// "any" for each field. `event_type` is matched against the wire string
/// ("standard", "void", "hold_release", "reservation_create",
/// "bucket_delete") exactly. `search` does a case-sensitive substring
/// match against `note` and `event_id`. `bucket_prefix` is a literal
/// prefix match against the bucket column — used to scope cluster-wide
/// listings to a single user's namespace without a full `bucket` match.
#[derive(Debug, Clone, Default)]
pub struct EventsFilter {
    pub bucket: Option<String>,
    pub bucket_prefix: Option<String>,
    pub account: Option<String>,
    pub origin: Option<String>,
    pub event_type: Option<String>,
    pub since_unix_ms: Option<u64>,
    pub until_unix_ms: Option<u64>,
    pub search: Option<String>,
}

/// Async storage backend trait per protocol §6.
pub trait StorageBackend: Send + Sync + 'static {
    // ── Event writes ─────────────────────────────────────────────────
    fn insert_event(&self, event: &Event) -> impl Future<Output = Result<InsertResult>> + Send;
    fn insert_events_bulk(&self, events: &[Event]) -> impl Future<Output = Result<usize>> + Send;

    // ── Event reads ──────────────────────────────────────────────────
    /// Read events in `[from_seq, to_seq]` for a single
    /// `(bucket, origin, epoch)` triple.
    fn query_events_range(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        from_seq: u64,
        to_seq: u64,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;
    /// Read every event for one bucket, sorted by `(origin, epoch, seq)`.
    /// Used at startup to replay the `__meta__` log and rebuild the
    /// `deleted_buckets` set from durable truth.
    fn query_events_by_bucket(
        &self,
        bucket: &str,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn query_all_events_sorted(&self) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn event_count(&self) -> impl Future<Output = Result<usize>> + Send;

    /// Paginated search over every event in the cluster's local store.
    /// Returns `(page, total_matching)` where `page` is at most `limit`
    /// rows, sorted newest-first by `created_at_unix_ms` (tiebreaker
    /// `event_id` desc for determinism). `total_matching` is the full
    /// count for the filter — not the page size — so the UI can render a
    /// paginator. Drives `/api/admin/events`.
    fn query_events_filtered(
        &self,
        filter: &EventsFilter,
        limit: u32,
        offset: u32,
    ) -> impl Future<Output = Result<(Vec<Event>, u64)>> + Send;

    // ── Bucket-wide cascade delete (§3.5) ────────────────────────────
    /// Atomically drop all rows for `bucket` from `events`,
    /// `rolling_digests`, and `bucket_seq_allocator`. Called when the
    /// node applies a `BucketDelete` meta event. Refuses to touch
    /// `META_BUCKET` — the meta log itself is never deleted.
    fn delete_bucket_cascade(&self, bucket: &str) -> impl Future<Output = Result<()>> + Send;

    // ── Balance/head rebuild ─────────────────────────────────────────
    fn aggregate_balances(&self)
    -> impl Future<Output = Result<Vec<(String, String, i64)>>> + Send;

    /// Returns `EpochKey = (bucket, origin_node_id, origin_epoch)` → sorted
    /// list of seqs present in storage. Used at startup to rebuild heads,
    /// pending_seqs, and max_known_seqs from the durable truth.
    fn sequences_by_origin_epoch(
        &self,
    ) -> impl Future<Output = Result<BTreeMap<EpochKey, Vec<u64>>>> + Send;

    /// Returns `(bucket, origin_node_id, origin_epoch, account)` tuples —
    /// every `(balance_key, epoch_key)` pair that contributes to at least
    /// one account's balance. Used at startup to rebuild
    /// `account_origin_epochs`.
    fn origin_account_epoch_mapping(
        &self,
    ) -> impl Future<Output = Result<Vec<(String, String, u32, String)>>> + Send;

    // ── Idempotency ──────────────────────────────────────────────────
    fn find_by_idempotency_key(
        &self,
        nonce: &str,
        bucket: &str,
        account: &str,
        amount: i64,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;

    // ── Holds ────────────────────────────────────────────────────────
    fn active_holds(&self, now_ms: u64) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn released_hold_refs(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

    // ── Checksum ─────────────────────────────────────────────────────
    fn checksum_data(&self) -> impl Future<Output = Result<String>> + Send;

    // ── Node meta ────────────────────────────────────────────────────
    fn load_node_meta(
        &self,
        node_id: &str,
    ) -> impl Future<Output = Result<Option<NodeMeta>>> + Send;
    fn save_node_meta(&self, meta: &NodeMeta) -> impl Future<Output = Result<()>> + Send;

    // ── Per-bucket seq/epoch allocator (§13.1) ───────────────────────
    /// Load every `(bucket, node_id)` allocator row for this node.
    fn load_bucket_allocators(
        &self,
        node_id: &str,
    ) -> impl Future<Output = Result<Vec<BucketAllocatorRow>>> + Send;

    /// Called on node startup. For every existing allocator row owned by
    /// `node_id`, set `needs_bump = TRUE`. The next write to each bucket
    /// will atomically bump the epoch and clear the flag.
    fn mark_bucket_allocators_pending(
        &self,
        node_id: &str,
    ) -> impl Future<Output = Result<usize>> + Send;

    /// Atomically bump `(bucket, node_id)`'s epoch and reset `next_seq=1`
    /// if `needs_bump = TRUE`; otherwise return current_epoch unchanged.
    /// If the row does not exist, insert it with epoch=1, seq=1,
    /// needs_bump=false. Returns the post-bump `current_epoch`.
    fn bump_bucket_epoch(
        &self,
        bucket: &str,
        node_id: &str,
    ) -> impl Future<Output = Result<u32>> + Send;

    /// Checkpoint the in-memory `next_seq` back to durable storage. Called
    /// periodically (on batch flush) so a crash can't lose allocated seqs.
    fn persist_bucket_next_seq(
        &self,
        bucket: &str,
        node_id: &str,
        next_seq: u64,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Recovery helper: derive the next unused seq for
    /// `(bucket, node_id, epoch)` from the events table. Used only when
    /// the allocator row is missing but events already exist (shouldn't
    /// happen in practice).
    fn derive_next_seq(
        &self,
        bucket: &str,
        node_id: &str,
        epoch: u32,
    ) -> impl Future<Output = Result<u64>> + Send;

    // ── Node registry ────────────────────────────────────────────────
    fn upsert_registry_entry(
        &self,
        entry: &NodeRegistryEntry,
    ) -> impl Future<Output = Result<()>> + Send;
    fn load_registry(&self) -> impl Future<Output = Result<Vec<NodeRegistryEntry>>> + Send;
    fn decommission_node(&self, node_id: &str) -> impl Future<Output = Result<()>> + Send;

    // ── Rolling digests (§8.3) ─────────────────────────────────────
    fn load_digests(
        &self,
    ) -> impl Future<Output = Result<BTreeMap<EpochKey, (u64, [u8; 32])>>> + Send;
    fn save_digest(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        head: u64,
        digest: &[u8; 32],
    ) -> impl Future<Output = Result<()>> + Send;

    // ── Matview ──────────────────────────────────────────────────────
    fn refresh_balance_summary(&self) -> impl Future<Output = Result<()>> + Send;
    fn read_balance_summary(
        &self,
    ) -> impl Future<Output = Result<Vec<(String, String, i64)>>> + Send;

    // ── Migrations ───────────────────────────────────────────────────
    fn run_migrations(&self) -> impl Future<Output = Result<()>> + Send;
}
