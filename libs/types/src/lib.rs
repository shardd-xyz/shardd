use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Domain types ──

/// A single event in the append-only log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub origin_node_id: String,
    pub origin_seq: u64,
    pub created_at_unix_ms: u64,
    #[serde(default = "default_bucket")]
    pub bucket: String,
    #[serde(default = "default_account")]
    pub account: String,
    pub amount: i64,
    pub note: Option<String>,
}

fn default_bucket() -> String {
    "default".to_string()
}

fn default_account() -> String {
    "main".to_string()
}

/// Persisted node identity and local sequence counter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMeta {
    pub node_id: String,
    pub host: String,
    pub port: u16,
    pub next_seq: u64,
}

/// Persisted peer list.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeersFile {
    pub peers: Vec<String>,
}

/// Balance key: (bucket, account).
pub type BalanceKey = (String, String);

// ── API request / response types ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateEventRequest {
    pub bucket: String,
    pub account: String,
    pub amount: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateEventResponse {
    pub event: Event,
    pub event_count: usize,
    pub balance: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddPeerRequest {
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub node_id: String,
    pub addr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinResponse {
    pub node_id: String,
    pub addr: String,
    pub peers: Vec<String>,
    pub heads: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeRequest {
    pub origin_node_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub node_id: String,
    pub addr: String,
    pub peer_count: usize,
    pub event_count: usize,
    pub total_balance: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateResponse {
    pub node_id: String,
    pub addr: String,
    pub next_seq: u64,
    pub peers: Vec<String>,
    pub event_count: usize,
    pub total_balance: i64,
    pub contiguous_heads: BTreeMap<String, u64>,
    pub checksum: String,
}

/// Per-bucket, per-account balance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountBalance {
    pub bucket: String,
    pub account: String,
    pub balance: i64,
    pub event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalancesResponse {
    pub accounts: Vec<AccountBalance>,
    pub total_balance: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplicateResponse {
    pub status: String,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebugOriginResponse {
    pub origin_node_id: String,
    pub contiguous_head: u64,
    pub present_seqs: Vec<u64>,
    pub min_seq: Option<u64>,
    pub max_seq: Option<u64>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncTriggerResponse {
    pub status: String,
    pub peers_contacted: usize,
    pub events_applied: usize,
}
