//! In-memory broadcaster for unit tests.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::broadcast;

use crate::{AckInfo, Broadcaster};
use shardd_types::Event;

#[derive(Clone)]
pub struct InMemoryBus {
    tx: broadcast::Sender<Event>,
}

impl InMemoryBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn broadcaster(&self) -> InMemoryBroadcaster {
        InMemoryBroadcaster {
            tx: self.tx.clone(),
            peer_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

pub struct InMemoryBroadcaster {
    tx: broadcast::Sender<Event>,
    peer_count: Arc<AtomicUsize>,
}

impl InMemoryBroadcaster {
    pub fn set_peer_count(&self, count: usize) {
        self.peer_count.store(count, Ordering::Relaxed);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[async_trait]
impl Broadcaster for InMemoryBroadcaster {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, _ack_timeout_ms: u64) -> AckInfo {
        let _ = self.tx.send(event.clone());
        let peers = self.peer_count.load(Ordering::Relaxed) as u32;
        AckInfo {
            received: peers.min(min_acks),
            requested: min_acks,
            timeout: min_acks > 0 && peers < min_acks,
        }
    }

    async fn broadcast_persisted(&self, _keys: &[shardd_types::OriginKey]) {}

    async fn peer_count(&self) -> usize {
        self.peer_count.load(Ordering::Relaxed)
    }
}
