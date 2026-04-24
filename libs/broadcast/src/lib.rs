//! Broadcast abstraction per protocol.md v1.7 §4.1, §12.
//!
//! Implementations:
//! - `LibP2pBroadcaster` — libp2p (gossipsub + request-response + Kademlia + PSK)
//! - `InMemoryBroadcaster` — tokio channels for unit tests

pub mod discovery;
#[path = "libp2p.rs"]
pub mod libp2p_broadcaster;
pub mod memory;
pub mod mesh_client;
pub mod metadata;

// Re-export libp2p crate for consumers (node crate doesn't need libp2p dep directly).
pub use ::libp2p as libp2p_crate;

use async_trait::async_trait;
use shardd_types::{Event, OriginKey};

// Re-export AckInfo from types (single definition, no duplication).
pub use shardd_types::AckInfo;

/// Trait for broadcasting events across cluster nodes.
#[async_trait]
pub trait Broadcaster: Send + Sync + 'static {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, ack_timeout_ms: u64) -> AckInfo;
    async fn broadcast_persisted(&self, keys: &[OriginKey]);
    async fn peer_count(&self) -> usize;
}
