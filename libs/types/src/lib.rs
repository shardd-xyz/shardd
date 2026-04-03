//! shardd-types v2 — All domain types per protocol.md v1.7.
//!
//! This crate defines the Event struct (§2.1), all API request/response
//! types (§7), node registry entry (§14), and helper types used across
//! the shardd workspace.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// ── Key type aliases ─────────────────────────────────────────────────

/// Globally unique dedup key: (origin_node_id, origin_epoch, origin_seq).
pub type OriginKey = (String, u32, u64);

/// Identifies a specific epoch of a specific origin: (origin_node_id, origin_epoch).
pub type EpochKey = (String, u32);

/// Identifies an account: (bucket, account).
pub type BalanceKey = (String, String);

// ── Event type enum (§2.2) ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Standard,
    Void,
    HoldRelease,
}

impl Default for EventType {
    fn default() -> Self {
        Self::Standard
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "standard"),
            Self::Void => write!(f, "void"),
            Self::HoldRelease => write!(f, "hold_release"),
        }
    }
}

// ── Event (§2.1) ─────────────────────────────────────────────────────

/// Immutable ledger entry. The atomic unit of data.
/// Dedup key: (origin_node_id, origin_epoch, origin_seq).
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
    #[serde(default)]
    pub idempotency_nonce: Option<String>,
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

    /// The globally unique dedup key.
    pub fn origin_key(&self) -> OriginKey {
        (
            self.origin_node_id.clone(),
            self.origin_epoch,
            self.origin_seq,
        )
    }

    /// The epoch key for head tracking.
    pub fn epoch_key(&self) -> EpochKey {
        (self.origin_node_id.clone(), self.origin_epoch)
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
            self.idempotency_nonce.as_deref().unwrap_or(""),
            self.hold_amount,
            self.hold_expires_at_unix_ms,
        )
    }

    /// Idempotency composite key (§10.1): (nonce, bucket, account, amount).
    /// Returns None if idempotency_nonce is None.
    pub fn idempotency_key(&self) -> Option<(String, String, String, i64)> {
        self.idempotency_nonce.as_ref().map(|nonce| {
            (
                nonce.clone(),
                self.bucket.clone(),
                self.account.clone(),
                self.amount,
            )
        })
    }

    /// Whether this event has an active hold (debit standard event with hold metadata).
    pub fn has_hold(&self) -> bool {
        self.r#type == EventType::Standard
            && self.amount < 0
            && self.hold_amount > 0
            && self.hold_expires_at_unix_ms > 0
    }
}

/// Compute SHA-256 checksum over a sorted list of events (§8.2).
pub fn compute_checksum(events: &mut [(String, u32, u64, String)]) -> String {
    // events: Vec of (origin_node_id, origin_epoch, origin_seq, canonical_string)
    events.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut hasher = Sha256::new();
    for (i, (_, _, _, canonical)) in events.iter().enumerate() {
        if i > 0 {
            hasher.update(b"\n");
        }
        hasher.update(canonical.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

// ── Node meta (§6.1) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMeta {
    pub node_id: String,
    pub host: String,
    pub port: u16,
    pub current_epoch: u32,
    pub next_seq: u64,
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
        assert_eq!(self.node_id, other.node_id, "cannot merge entries with different node_ids");

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
            addr: latest.addr.clone(),
            first_seen_at_unix_ms: first_seen,
            last_seen_at_unix_ms: last_seen,
            status,
        }
    }
}

// ── API request/response types (§7) ──────────────────────────────────

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
    #[serde(default)]
    pub idempotency_nonce: Option<String>,
    #[serde(default)]
    pub max_overdraft: Option<u64>,
    #[serde(default)]
    pub min_acks: Option<u32>,
    #[serde(default)]
    pub ack_timeout_ms: Option<u64>,
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

// §7.2 POST /events/replicate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicateResponse {
    pub status: String,
    pub inserted: bool,
}

// §7.2 POST /events/range
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeRequest {
    pub origin_node_id: String,
    pub origin_epoch: u32,
    pub from_seq: u64,
    pub to_seq: u64,
}

// §7.2 POST /join
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub node_id: String,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub node_id: String,
    pub addr: String,
    pub registry: Vec<NodeRegistryEntry>,
    /// Heads keyed by "{origin_node_id}:{origin_epoch}" → contiguous_head.
    pub heads: BTreeMap<String, u64>,
}

