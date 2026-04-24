use serde::{Deserialize, Serialize};

/// A ledger event — the atomic unit of state in shardd. Every event is
/// immutable and globally identified by
/// `(bucket, origin_node_id, origin_epoch, origin_seq)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub origin_node_id: String,
    #[serde(default = "default_epoch")]
    pub origin_epoch: u32,
    pub origin_seq: u64,
    pub created_at_unix_ms: u64,
    #[serde(default = "default_type", rename = "type")]
    pub event_type: String,
    pub bucket: String,
    pub account: String,
    pub amount: i64,
    #[serde(default)]
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
fn default_type() -> String {
    "standard".to_string()
}

/// Optional knobs for [`Client::create_event`](crate::Client::create_event).
/// Leave everything unset for the common case of "charge or credit an account".
#[derive(Debug, Clone, Default)]
pub struct CreateEventOptions {
    /// Human-readable description stored on the event.
    pub note: Option<String>,
    /// Supply your own dedup key. If omitted, the SDK generates a UUID v4.
    /// Reuse the same nonce across retries of the same logical operation —
    /// the server will return the original event instead of double-charging.
    pub idempotency_nonce: Option<String>,
    /// Allow the debit to drive the balance this far negative (in credit
    /// units). Default 0 = overdraft rejected.
    pub max_overdraft: Option<u64>,
    /// Wait for at least this many cross-region acks before returning.
    pub min_acks: Option<u32>,
    /// Cap the ack wait at this many milliseconds.
    pub ack_timeout_ms: Option<u64>,
    /// Reserve this many additional credit units beyond the debit. Used
    /// for pre-auth / hold flows.
    pub hold_amount: Option<u64>,
    /// Unix-ms timestamp at which the hold auto-releases.
    pub hold_expires_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CreateEventBody<'a> {
    pub bucket: &'a str,
    pub account: &'a str,
    pub amount: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'a str>,
    pub idempotency_nonce: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_overdraft: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_acks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_amount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_expires_at_unix_ms: Option<u64>,
}

/// Result of a successful [`Client::create_event`](crate::Client::create_event).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateEventResult {
    /// The event as it lives on the ledger. On an idempotent retry this is
    /// the original event, not a new one.
    pub event: Event,
    /// Post-event balance on `(bucket, account)`.
    pub balance: i64,
    /// Balance minus any active hold total on `(bucket, account)`.
    pub available_balance: i64,
    /// `true` if a prior event with the same nonce already existed — the
    /// write was a no-op. `false` if this call created a fresh event.
    #[serde(default)]
    pub deduplicated: bool,
    /// Cross-region acknowledgement summary.
    #[serde(default)]
    pub acks: AckInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AckInfo {
    #[serde(default)]
    pub requested: u32,
    #[serde(default)]
    pub received: u32,
    #[serde(default)]
    pub timeout: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventList {
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Balances {
    pub accounts: Vec<AccountBalance>,
    #[serde(default)]
    pub total_balance: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountBalance {
    pub bucket: String,
    pub account: String,
    pub balance: i64,
    #[serde(default)]
    pub available_balance: i64,
    #[serde(default)]
    pub active_hold_total: u64,
    #[serde(default)]
    pub event_count: u64,
}

/// Collapsed snapshot for a single `(bucket, account)`, returned by
/// [`Client::get_account`](crate::Client::get_account).
#[derive(Debug, Clone, Deserialize)]
pub struct AccountDetail {
    pub bucket: String,
    pub account: String,
    pub balance: i64,
    #[serde(default)]
    pub available_balance: i64,
    #[serde(default)]
    pub active_hold_total: u64,
    #[serde(default)]
    pub event_count: u64,
}

/// One row of [`Client::edges`](crate::Client::edges). Matches the gateway's
/// `/gateway/edges` response shape.
#[derive(Debug, Clone, Deserialize)]
pub struct EdgeInfo {
    pub edge_id: String,
    pub region: String,
    pub base_url: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub reachable: bool,
    #[serde(default)]
    pub sync_gap: Option<u64>,
    #[serde(default)]
    pub overloaded: Option<bool>,
    #[serde(default)]
    pub healthy_nodes: usize,
    #[serde(default)]
    pub discovered_nodes: usize,
    #[serde(default)]
    pub best_node_rtt_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EdgeDirectoryResponse {
    pub edges: Vec<EdgeInfo>,
}

/// Health snapshot returned by [`Client::health`](crate::Client::health).
#[derive(Debug, Clone, Deserialize)]
pub struct EdgeHealth {
    #[serde(default)]
    pub edge_id: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub discovered_nodes: usize,
    #[serde(default)]
    pub healthy_nodes: usize,
    #[serde(default)]
    pub best_node_rtt_ms: Option<u64>,
    #[serde(default)]
    pub sync_gap: Option<u64>,
    #[serde(default)]
    pub overloaded: Option<bool>,
    #[serde(default)]
    pub auth_enabled: bool,
}
