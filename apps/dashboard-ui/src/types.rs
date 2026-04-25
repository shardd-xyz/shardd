use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Session {
    pub id: String,
    pub email: String,
    pub is_admin: bool,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub generation: u64,
    pub tone: NoticeTone,
    pub title: String,
    pub message: String,
}

impl Notice {
    pub fn new(tone: NoticeTone, title: impl Into<String>, message: impl Into<String>) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NOTICE_GEN: AtomicU64 = AtomicU64::new(1);
        Self {
            generation: NOTICE_GEN.fetch_add(1, Ordering::Relaxed),
            tone,
            title: title.into(),
            message: message.into(),
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum NoticeTone {
    Success,
    Warning,
    Danger,
    Info,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlashKey {
    pub label: String,
    pub raw_key: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeveloperProfile {
    pub is_frozen: bool,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key_prefix: String,
    pub last_used_at: Option<String>,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ApiKeyScope {
    pub id: String,
    /// "bucket" (data plane) or "control" (dashboard control plane).
    /// Defaults to "bucket" so older API responses missing the field
    /// continue to render correctly.
    #[serde(default = "default_scope_resource_type")]
    pub resource_type: String,
    pub match_type: String,
    pub resource_value: Option<String>,
    pub can_read: bool,
    pub can_write: bool,
}

fn default_scope_resource_type() -> String {
    "bucket".to_string()
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct IssuedKeyResponse {
    pub api_key: ApiKey,
    pub raw_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BucketStatus {
    Active,
    Archived,
    Nuked,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BucketListResponse {
    pub buckets: Vec<BucketSummary>,
    pub total: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BucketSummary {
    pub bucket: String,
    pub status: BucketStatus,
    #[serde(default)]
    pub account_count: Option<usize>,
    #[serde(default)]
    pub event_count: Option<usize>,
    #[serde(default)]
    pub total_balance: Option<i64>,
    #[serde(default)]
    pub available_balance: Option<i64>,
    #[serde(default)]
    pub last_event_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub archived_at_unix_ms: Option<i64>,
    #[serde(default)]
    pub deleted_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BucketDetailResponse {
    pub summary: BucketDetailSummary,
    pub accounts: Vec<AccountSummary>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BucketDetailSummary {
    pub bucket: String,
    pub account_count: usize,
    pub event_count: usize,
    pub available_balance: i64,
    pub active_hold_total: i64,
    pub last_event_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AccountSummary {
    pub account: String,
    pub balance: i64,
    pub available_balance: i64,
    pub active_hold_total: i64,
    pub event_count: usize,
    pub last_event_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EventListResponse {
    pub events: Vec<BucketEvent>,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct BucketEvent {
    pub event_id: String,
    pub origin_node_id: Option<String>,
    pub created_at_unix_ms: u64,
    #[serde(default)]
    pub r#type: String,
    pub bucket: Option<String>,
    pub account: String,
    pub amount: i64,
    pub note: Option<String>,
    pub idempotency_nonce: Option<String>,
    pub void_ref: Option<String>,
    #[serde(default)]
    pub hold_amount: u64,
    #[serde(default)]
    pub hold_expires_at_unix_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateEventRequest {
    pub account: String,
    pub amount: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_overdraft: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_acks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack_timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct EdgeInfo {
    pub edge_id: String,
    pub region: String,
    pub base_url: String,
    pub node_id: Option<String>,
    pub label: Option<String>,
    pub node_label: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EdgeHealth {
    pub ready: bool,
    pub discovered_nodes: usize,
    pub healthy_nodes: usize,
    pub best_node_rtt_ms: Option<u64>,
    pub sync_gap: Option<u64>,
    pub overloaded: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct MeshNodeSummary {
    pub node_id: String,
    pub peer_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub advertise_addr: Option<String>,
    #[serde(default)]
    pub listen_addrs: Vec<String>,
    #[serde(default)]
    pub ping_rtt_ms: Option<u64>,
    #[serde(default)]
    pub ready: Option<bool>,
    #[serde(default)]
    pub sync_gap: Option<u64>,
    #[serde(default)]
    pub overloaded: Option<bool>,
    #[serde(default)]
    pub failure_count: u32,
    #[serde(default)]
    pub is_best: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct MeshEdgeNodes {
    pub edge_id: String,
    pub region: String,
    pub base_url: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub nodes: Vec<MeshNodeSummary>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AdminStats {
    pub total_users: usize,
    pub users_last_7_days: usize,
    pub frozen_users: usize,
    pub admin_users: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserListResponse {
    pub users: Vec<UserSummary>,
    pub total: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserSummary {
    pub id: String,
    pub email: String,
    pub is_admin: bool,
    pub is_frozen: bool,
    pub last_login_at: Option<String>,
    pub created_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AdminSubscription {
    pub plan_slug: String,
    pub plan_name: String,
    pub monthly_credits: i64,
    pub credit_balance: i64,
    pub subscription_status: String,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuditListResponse {
    pub entries: Vec<AuditEntry>,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AdminEvent {
    pub event_id: String,
    pub origin_node_id: String,
    #[serde(default = "default_origin_epoch")]
    pub origin_epoch: u32,
    pub origin_seq: u64,
    pub created_at_unix_ms: u64,
    #[serde(default)]
    pub r#type: String,
    pub bucket: String,
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

fn default_origin_epoch() -> u32 {
    1
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdminEventListResponse {
    #[serde(default)]
    pub events: Vec<AdminEvent>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    /// Per-`(bucket, origin, epoch)` heads from the answering node.
    /// Key format: `"{bucket}\t{origin}:{epoch}"`.
    #[serde(default)]
    pub heads: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub max_known_seqs: std::collections::BTreeMap<String, u64>,
    /// Optional per-node replication snapshot (only present when
    /// the request asked for replication=true).
    #[serde(default)]
    pub replication: Option<ReplicationSnapshot>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ReplicationSnapshot {
    #[serde(default)]
    pub per_node: std::collections::BTreeMap<String, ReplicationNodeEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct ReplicationNodeEntry {
    #[serde(default)]
    pub heads: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub max_known_seqs: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AdminEventsFilter {
    pub bucket: String,
    pub account: String,
    pub origin: String,
    pub event_type: String,
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub search: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuditEntry {
    pub created_at: String,
    pub admin_email: String,
    pub action: String,
    pub target_email: Option<String>,
    pub target_user_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
}