// §7.1 GET /health
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub node_id: String,
    pub addr: String,
    pub current_epoch: u32,
    pub ready: bool,
    pub peer_count: usize,
    pub event_count: usize,
    pub total_balance: i64,
}

// §7.1 GET /state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateResponse {
    pub node_id: String,
    pub addr: String,
    pub current_epoch: u32,
    pub next_seq: u64,
    pub ready: bool,
    pub peers: Vec<String>,
    pub event_count: usize,
    pub total_balance: i64,
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
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalancesResponse {
    pub accounts: Vec<AccountBalance>,
    pub total_balance: i64,
}

// §7.1 GET /persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceStats {
    pub buffered: usize,
    pub unpersisted: usize,
    pub oldest_unpersisted_age_ms: Option<u64>,
}

// §7.1 GET /debug/origin/:id
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
    pub contributing_origins: BTreeMap<String, OriginProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginProgress {
    pub head: u64,
    pub max_known: u64,
}

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
            idempotency_nonce: Some("completion:abc".into()),
            void_ref: None,
            hold_amount: 1000,
            hold_expires_at_unix_ms: 2000,
        };

        assert_eq!(event.origin_key(), ("node-1".into(), 3, 42));
        assert_eq!(event.epoch_key(), ("node-1".into(), 3));
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
            serde_json::to_string(&EventType::Void).unwrap(),
            "\"void\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::HoldRelease).unwrap(),
            "\"hold_release\""
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"void\"").unwrap(),
            EventType::Void
        );
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
            "amount": 50
        }"#;

        let event: Event = serde_json::from_str(json).unwrap();
        assert_eq!(event.origin_epoch, 1); // default
        assert_eq!(event.r#type, EventType::Standard); // default
        assert_eq!(event.idempotency_nonce, None);
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
            idempotency_nonce: Some("nonce1".into()),
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
            idempotency_nonce: None,
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        assert_eq!(
            event.canonical(),
            "n1:1:1:eid:standard:b:a:100:::0:0"
        );
    }

    #[test]
    fn ack_info_fire_and_forget() {
        let ack = AckInfo::fire_and_forget();
        assert_eq!(ack.received, 0);
        assert_eq!(ack.requested, 0);
        assert!(!ack.timeout);
    }

    #[test]
    fn idempotency_key_with_nonce() {
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
            idempotency_nonce: Some("completion:abc".into()),
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        let key = event.idempotency_key().unwrap();
        assert_eq!(key, ("completion:abc".into(), "b".into(), "a".into(), -50));
    }

    #[test]
    fn idempotency_key_without_nonce() {
        let event = Event {
            event_id: "e1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: 100,
            note: None,
            idempotency_nonce: None,
            void_ref: None,
            hold_amount: 0,
            hold_expires_at_unix_ms: 0,
        };

        assert!(event.idempotency_key().is_none());
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
            idempotency_nonce: Some("nonce".into()),
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
            idempotency_nonce: Some("nonce".into()),
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
            ("n1".into(), 1u32, 1u64, "n1:1:1:e1:standard:b:a:100:::0:0".into()),
            ("n1".into(), 1, 2, "n1:1:2:e2:standard:b:a:50:::0:0".into()),
        ];

        let c1 = compute_checksum(&mut data.clone());
        let c2 = compute_checksum(&mut data);
        assert_eq!(c1, c2);
        assert_eq!(c1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn has_hold_only_for_debit_standard_with_metadata() {
        let debit_with_hold = Event {
            event_id: "e1".into(),
            origin_node_id: "n1".into(),
            origin_epoch: 1,
            origin_seq: 1,
            created_at_unix_ms: 1000,
            r#type: EventType::Standard,
            bucket: "b".into(),
            account: "a".into(),
            amount: -100,
            note: None,
            idempotency_nonce: None,
            void_ref: None,
            hold_amount: 500,
            hold_expires_at_unix_ms: 9999,
        };
        assert!(debit_with_hold.has_hold());

        // Credit with hold metadata: NOT a hold
        let credit = Event { amount: 100, ..debit_with_hold.clone() };
        assert!(!credit.has_hold());

        // Void event: NOT a hold
        let void_event = Event { r#type: EventType::Void, ..debit_with_hold.clone() };
        assert!(!void_event.has_hold());

        // No hold metadata: NOT a hold
        let no_hold = Event { hold_amount: 0, ..debit_with_hold.clone() };
        assert!(!no_hold.has_hold());
    }
}
