//! shardd-types v2 — All domain types per protocol.md v1.7.
//!
//! This crate defines the Event struct (§2.1), all API request/response
//! types (§7), node registry entry (§14), and helper types used across
//! the shardd workspace.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// ── Key type aliases ─────────────────────────────────────────────────

/// Globally unique dedup key: (bucket, origin_node_id, origin_epoch, origin_seq).
///
/// Each `(bucket, origin_node_id)` pair maintains its own independent epoch
/// and sequence space — a crash or purge in one bucket cannot desync
/// another. Multi-writer safety still comes from including `origin_node_id`
/// in the tuple: two nodes writing to the same bucket allocate from their
/// own per-(bucket, node) counters and produce distinct identities.
pub type OriginKey = (String, String, u32, u64);

/// Identifies a specific `(bucket, origin_node_id, origin_epoch)` — the
/// granularity at which heads, pending seqs, and rolling digests track.
pub type EpochKey = (String, String, u32);

/// Identifies an account: (bucket, account).
pub type BalanceKey = (String, String);

// ── Reserved bucket names ─────────────────────────────────────────────

/// The special bucket that holds the cluster-wide meta log. `BucketDelete`
/// events live here; the bucket itself is never deleted. Replicates via
/// the exact same per-bucket protocol as any user bucket; see §3.5.
pub const META_BUCKET: &str = "__meta__";

/// Prefix for auto-generated billing buckets (`__billing__<user_id>`).
pub const BILLING_BUCKET_PREFIX: &str = "__billing__";

/// Returns true for names clients must not write to directly — reserved
/// for internal use (meta log, billing, etc.).
pub fn is_reserved_bucket_name(name: &str) -> bool {
    name == META_BUCKET || name.starts_with(BILLING_BUCKET_PREFIX)
}

// ── Event type enum (§2.2) ───────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    #[default]
    Standard,
    ReservationCreate,
    Void,
    HoldRelease,
    /// Meta-log event that instructs every node to atomically drop all
    /// state for the bucket named in `event.account`. Only valid when
    /// `event.bucket == META_BUCKET`; any other location is a protocol
    /// violation. See §3.5.
    BucketDelete,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "standard"),
            Self::ReservationCreate => write!(f, "reservation_create"),
            Self::Void => write!(f, "void"),
            Self::HoldRelease => write!(f, "hold_release"),
            Self::BucketDelete => write!(f, "bucket_delete"),
        }
    }
}

// ── Event (§2.1) ─────────────────────────────────────────────────────

/// Immutable ledger entry. The atomic unit of data.
/// Dedup key: (bucket, origin_node_id, origin_epoch, origin_seq).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub origin_node_id: String,
    #[serde(default = "default_epoch")]
    pub origin_epoch: u32,
    pub origin_seq: u64,
    pub created_at_unix_ms: u64,
    #[serde(default)]
    pub r#type: EventType,
    #[serde(default = "default_bucket")]
    pub bucket: String,
    #[serde(default = "default_account")]
    pub account: String,
    pub amount: i64,
    pub note: Option<String>,
    pub idempotency_nonce: String,
    #[serde(default)]
    pub void_ref: Option<String>,
    #[serde(default)]
    pub hold_amount: u64,
    #[serde(default)]
    pub hold_expires_at_unix_ms: u64,
}

fn default_epoch() -> u32 {
    1
}
fn default_bucket() -> String {
    "default".to_string()
}
fn default_account() -> String {
    "main".to_string()
}

impl Event {
    /// Generate a new random UUID v4 event_id.
    pub fn generate_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Current time in milliseconds since Unix epoch.
    pub fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// The globally unique dedup key (bucket, origin_node_id, origin_epoch, origin_seq).
    pub fn origin_key(&self) -> OriginKey {
        (
            self.bucket.clone(),
            self.origin_node_id.clone(),
            self.origin_epoch,
            self.origin_seq,
        )
    }

