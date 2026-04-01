//! In-memory broadcaster for unit tests.
//! Multiple "nodes" in the same process share a broadcast channel.

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

use shardd_types::Event;

use crate::{AckInfo, BroadcastMsg, Broadcaster};

/// Shared channel that multiple InMemoryBroadcaster instances subscribe to.
#[derive(Clone)]
pub struct InMemoryBus {
    tx: broadcast::Sender<BroadcastMsg>,
}

impl InMemoryBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Create a broadcaster connected to this bus.
    pub fn broadcaster(&self) -> InMemoryBroadcaster {
        InMemoryBroadcaster {
            tx: self.tx.clone(),
            rx: Arc::new(tokio::sync::Mutex::new(self.tx.subscribe())),
            peer_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Update peer count for all broadcasters (simulates cluster size).
    pub fn set_peer_count(&self, _count: usize) {
        // Each broadcaster tracks its own peer count
    }
}

pub struct InMemoryBroadcaster {
    tx: broadcast::Sender<BroadcastMsg>,
    rx: Arc<tokio::sync::Mutex<broadcast::Receiver<BroadcastMsg>>>,
    peer_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl InMemoryBroadcaster {
    /// Receive the next broadcast message (for test consumption).
    pub async fn recv(&self) -> Option<BroadcastMsg> {
        self.rx.lock().await.recv().await.ok()
    }

    pub fn set_peer_count(&self, count: usize) {
        self.peer_count.store(count, std::sync::atomic::Ordering::Relaxed);
    }
}

#[async_trait]
impl Broadcaster for InMemoryBroadcaster {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, _ack_timeout_ms: u64) -> AckInfo {
        let _ = self.tx.send(BroadcastMsg::Event(event.clone()));

        // In-memory: assume all receivers got it instantly
        let peers = self.peer_count.load(std::sync::atomic::Ordering::Relaxed) as u32;
        let received = peers.min(min_acks);
        AckInfo {
            received,
            requested: min_acks,
            timeout: received < min_acks && min_acks > 0,
        }
    }

    async fn broadcast_persisted(&self, keys: &[(String, u64)]) {
        let _ = self.tx.send(BroadcastMsg::Persisted(keys.to_vec()));
    }

    async fn peer_count(&self) -> usize {
        self.peer_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}
