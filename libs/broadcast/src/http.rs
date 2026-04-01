//! HTTP-based broadcaster. Sends events to known peers via POST /events/replicate.
//! Simple deployment model — peers are configured manually.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, warn};

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

    pub async fn add_peer(&self, addr: String) {
        let mut peers = self.peers.lock().await;
        if !peers.contains(&addr) {
            peers.push(addr);
        }
    }
}

#[async_trait]
impl Broadcaster for HttpBroadcaster {
    async fn broadcast_event(
        &self,
        event: &Event,
        min_acks: u32,
        ack_timeout_ms: u64,
    ) -> AckInfo {
        let peers = self.peers.lock().await.clone();

        if min_acks == 0 {
            for peer in peers {
                let client = self.client.clone();
                let url = format!("http://{peer}/events/replicate");
                let event = event.clone();
                tokio::spawn(async move {
                    if let Err(e) = client
                        .post(&url)
                        .json(&event)
                        .timeout(Duration::from_secs(3))
                        .send()
                        .await
                    {
                        debug!(error = %e, "broadcast failed");
                    }
                });
            }
            return AckInfo::fire_and_forget();
        }

        let timeout = Duration::from_millis(ack_timeout_ms);
        let mut handles = Vec::new();

        for peer in peers {
            let client = self.client.clone();
            let url = format!("http://{peer}/events/replicate");
            let event = event.clone();
            handles.push(tokio::spawn(async move {
                client
                    .post(&url)
                    .json(&event)
                    .timeout(timeout)
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
            }));
        }

        // Collect results with overall timeout
        let mut received = 0u32;
        let deadline = tokio::time::Instant::now() + timeout;

        for handle in handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, handle).await {
                Ok(Ok(true)) => {
                    received += 1;
                    if received >= min_acks {
                        break;
                    }
                }
                _ => {}
            }
        }

        AckInfo {
            received,
            requested: min_acks,
            timeout: received < min_acks,
        }
    }

    async fn broadcast_persisted(&self, keys: &[(String, u64)]) {
        let peers = self.peers.lock().await.clone();
        for peer in peers {
            let client = self.client.clone();
            let url = format!("http://{peer}/events/persisted");
            let keys = keys.to_vec();
            tokio::spawn(async move {
                if let Err(e) = client
                    .post(&url)
                    .json(&keys)
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                {
                    debug!(error = %e, "persisted broadcast failed");
                }
            });
        }
    }

    async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }
}
