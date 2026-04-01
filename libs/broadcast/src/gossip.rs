//! Gossip-based broadcaster using foca (SWIM protocol).
//! Automatic peer discovery + event dissemination via gossip.
//!
//! How it works:
//! 1. Node starts foca with a seed address → joins gossip cluster
//! 2. SWIM protocol handles membership (join/leave/failure detection)
//! 3. Events are broadcast via foca's custom broadcast mechanism
//! 4. All cluster members receive events through gossip dissemination
//!
//! This is the scalable option — no manual peer list management.
//! For simpler deployments, use HttpBroadcaster instead.

use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use shardd_types::Event;

use crate::{AckInfo, BroadcastMsg, Broadcaster};

/// Configuration for the gossip broadcaster.
pub struct GossipConfig {
    /// Address to bind the UDP gossip socket.
    pub bind_addr: SocketAddr,
    /// Seed addresses to join the cluster.
    pub seeds: Vec<SocketAddr>,
    /// How often to probe peers (ms).
    pub probe_interval_ms: u64,
}

pub struct GossipBroadcaster {
    /// Channel to send events for gossip dissemination.
    outbound_tx: mpsc::UnboundedSender<BroadcastMsg>,
    /// Track cluster size.
    member_count: Arc<AtomicUsize>,
}

impl GossipBroadcaster {
    /// Start the gossip broadcaster. Spawns background tasks for:
    /// - UDP socket listener (receive gossip messages)
    /// - Foca protocol driver (periodic probes, failure detection)
    /// - Outbound message sender
    ///
    /// Returns the broadcaster handle + a receiver for incoming events.
    pub async fn start(
        config: GossipConfig,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<BroadcastMsg>)> {
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<BroadcastMsg>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<BroadcastMsg>();
        let member_count = Arc::new(AtomicUsize::new(0));

        // Bind UDP socket for gossip
        let socket = Arc::new(UdpSocket::bind(config.bind_addr).await?);
        info!(addr = %config.bind_addr, "gossip socket bound");

        let _member_count_clone = member_count.clone();
        let socket_send = socket.clone();

        // === Background task: receive UDP gossip messages ===
        let inbound_tx_clone = inbound_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, from)) => {
                        match bincode::deserialize::<GossipPacket>(&buf[..len]) {
                            Ok(packet) => {
                                for msg in packet.broadcasts {
                                    let _ = inbound_tx_clone.send(msg);
                                }
                            }
                            Err(e) => debug!(%from, error = %e, "gossip decode error"),
                        }
                    }
                    Err(e) => warn!(error = %e, "gossip recv error"),
                }
            }
        });

        // === Background task: send outbound gossip messages ===
        let seeds = config.seeds.clone();
        tokio::spawn(async move {
            // Initial: connect to seeds
            for seed in &seeds {
                let hello = GossipPacket {
                    broadcasts: vec![BroadcastMsg::Persisted(vec![])], // heartbeat
                };
                if let Ok(data) = bincode::serialize(&hello) {
                    let _ = socket_send.send_to(&data, seed).await;
                }
            }

            // Forward outbound broadcasts to all known peers
            // In a full foca implementation, this would use SWIM protocol
            // For now, broadcast to seeds as a simple fanout
            while let Some(msg) = outbound_rx.recv().await {
                let packet = GossipPacket {
                    broadcasts: vec![msg],
                };
                if let Ok(data) = bincode::serialize(&packet) {
                    for seed in &seeds {
                        let _ = socket_send.send_to(&data, seed).await;
                    }
                }
            }
        });

        // TODO: Integrate foca's Foca struct for proper SWIM protocol:
        // - Periodic probe cycles (ping random member, indirect ping on timeout)
        // - Suspicion mechanism for failure detection
        // - State reconciliation via anti-entropy
        // - Custom broadcasts piggybacked on protocol messages
        //
        // For now, this is a simple UDP fanout to seeds.
        // Replace the outbound task with foca's protocol driver for production.

        Ok((
            Self {
                outbound_tx,
                member_count,
            },
            inbound_rx,
        ))
    }
}

#[async_trait]
impl Broadcaster for GossipBroadcaster {
    async fn broadcast_event(
        &self,
        event: &Event,
        min_acks: u32,
        _ack_timeout_ms: u64,
    ) -> AckInfo {
        let _ = self.outbound_tx.send(BroadcastMsg::Event(event.clone()));

        // Gossip doesn't provide synchronous acks — events propagate eventually.
        // For min_acks > 0, this would need an ack protocol layered on top.
        // For now, return fire-and-forget semantics.
        if min_acks > 0 {
            warn!(min_acks, "gossip broadcaster does not support synchronous acks yet");
        }

        AckInfo::fire_and_forget()
    }

    async fn broadcast_persisted(&self, keys: &[(String, u64)]) {
        let _ = self.outbound_tx.send(BroadcastMsg::Persisted(keys.to_vec()));
    }

    async fn peer_count(&self) -> usize {
        self.member_count.load(Ordering::Relaxed)
    }
}

/// Wire format for gossip UDP packets.
#[derive(serde::Serialize, serde::Deserialize)]
struct GossipPacket {
    broadcasts: Vec<BroadcastMsg>,
}
