//! Broadcast abstraction for event dissemination across cluster nodes.
//!
//! Three implementations:
//! - `InMemoryBroadcaster` — tokio channels, for unit tests
//! - `HttpBroadcaster` — POST to known peers, for simple deployments
//! - `GossipBroadcaster` — foca SWIM protocol, for scalable clusters

pub mod http;
pub mod memory;
pub mod gossip;

use async_trait::async_trait;
use shardd_types::Event;

/// Result of broadcasting an event with optional quorum acks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AckInfo {
    pub received: u32,
    pub requested: u32,
    pub timeout: bool,
}

impl AckInfo {
    pub fn fire_and_forget() -> Self {
        Self { received: 0, requested: 0, timeout: false }
    }
}

/// Messages that flow through the broadcast layer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum BroadcastMsg {
    Event(Event),
    Persisted(Vec<(String, u64)>),
}

/// Trait for broadcasting events across cluster nodes.
/// Implementations hide transport details (in-memory, HTTP, gossip).
#[async_trait]
pub trait Broadcaster: Send + Sync + 'static {
    /// Broadcast an event to all cluster members.
    /// If min_acks > 0, waits until that many peers acknowledge or timeout.
    async fn broadcast_event(
        &self,
        event: &Event,
        min_acks: u32,
        ack_timeout_ms: u64,
    ) -> AckInfo;

    /// Notify cluster that events have been persisted to Postgres.
    async fn broadcast_persisted(&self, keys: &[(String, u64)]);

    /// Get the number of known peers.
    async fn peer_count(&self) -> usize;
}