    /// The epoch key for head tracking (bucket, origin_node_id, origin_epoch).
    pub fn epoch_key(&self) -> EpochKey {
        (
            self.bucket.clone(),
            self.origin_node_id.clone(),
            self.origin_epoch,
        )
    }

    /// The balance key for account lookups.
    pub fn balance_key(&self) -> BalanceKey {
        (self.bucket.clone(), self.account.clone())
    }

    /// Canonical string for checksum computation (§8.2).
    /// Excludes `note` (cosmetic). Nullable fields use empty string when null.
    pub fn canonical(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            self.origin_node_id,
            self.origin_epoch,
            self.origin_seq,
            self.event_id,
            self.r#type,
            self.bucket,
            self.account,
            self.amount,
            self.void_ref.as_deref().unwrap_or(""),
            self.idempotency_nonce,
            self.hold_amount,
            self.hold_expires_at_unix_ms,
        )
    }

    /// Idempotency composite key (§10.1): (nonce, bucket, account, amount).
    /// Always populated — every event carries a nonce.
    pub fn idempotency_key(&self) -> (String, String, String, i64) {
        (
            self.idempotency_nonce.clone(),
            self.bucket.clone(),
            self.account.clone(),
            self.amount,
        )
    }

    /// Whether this event contributes an active reservation.
    /// Accepts legacy standard-debit holds for backward compatibility.
    pub fn has_hold(&self) -> bool {
        match self.r#type {
            EventType::ReservationCreate => {
                self.hold_amount > 0 && self.hold_expires_at_unix_ms > 0
            }
            EventType::Standard => {
                self.amount < 0 && self.hold_amount > 0 && self.hold_expires_at_unix_ms > 0
            }
            EventType::Void | EventType::HoldRelease | EventType::BucketDelete => false,
        }
    }

    /// If this event is a `BucketDelete` meta event, return the target
    /// bucket being deleted. The target is carried in the `account`
    /// field to avoid a schema extension — see §3.5 of the protocol.
    /// Returns `None` for any other event shape, including a
    /// `BucketDelete` whose `bucket` isn't `META_BUCKET` (protocol
    /// violation — treat as a no-op, don't cascade).
    pub fn meta_target_bucket(&self) -> Option<&str> {
        if self.r#type == EventType::BucketDelete && self.bucket == META_BUCKET {
            Some(&self.account)
        } else {
            None
        }
    }
}

