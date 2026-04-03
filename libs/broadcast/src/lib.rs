//! Broadcast abstraction per protocol.md v1.7 §4.1, §12.
//!
//! Implementations:
//! - `HttpBroadcaster` — POST to known peers with ack collection
//! - `InMemoryBroadcaster` — tokio channels for unit tests
//! - `GossipBroadcaster` — foca SWIM (task 0015)

pub mod http;
pub mod memory;
pub mod gossip;

use async_trait::async_trait;
use shardd_types::Event;

/// Ack info returned from broadcast.
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

/// Trait for broadcasting events across cluster nodes.
#[async_trait]
pub trait Broadcaster: Send + Sync + 'static {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, ack_timeout_ms: u64) -> AckInfo;
    async fn broadcast_persisted(&self, keys: &[(String, u32, u64)]);
    async fn peer_count(&self) -> usize;
}
