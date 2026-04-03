//! HTTP-based broadcaster — POST /events/replicate to known peers.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::debug;

use shardd_types::Event;
use crate::{AckInfo, Broadcaster};

pub struct HttpBroadcaster {
    peers: Arc<Mutex<Vec<String>>>,
    client: reqwest::Client,
}

impl HttpBroadcaster {
    pub fn new(peers: Vec<String>) -> Self {
        Self {
            peers: Arc::new(Mutex::new(peers)),
            client: reqwest::Client::new(),
        }
    }

    pub async fn set_peers(&self, peers: Vec<String>) {
        *self.peers.lock().await = peers;
    }
}

#[async_trait]
impl Broadcaster for HttpBroadcaster {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, ack_timeout_ms: u64) -> AckInfo {
        let peers = self.peers.lock().await.clone();

        if min_acks == 0 {
            for peer in peers {
                let client = self.client.clone();
                let url = format!("http://{peer}/events/replicate");
                let event = event.clone();
                tokio::spawn(async move {
                    let _ = client.post(&url).json(&event)
                        .timeout(Duration::from_secs(3)).send().await;
                });
            }
            return AckInfo::fire_and_forget();
        }

        let timeout = Duration::from_millis(ack_timeout_ms);
        let mut handles = Vec::new();
        for peer in peers {
            let client = self.client.clone();
            let event = event.clone();
            let url = format!("http://{peer}/events/replicate");
            handles.push(tokio::spawn(async move {
                client.post(&url).json(&event).timeout(timeout).send().await
                    .map(|r| r.status().is_success()).unwrap_or(false)
            }));
        }

        let mut received = 0u32;
        let deadline = tokio::time::Instant::now() + timeout;
        for handle in handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() { break; }
            if let Ok(Ok(true)) = tokio::time::timeout(remaining, handle).await {
                received += 1;
                if received >= min_acks { break; }
            }
        }

        AckInfo { received, requested: min_acks, timeout: received < min_acks }
    }

    async fn broadcast_persisted(&self, _keys: &[(String, u32, u64)]) {
        // Persistence notifications are best-effort; skip for HTTP broadcaster
    }

    async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }
}