/// Compute SHA-256 checksum over a sorted list of events (§8.2).
pub fn compute_checksum(events: &mut [(String, String, u32, u64, String)]) -> String {
    // events: Vec of (bucket, origin_node_id, origin_epoch, origin_seq, canonical_string)
    events.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });

    let mut hasher = Sha256::new();
    for (i, (_, _, _, _, canonical)) in events.iter().enumerate() {
        if i > 0 {
            hasher.update(b"\n");
        }
        hasher.update(canonical.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

// ── Node meta (§6.1) ─────────────────────────────────────────────────
//
// Since seq/epoch are now per-`(bucket, origin_node_id)` (see `OriginKey`),
// node-wide `current_epoch` and `next_seq` no longer exist. Bucket-level
// allocator state lives in the `bucket_seq_allocator` table.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMeta {
    pub node_id: String,
    pub host: String,
    pub port: u16,
}

// ── Node registry entry (§14.1) ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Active,
    Suspect,
    Unreachable,
    Decommissioned,
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Suspect => write!(f, "suspect"),
            Self::Unreachable => write!(f, "unreachable"),
            Self::Decommissioned => write!(f, "decommissioned"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegistryEntry {
    pub node_id: String,
    pub addr: String,
    pub first_seen_at_unix_ms: u64,
    pub last_seen_at_unix_ms: u64,
    pub status: NodeStatus,
}

impl NodeRegistryEntry {
    /// CRDT merge per §14.3. Commutative, associative, idempotent.
    /// `decommissioned` is a monotonic tombstone.
    pub fn merge(&self, other: &Self) -> Self {
        assert_eq!(
            self.node_id, other.node_id,
            "cannot merge entries with different node_ids"
        );

        let first_seen = self.first_seen_at_unix_ms.min(other.first_seen_at_unix_ms);
        let last_seen = self.last_seen_at_unix_ms.max(other.last_seen_at_unix_ms);

        let latest = if other.last_seen_at_unix_ms > self.last_seen_at_unix_ms {
            other
        } else {
            self
        };

        let status = if self.status == NodeStatus::Decommissioned
            || other.status == NodeStatus::Decommissioned
        {
            NodeStatus::Decommissioned
        } else {
            latest.status.clone()
        };

        Self {
            node_id: self.node_id.clone(),
            addr: latest_non_empty_addr(self, other),
            first_seen_at_unix_ms: first_seen,
            last_seen_at_unix_ms: last_seen,
            status,
        }
    }
}

fn latest_non_empty_addr(current: &NodeRegistryEntry, incoming: &NodeRegistryEntry) -> String {
    if incoming.last_seen_at_unix_ms > current.last_seen_at_unix_ms {
        if incoming.addr.is_empty() {
            current.addr.clone()
        } else {
            incoming.addr.clone()
        }
    } else if current.last_seen_at_unix_ms > incoming.last_seen_at_unix_ms {
        if current.addr.is_empty() {
            incoming.addr.clone()
        } else {
            current.addr.clone()
        }
    } else if !incoming.addr.is_empty() {
        incoming.addr.clone()
    } else {
        current.addr.clone()
    }
}

// ── API request/response types (§7) ──────────────────────────────────

pub const MAX_EVENT_NOTE_CHARS: usize = 4096;

pub fn validate_event_note(note: Option<&str>) -> Result<(), String> {
    let Some(note) = note else {
        return Ok(());
    };

    let note_len = note.chars().count();
    if note_len > MAX_EVENT_NOTE_CHARS {
        return Err(format!(
            "note must be at most {MAX_EVENT_NOTE_CHARS} characters"
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckInfo {
    pub received: u32,
    pub requested: u32,
    pub timeout: bool,
}

impl AckInfo {
    pub fn fire_and_forget() -> Self {
        Self {
            received: 0,
            requested: 0,
            timeout: false,
        }
    }
}

// §7.1 POST /events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEventRequest {
    pub bucket: String,
    pub account: String,
    pub amount: i64,
    #[serde(default)]
    pub note: Option<String>,
    pub idempotency_nonce: String,
    #[serde(default)]
    pub max_overdraft: Option<u64>,
    #[serde(default)]
    pub min_acks: Option<u32>,
    #[serde(default)]
    pub ack_timeout_ms: Option<u64>,
    /// Internal-only: set by the gateway's `/internal/billing/events`
    /// route to write into reserved buckets (e.g. `__billing__<user>`).
    /// Public RPC clients can never set this — the gateway's external
    /// routes don't deserialize it from the wire payload.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub allow_reserved_bucket: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEventResponse {
    pub event: Event,
    pub balance: i64,
    pub available_balance: i64,
    pub deduplicated: bool,
    pub acks: AckInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsufficientFundsError {
    pub error: String,
    pub balance: i64,
    pub available_balance: i64,
    pub projected_available_balance: i64,
    pub limit: i64,
}

// §7.1 GET /health
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub observed_at_unix_ms: u64,
    pub node_id: String,
    pub addr: String,
    pub ready: bool,
    pub peer_count: usize,
    pub known_nodes: usize,
    /// Max `(max_known_seq − contiguous_head)` across every `(bucket,
    /// origin_node_id, origin_epoch)` entry the node tracks.
    pub sync_gap: u64,
    /// Per-bucket view of the same gap. Key = bucket name. Value = the
    /// max `(max_known − head)` across all `(origin, epoch)` entries
    /// within that bucket. Lets Grafana (and `shardd_node_sync_gap_per_bucket`)
    /// pinpoint which bucket is responsible for a cluster-wide gap spike
    /// without a separate RPC.
    #[serde(default)]
    pub sync_gap_per_bucket: BTreeMap<String, u64>,
    pub inflight_requests: u64,
    pub completed_requests: u64,
    pub failed_requests: u64,
    pub overloaded: bool,
    pub event_count: usize,
    pub total_balance: i64,
}

// Public edge gateway health / discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicEdgeHealthResponse {
    pub observed_at_unix_ms: u64,
    #[serde(default)]
    pub edge_id: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    pub ready: bool,
    pub discovered_nodes: usize,
    pub healthy_nodes: usize,
    #[serde(default)]
    pub best_node_rtt_ms: Option<u64>,
    #[serde(default)]
    pub sync_gap: Option<u64>,
    #[serde(default)]
    pub overloaded: Option<bool>,
    pub auth_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicEdgeSummary {
    pub edge_id: String,
    pub region: String,
    pub base_url: String,
    pub health_url: String,
    pub reachable: bool,
    pub ready: bool,
    #[serde(default)]
    pub observed_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub discovered_nodes: Option<usize>,
    #[serde(default)]
    pub healthy_nodes: Option<usize>,
    #[serde(default)]
    pub best_node_rtt_ms: Option<u64>,
    #[serde(default)]
    pub sync_gap: Option<u64>,
    #[serde(default)]
    pub overloaded: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicEdgeDirectoryResponse {
    pub observed_at_unix_ms: u64,
    pub edges: Vec<PublicEdgeSummary>,
}

// §7.1 GET /state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateResponse {
    pub node_id: String,
    pub addr: String,
    pub ready: bool,
    pub event_count: usize,
    pub total_balance: i64,
    /// Per-`(bucket, origin_node_id, origin_epoch)` contiguous head. Key is
    /// encoded as `"{bucket}\t{origin_node_id}:{origin_epoch}"` so the
    /// BTreeMap sorts deterministically first by bucket, then by origin.
    pub contiguous_heads: BTreeMap<String, u64>,
    pub checksum: String,
}

// §7.1 GET /balances
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBalance {
    pub bucket: String,
    pub account: String,
    pub balance: i64,
    pub available_balance: i64,
    pub active_hold_total: i64,
    #[serde(default)]
    pub reserved_by_origin: BTreeMap<String, OriginReservationSummary>,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalancesResponse {
    pub accounts: Vec<AccountBalance>,
    pub total_balance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsResponse {
    pub events: Vec<Event>,
}

// §7.1 GET /persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceStats {
    pub buffered: usize,
    pub unpersisted: usize,
    pub oldest_unpersisted_age_ms: Option<u64>,
}

// §7.1 GET /debug/origin/:id
/// Rolling prefix digest info per origin-epoch (§8.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestInfo {
    pub head: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugOriginResponse {
    pub origin_node_id: String,
    pub epochs: BTreeMap<u32, DebugEpochInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugEpochInfo {
    pub contiguous_head: u64,
    pub present_seqs: Vec<u64>,
    pub min_seq: Option<u64>,
    pub max_seq: Option<u64>,
    pub count: usize,
}

// §7.1 GET /collapsed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapsedBalance {
    pub balance: i64,
    pub available_balance: i64,
    pub status: String,
    #[serde(default)]
    pub reserved_by_origin: BTreeMap<String, OriginReservationSummary>,
    pub contributing_origins: BTreeMap<String, OriginProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginProgress {
    pub head: u64,
    pub max_known: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginReservationSummary {
    pub reserved_amount: u64,
    pub reservation_count: usize,
    pub next_expiry_unix_ms: Option<u64>,
    pub latest_expiry_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "body")]
pub enum NodeRpcRequest {
    CreateEvent(CreateEventRequest),
    Health,
    State,
    Events,
    Heads,
    Balances,
    Collapsed,
    CollapsedAccount {
        bucket: String,
        account: String,
    },
    Persistence,
    Digests,
    DebugOrigin {
        origin_id: String,
    },
    Registry,
    /// §3.5: emit a `BucketDelete` meta event for `bucket`. Admin/owner
    /// gated at the dashboard; the node trusts the machine-auth layer.
    /// Never reachable through the public edge RPC path.
    DeleteBucket {
        bucket: String,
        reason: Option<String>,
    },
    /// Paginated + filtered event listing for the admin viewer. Only
    /// reachable via the gateway's machine-auth `/internal/admin/events`
    /// route; never exposed to client-facing RPC.
    EventsFilter(EventsFilterRequest),
    /// Snapshot of the node's in-memory `deleted_buckets` map — names of
    /// every bucket that has been hard-purged cluster-wide, with the
    /// meta-event timestamp. Machine-auth-gated at the gateway.
    DeletedBuckets,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "body")]
pub enum NodeRpcResponse {
    CreateEvent(CreateEventResponse),
    Health(HealthResponse),
    State(StateResponse),
    Events(EventsResponse),
    Heads(BTreeMap<String, u64>),
    Balances(BalancesResponse),
    Collapsed(BTreeMap<String, CollapsedBalance>),
    CollapsedAccount(CollapsedBalance),
    Persistence(PersistenceStats),
    Digests(BTreeMap<String, DigestInfo>),
    DebugOrigin(DebugOriginResponse),
    Registry(Vec<NodeRegistryEntry>),
    /// §3.5: the `BucketDelete` meta event the node emitted.
    DeleteBucket(Event),
    /// One page of filtered events plus the heads/max_known snapshot
    /// the answering node had at response time.
    EventsFilter(EventsFilterResponse),
    /// List of hard-purged buckets with the deletion timestamp.
    DeletedBuckets(Vec<DeletedBucketEntry>),
}

/// A single entry in the node's `deleted_buckets` projection. `name` is
/// the cluster-global bucket name; `deleted_at_unix_ms` is the
/// `created_at_unix_ms` of the `BucketDelete` meta event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletedBucketEntry {
    pub name: String,
    pub deleted_at_unix_ms: u64,
}

/// Filter for paginated admin events listing. All fields optional; `None`
/// means "any". `event_type` matches against the wire string exactly
/// ("standard" | "void" | "hold_release" | "reservation_create" |
/// "bucket_delete"). `search` is a case-insensitive substring match
/// against `note` and `event_id`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventsFilterRequest {
    #[serde(default)]
    pub bucket: Option<String>,
    /// Literal bucket-name prefix. Used by the gateway's per-user events
    /// endpoint to scope a cluster-wide filter to one user's namespace
    /// (`user_{uuid}__bucket_`) without a full `bucket` match.
    #[serde(default)]
    pub bucket_prefix: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub since_unix_ms: Option<u64>,
    #[serde(default)]
    pub until_unix_ms: Option<u64>,
    #[serde(default)]
    pub search: Option<String>,
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

/// One page of events plus the answering node's current head /
/// max_known snapshot, keyed as `"{bucket}\t{origin}:{epoch}"` (same
/// encoding as `StateResponse::contiguous_heads`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsFilterResponse {
    pub events: Vec<Event>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
    pub heads: BTreeMap<String, u64>,
    pub max_known_seqs: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeRpcErrorCode {
    ServiceUnavailable,
    InsufficientFunds,
    InvalidInput,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRpcError {
    pub code: NodeRpcErrorCode,
    pub message: String,
    #[serde(default)]
    pub insufficient_funds: Option<InsufficientFundsError>,
}

impl NodeRpcError {
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            code: NodeRpcErrorCode::ServiceUnavailable,
            message: message.into(),
            insufficient_funds: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: NodeRpcErrorCode::Internal,
            message: message.into(),
            insufficient_funds: None,
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: NodeRpcErrorCode::InvalidInput,
            message: message.into(),
            insufficient_funds: None,
        }
    }

    pub fn insufficient_funds(error: InsufficientFundsError) -> Self {
        Self {
            code: NodeRpcErrorCode::InsufficientFunds,
            message: error.error.clone(),
            insufficient_funds: Some(error),
        }
    }
}

pub type NodeRpcResult = Result<NodeRpcResponse, NodeRpcError>;

// ── Helper: deterministic idempotency winner (§10.4) ─────────────────

/// Determine the idempotency winner between two events.
/// Lower created_at_unix_ms wins. If equal, lower event_id (lexicographic) wins.
pub fn idempotency_winner<'a>(a: &'a Event, b: &'a Event) -> (&'a Event, &'a Event) {
    if a.created_at_unix_ms < b.created_at_unix_ms {
        (a, b)
    } else if b.created_at_unix_ms < a.created_at_unix_ms {
        (b, a)
    } else if a.event_id < b.event_id {
        (a, b)
    } else {
        (b, a)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_has_all_14_fields() {
        let event = Event {
            event_id: "test-id".into(),
            origin_node_id: "node-1".into(),
            origin_epoch: 3,
            origin_seq: 42,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "default".into(),
            account: "alice".into(),
            amount: -100,
            note: Some("charge".into()),
            idempotency_nonce: "completion:abc".into(),
            void_ref: None,
            hold_amount: 1000,
            hold_expires_at_unix_ms: 2000,
        };

        assert_eq!(
            event.origin_key(),
            ("default".into(), "node-1".into(), 3, 42)
        );
        assert_eq!(event.epoch_key(), ("default".into(), "node-1".into(), 3));
        assert_eq!(event.balance_key(), ("default".into(), "alice".into()));
        assert!(event.has_hold());
    }

    #[test]
    fn event_type_serialization() {
        assert_eq!(
            serde_json::to_string(&EventType::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::ReservationCreate).unwrap(),
            "\"reservation_create\""
        );
        assert_eq!(serde_json::to_string(&EventType::Void).unwrap(), "\"void\"");
        assert_eq!(
            serde_json::to_string(&EventType::HoldRelease).unwrap(),
            "\"hold_release\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::BucketDelete).unwrap(),
            "\"bucket_delete\""
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"void\"").unwrap(),
            EventType::Void
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"bucket_delete\"").unwrap(),
            EventType::BucketDelete
        );
    }

    #[test]
    fn meta_target_bucket_matches_only_valid_shape() {
        let delete_meta = Event {
            event_id: "m1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::BucketDelete,
            bucket: META_BUCKET.into(),
            account: "orders".into(),
            amount: 0,
            note: Some("admin@example.com".into()),
            idempotency_nonce: "delete:orders".into(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };
        assert_eq!(delete_meta.meta_target_bucket(), Some("orders"));

        // Wrong bucket name: not a valid meta event.
        let not_in_meta = Event {
            bucket: "orders".into(),
            ..delete_meta.clone()
        };
        assert_eq!(not_in_meta.meta_target_bucket(), None);

        // Wrong type: not a delete.
        let wrong_type = Event {
            r#type: EventType::Standard,
            ..delete_meta.clone()
        };
        assert_eq!(wrong_type.meta_target_bucket(), None);
    }

    #[test]
    fn is_reserved_bucket_name_covers_meta_and_billing() {
        assert!(is_reserved_bucket_name(META_BUCKET));
        assert!(is_reserved_bucket_name("__billing__abc-123"));
        assert!(!is_reserved_bucket_name("orders"));
        assert!(!is_reserved_bucket_name("users"));
        assert!(!is_reserved_bucket_name("meta"));
        assert!(!is_reserved_bucket_name("__meta"));
    }

    #[test]
    fn event_json_roundtrip_with_defaults() {
        let json = r#"{
            "event_id": "abc",
            "origin_node_id": "n1",
            "origin_seq": 1,
            "created_at_unix_ms": 1000,
            "bucket": "b",
            "account": "a",
            "amount": 50,
            "idempotency_nonce": "nonce-xyz"
        }"#;

        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.origin_epoch, 1); // default
        assert_eq!(event.r#type, EventType::Standard); // default
        assert_eq!(event.idempotency_nonce, "nonce-xyz");
        assert_eq!(event.void_ref, None);
        assert_eq!(event.hold_amount, 0);
        assert_eq!(event.hold_expires_at_unix_ms, 0);
    }

    #[test]
    fn canonical_format_matches_spec() {
        let event = Event {
            event_id: "eid".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 2,
            origin_seq: 5,
            created_at_unix_ms: 1000,
            r#type: EventType::Void,
            bucket: "b".into(),
            account: "a".into(),
            amount: 50,
            note: Some("should be excluded".into()),
            idempotency_nonce: "nonce1".into(),
            void_ref: Some("ref1".into()),
            hold_amount: 100,
            hold_expires_at_unix_ms: 9999,
        };

        assert_eq!(
            event.canonical(),
            "n1:2:5:eid:void:b:a:50:ref1:nonce1:100:9999"
        );
    }

    #[test]
    fn canonical_nullable_fields_empty_string() {
        let event = Event {
            event_id: "eid".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 100,
            note: None,
            idempotency_nonce: "n".into(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        assert_eq!(event.canonical(), "n1:1:1:eid:standard:b:a:100::n:0:0");
    }

    #[test]
    fn ack_info_fire_and_forget() {
        let ack = AckInfo::fire_and_forget();
        assert_eq!(ack.received, 0);
        assert_eq!(ack.requested, 0);
        assert!(!ack.timeout);
    }

    #[test]
    fn idempotency_key_is_always_populated() {
        let event = Event {
            event_id: "e1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -50,
            note: None,
            idempotency_nonce: "completion:abc".into(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        let key = event.idempotency_key();
        assert_eq!(key, ("completion:abc".into(), "b".into(), "a".into(), -50));
    }

    #[test]
    fn idempotency_winner_by_timestamp() {
        let a = Event {
            event_id: "zzz".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -50,
            note: None,
            idempotency_nonce: "nonce".into(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        let b = Event {
            created_at_unix_ms: 2000,
            event_id: "aaa".into(),
            origin_node_id: "n2".into(),
            ..a.clone()
        };

        let (winner, loser) = idempotency_winner(&a, &b);
        assert_eq!(winner.event_id, "zzz"); // lower timestamp wins
        assert_eq!(loser.event_id, "aaa");
    }

    #[test]
    fn idempotency_winner_by_event_id_tiebreak() {
        let a = Event {
            event_id: "bbb".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -50,
            note: None,
            idempotency_nonce: "nonce".into(),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        let b = Event {
            event_id: "aaa".into(),
            origin_node_id: "n2".into(),
            ..a.clone()
        };

        let (winner, _loser) = idempotency_winner(&a, &b);
        assert_eq!(winner.event_id, "aaa"); // lower event_id wins on tie
    }

    #[test]
    fn registry_merge_preserves_last_known_addr_when_newer_update_has_none() {
        let active = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "10.0.0.1:4001".into(),
            first_seen_at_unix_ms: 100,
            last_seen_at_unix_ms: 200,
            status: NodeStatus::Active,
        };
        let unreachable = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: String::new(),
            first_seen_at_unix_ms: 100,
            last_seen_at_unix_ms: 300,
            status: NodeStatus::Unreachable,
        };

        let merged = active.merge(&unreachable);
        assert_eq!(merged.addr, "10.0.0.1:4001");
        assert_eq!(merged.status, NodeStatus::Unreachable);
        assert_eq!(merged.last_seen_at_unix_ms, 300);
    }

    #[test]
    fn registry_crdt_merge_basic() {
        let local = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "host1:3001".into(),
            first_seen_at_unix_ms: 1000,
            last_seen_at_unix_ms: 5000,
            status: NodeStatus::Active,
        };

        let remote = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "host1:3002".into(),
            first_seen_at_unix_ms: 2000,
            last_seen_at_unix_ms: 6000,
            status: NodeStatus::Suspect,
        };

        let merged = local.merge(&remote);
        assert_eq!(merged.first_seen_at_unix_ms, 1000); // MIN
        assert_eq!(merged.last_seen_at_unix_ms, 6000); // MAX
        assert_eq!(merged.addr, "host1:3002"); // latest wins
        assert_eq!(merged.status, NodeStatus::Suspect); // latest wins
    }

    #[test]
    fn registry_crdt_merge_decommissioned_tombstone() {
        let local = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "host1:3001".into(),
            first_seen_at_unix_ms: 1000,
            last_seen_at_unix_ms: 9000, // newer timestamp
            status: NodeStatus::Active,
        };

        let remote = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "host1:3002".into(),
            first_seen_at_unix_ms: 2000,
            last_seen_at_unix_ms: 5000, // older timestamp
            status: NodeStatus::Decommissioned,
        };

        let merged = local.merge(&remote);
        // Decommissioned wins regardless of timestamp
        assert_eq!(merged.status, NodeStatus::Decommissioned);
        // But addr still follows latest timestamp
        assert_eq!(merged.addr, "host1:3001");
    }

    #[test]
    fn registry_crdt_merge_is_commutative() {
        let a = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "a:1".into(),
            first_seen_at_unix_ms: 100,
            last_seen_at_unix_ms: 500,
            status: NodeStatus::Active,
        };

        let b = NodeRegistryEntry {
            node_id: "n1".into(),
            addr: "b:2".into(),
            first_seen_at_unix_ms: 200,
            last_seen_at_unix_ms: 600,
            status: NodeStatus::Unreachable,
        };

        let ab = a.merge(&b);
        let ba = b.merge(&a);
        assert_eq!(ab.addr, ba.addr);
        assert_eq!(ab.status, ba.status);
        assert_eq!(ab.first_seen_at_unix_ms, ba.first_seen_at_unix_ms);
        assert_eq!(ab.last_seen_at_unix_ms, ba.last_seen_at_unix_ms);
    }

    #[test]
    fn checksum_deterministic() {
        let mut data = vec![
            (
                "b".into(),
                "n1".into(),
                1u32,
                1u64,
                "n1:1:1:e1:standard:b:a:100:::0:0".into(),
            ),
            (
                "b".into(),
                "n1".into(),
                1,
                2,
                "n1:1:2:e2:standard:b:a:50:::0:0".into(),
            ),
        ];

        let c1 = compute_checksum(&mut data.clone());
        let c2 = compute_checksum(&mut data);
        assert_eq!(c1, c2);
        assert_eq!(c1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn has_hold_for_reservation_create_and_legacy_standard_debit() {
        let reservation = Event {
            event_id: "e1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::ReservationCreate,
            bucket: "b".into(),
            account: "a".into(),
            amount: 0,
            note: None,
            idempotency_nonce: "reserve:nonce".into(),
            void_ref: None,
            hold_amount: 500,
            hold_expires_at_unix_ms: 9999,
        };
        assert!(reservation.has_hold());

        let legacy_debit_with_hold = Event {
            amount: -100,
            r#type: EventType::Standard,
            ..reservation.clone()
        };
        assert!(legacy_debit_with_hold.has_hold());

        // Standard charge: NOT a reservation
        let credit = Event {
            amount: 100,
            r#type: EventType::Standard,
            ..reservation.clone()
        };
        assert!(!credit.has_hold());

        // Void event: NOT a hold
        let void_event = Event {
            r#type: EventType::Void,
            ..reservation.clone()
        };
        assert!(!void_event.has_hold());

        // No hold metadata: NOT a hold
        let no_hold = Event {
            hold_amount: 0,
            ..reservation.clone()
        };
        assert!(!no_hold.has_hold());
    }

    #[test]
    fn event_note_limit_counts_characters() {
        let note = "界".repeat(MAX_EVENT_NOTE_CHARS);
        assert!(validate_event_note(Some(&note)).is_ok());
        assert!(validate_event_note(Some(&(note + "界"))).is_err());
    }
}
