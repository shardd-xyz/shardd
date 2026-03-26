use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Domain types ──

/// A single event in the append-only log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub origin_node_id: String,
    pub origin_seq: u64,
    pub created_at_unix_ms: u64,
    pub amount: i64,
    pub note: Option<String>,
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

// ── API request / response types ──

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateEventRequest {
    pub amount: i64,
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateEventResponse {
    pub event: Event,
    pub event_count: usize,
    pub balance: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AddPeerRequest {
    pub addr: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinRequest {
    pub node_id: String,
    pub addr: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinResponse {
    pub node_id: String,
    pub addr: String,
    pub peers: Vec<String>,
    pub heads: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RangeRequest {
    pub origin_node_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub node_id: String,
    pub addr: String,
    pub peer_count: usize,
    pub event_count: usize,
    pub balance: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StateResponse {
    pub node_id: String,
    pub addr: String,
    pub next_seq: u64,
    pub peers: Vec<String>,
    pub event_count: usize,
    pub balance: i64,
    pub contiguous_heads: BTreeMap<String, u64>,
    pub checksum: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReplicateResponse {
    pub status: String,
    pub inserted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DebugOriginResponse {
    pub origin_node_id: String,
    pub contiguous_head: u64,
    pub present_seqs: Vec<u64>,
    pub min_seq: Option<u64>,
    pub max_seq: Option<u64>,
    pub count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncTriggerResponse {
    pub status: String,
    pub peers_contacted: usize,
    pub events_applied: usize,
}
