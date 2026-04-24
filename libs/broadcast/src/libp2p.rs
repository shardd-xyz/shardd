//! libp2p-based broadcaster: gossipsub + request-response + Kademlia + PSK private mesh.
//!
//! Replaces foca SWIM (gossip.rs), HttpBroadcaster (http.rs), and HybridBroadcaster (hybrid.rs)
//! with a unified networking layer based on rust-libp2p.
//!
//! Per protocol.md §12:
//! - gossipsub handles fire-and-forget event dissemination (`shardd/events/v1` topic)
//! - request-response handles quorum acks (min_acks > 0) and sync queries
//! - Kademlia DHT provides automatic peer discovery
//! - Identify exchanges peer metadata (node_id, epoch)
//! - PSK (pre-shared key) encrypts the private mesh

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder, Transport,
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade},
    dns, gossipsub, identify, kad, noise, ping,
    pnet::{PnetConfig, PreSharedKey},
    request_response::{self, ProtocolSupport},
    swarm::{ConnectionError, NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::metadata::{
    PROTOCOL_VERSION, SharddPeerMetadata, encode_agent_version, parse_agent_version,
};
use crate::{AckInfo, Broadcaster};
use shardd_types::{Event, NodeRpcRequest, NodeRpcResult};

// ── Constants ──────────────────────────────────────────────────────

/// Gossipsub topic for event dissemination (§12.2).
pub const EVENT_TOPIC: &str = "shardd/events/v1";

/// Request-response protocol for quorum acks (§12.3).
const ACK_PROTOCOL: &str = "/shardd/ack/1";

/// Request-response protocol for head exchange (§4.2).
///
/// v2 (protocol.md v1.8): heads are keyed by `(bucket, origin_node_id,
/// origin_epoch)`. Wire-level key format is `"{bucket}\t{origin}:{epoch}"`.
/// A pre-v2 peer will still negotiate the old protocol name (`/shardd/
/// heads/1`), which this binary no longer advertises — that's intentional,
/// since the v2 wipe invalidated all v1 data.
const HEADS_PROTOCOL: &str = "/shardd/heads/2";

/// Request-response protocol for event range fetch (§4.2).
const RANGE_PROTOCOL: &str = "/shardd/range/2";

/// Request-response protocol for mesh membership snapshots.
const MEMBERS_PROTOCOL: &str = "/shardd/members/1";

/// Request-response protocol for client RPCs over libp2p.
pub const CLIENT_PROTOCOL: &str = "/shardd/client/1";

// ── Request-response types ─────────────────────────────────────────

/// Request sent via libp2p request-response to collect a quorum acknowledgment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckRequest {
    pub event: Event,
}

/// Response: whether the peer has the event after handling the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckResponse {
    pub inserted: bool,
}

/// Incoming ack request from a peer, forwarded to server.rs for handling.
#[derive(Debug)]
pub struct IncomingAckRequest {
    pub event: Event,
    pub response_tx: oneshot::Sender<AckResponse>,
}

/// Request for a peer's contiguous heads (§4.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadsRequest;

/// Response: map of `"{bucket}\t{origin_node_id}:{origin_epoch}"` →
/// contiguous head sequence. See `HEADS_PROTOCOL` for the versioning note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadsResponse {
    pub heads: std::collections::BTreeMap<String, u64>,
}

/// Incoming heads request from a peer, forwarded to server.rs for handling.
#[derive(Debug)]
pub struct IncomingHeadsRequest {
    pub response_tx: oneshot::Sender<HeadsResponse>,
}

/// Request for an event range from a specific `(bucket, origin, epoch)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeRequest {
    pub bucket: String,
    pub origin_node_id: String,
    pub origin_epoch: u32,
    pub from_seq: u64,
    pub to_seq: u64,
}

/// Encode a `(bucket, origin, epoch)` triple into the `HeadsResponse`
/// wire format. Tab separator avoids ambiguity with `:` or `_` inside
/// bucket names.
pub fn encode_head_key(bucket: &str, origin: &str, epoch: u32) -> String {
    format!("{bucket}\t{origin}:{epoch}")
}

/// Decode a wire-format heads key into its `(bucket, origin, epoch)` parts.
/// Returns `None` if the shape doesn't match.
pub fn decode_head_key(key: &str) -> Option<(String, String, u32)> {
    let (bucket, origin_epoch) = key.split_once('\t')?;
    let (origin, epoch_str) = origin_epoch.rsplit_once(':')?;
    let epoch: u32 = epoch_str.parse().ok()?;
    Some((bucket.to_string(), origin.to_string(), epoch))
}

/// Response: the requested events (may be fewer than requested if missing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeResponse {
    pub events: Vec<Event>,
}

/// Incoming range request from a peer, forwarded to server.rs for handling.
#[derive(Debug)]
pub struct IncomingRangeRequest {
    pub request: RangeRequest,
    pub response_tx: oneshot::Sender<RangeResponse>,
}

/// Incoming client RPC request, forwarded to server.rs for handling.
#[derive(Debug)]
pub struct IncomingClientRpcRequest {
    pub request: NodeRpcRequest,
    pub response_tx: oneshot::Sender<NodeRpcResult>,
}

/// Request for a peer's current view of mesh membership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembersRequest;

/// Metadata for a single mesh member, used by clients to expand discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberInfo {
    pub peer_id: String,
    pub node_id: String,
    pub advertise_addr: Option<String>,
    pub listen_addrs: Vec<String>,
}

/// Response containing the node's current view of reachable peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembersResponse {
    pub members: Vec<MemberInfo>,
}

// ── Public configuration ───────────────────────────────────────────

/// Configuration for starting a LibP2pBroadcaster.
pub struct LibP2pConfig {
    /// Node ID from the database (stable across restarts).
    pub node_id: String,
    /// Current epoch.
    pub epoch: u32,
    /// Multiaddrs advertised to peers. First is the "primary" — used in the
    /// Identify agent_version for backward compat. All are registered as
    /// external addresses so libp2p includes them as dial candidates
    /// (happy-eyeballs race picks the fastest reachable one).
    pub advertise_addrs: Vec<String>,
    /// TCP address to bind libp2p transport on.
    pub listen_addr: SocketAddr,
    /// Bootstrap peer multiaddrs (e.g., `/ip4/1.2.3.4/tcp/9000`).
    pub bootstrap_peers: Vec<Multiaddr>,
    /// Optional 32-byte PSK for private mesh encryption.
    pub psk: Option<[u8; 32]>,
}

// ── Membership events ──────────────────────────────────────────────

/// Membership change notification from libp2p, forwarded to the registry bridge.
#[derive(Debug, Clone)]
pub enum MembershipEvent {
    Up {
        peer_id: PeerId,
        node_id: String,
        addr: Multiaddr,
        advertise_addr: Option<String>,
    },
    Down {
        peer_id: PeerId,
        node_id: String,
    },
}

// ── Swarm commands ─────────────────────────────────────────────────

/// Commands sent from LibP2pBroadcaster to the swarm task.
pub(crate) enum SwarmCommand {
    /// Publish an event via gossipsub (fire-and-forget).
    GossipPublish(Event),
    /// Send an event to up to min_acks peers and wait for acks.
    RequestAcks {
        event: Event,
        min_acks: u32,
        timeout_ms: u64,
        reply_tx: oneshot::Sender<AckInfo>,
    },
    /// List currently connected peers (for sync loop).
    ConnectedPeers {
        reply_tx: oneshot::Sender<Vec<PeerId>>,
    },
    /// Query a peer for its contiguous heads (§4.2).
    QueryHeads {
        peer: PeerId,
        reply_tx: oneshot::Sender<Option<HeadsResponse>>,
    },
    /// Fetch an event range from a peer (§4.2).
    QueryRange {
        peer: PeerId,
        request: RangeRequest,
        reply_tx: oneshot::Sender<Option<RangeResponse>>,
    },
}

// ── NetworkBehaviour ───────────────────────────────────────────────

/// Composite libp2p NetworkBehaviour for shardd.
#[derive(NetworkBehaviour)]
pub struct ShardBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub ack_rr: request_response::json::Behaviour<AckRequest, AckResponse>,
    pub heads_rr: request_response::json::Behaviour<HeadsRequest, HeadsResponse>,
    pub range_rr: request_response::json::Behaviour<RangeRequest, RangeResponse>,
    pub members_rr: request_response::json::Behaviour<MembersRequest, MembersResponse>,
    pub client_rr: request_response::json::Behaviour<NodeRpcRequest, NodeRpcResult>,
}

// ── Channels returned from construction ────────────────────────────

/// Channels exposed for main.rs to consume.
pub struct LibP2pChannels {
    /// Raw JSON bytes of events received from peers via gossipsub.
    /// Deserialization is deferred to a worker pool in main.rs so the
    /// swarm task stays responsive to network I/O.
    pub event_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Peer membership changes for registry bridge.
    pub membership_rx: mpsc::UnboundedReceiver<MembershipEvent>,
    /// Incoming ack requests from peers. Handler must send AckResponse via `response_tx`.
    pub incoming_ack_rx: mpsc::UnboundedReceiver<IncomingAckRequest>,
    /// Incoming heads requests from peers.
    pub incoming_heads_rx: mpsc::UnboundedReceiver<IncomingHeadsRequest>,
    /// Incoming range requests from peers.
    pub incoming_range_rx: mpsc::UnboundedReceiver<IncomingRangeRequest>,
    /// Incoming client RPC requests from external libp2p clients.
    pub incoming_client_rx: mpsc::UnboundedReceiver<IncomingClientRpcRequest>,
}

// ── LibP2pBroadcaster ──────────────────────────────────────────────

/// libp2p-based broadcaster implementing the Broadcaster trait.
pub struct LibP2pBroadcaster {
    cmd_tx: mpsc::Sender<SwarmCommand>,
    peer_count: Arc<AtomicUsize>,
    local_peer_id: PeerId,
}

impl LibP2pBroadcaster {
    /// The libp2p PeerId of this node.
    pub fn peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    /// List currently connected peers (used by catch-up sync loop).
    pub async fn connected_peers(&self) -> Vec<PeerId> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(SwarmCommand::ConnectedPeers { reply_tx: tx })
            .await;
        rx.await.unwrap_or_default()
    }

    /// Query a peer for its contiguous heads (§4.2).
    ///
    /// Per-request timeout is enforced by the request-response behaviour's
    /// static config (set in `Self::start`).
    pub async fn query_heads(&self, peer: PeerId) -> Option<HeadsResponse> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(SwarmCommand::QueryHeads { peer, reply_tx: tx })
            .await;
        rx.await.ok().flatten()
    }

    /// Fetch an event range from a peer (§4.2).
    ///
    /// Per-request timeout is enforced by the request-response behaviour's
    /// static config (set in `Self::start`).
    pub async fn query_range(&self, peer: PeerId, request: RangeRequest) -> Option<RangeResponse> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(SwarmCommand::QueryRange {
                peer,
                request,
                reply_tx: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    /// Start a libp2p swarm and return the broadcaster + channels for consumption.
    ///
    /// Creates:
    /// 1. TCP transport with Noise encryption and Yamux multiplexing (optional PSK)
    /// 2. NetworkBehaviour combining gossipsub + Kademlia + Identify
    /// 3. Swarm with the behaviour
    /// 4. Subscribes to the EVENT_TOPIC
    /// 5. Dials bootstrap peers
    /// 6. Spawns the swarm event loop in a tokio task
    pub async fn start(config: LibP2pConfig) -> anyhow::Result<(Self, LibP2pChannels)> {
        // Deterministically derive the libp2p keypair from the mesh PSK
        // mixed with the persistent node_id. The PSK is the secret half of
        // the seed (only mesh members have it), so the resulting private
        // key isn't reconstructible from the publicly-broadcast node_id.
        // The result is stable across restarts → peer caches, Kademlia
        // routing tables, and peer-id-pinned dials all stay valid after a
        // redeploy.
        // PSK is also consumed below by the pnet transport, so copy out
        // (it's a [u8; 32]) without moving it from the config.
        let identity_psk = config.psk.ok_or_else(|| {
            anyhow::anyhow!(
                "libp2p broadcaster requires a PSK (cluster_key) to derive a stable identity"
            )
        })?;
        let keypair = crate::discovery::derive_keypair_from_seed(&identity_psk, &config.node_id)?;
        let local_peer_id = PeerId::from(keypair.public());
        info!(peer_id = %local_peer_id, node_id = %config.node_id, "libp2p identity derived from psk+node_id");

        // Build gossipsub config. mesh_n_high=8 (was default 12) caps mesh
        // degree in small-to-medium clusters to bound per-node forwarding work.
        // heartbeat 700ms gives slightly snappier mesh grafts/prunes than the
        // default 1s. All other knobs use libp2p defaults (mesh_n, mesh_n_low,
        // mesh_outbound_min, gossip_lazy) because we have no tuned values.
        //
        // Peer scoring is intentionally NOT enabled: `PeerScoreParams::default()`
        // configures no topic weights, so it cannot rebalance hub load without
        // application-specific tuning. Revisit when we have bench data to
        // calibrate per-topic `TopicScoreParams`.
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_millis(700))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .mesh_n_high(8)
            .message_id_fn(|msg: &gossipsub::Message| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&msg.data, &mut hasher);
                gossipsub::MessageId::from(std::hash::Hasher::finish(&hasher).to_string())
            })
            .build()
            .map_err(|e| anyhow::anyhow!("gossipsub config: {e}"))?;

        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(keypair.clone()),
            gossipsub_config,
        )
        .map_err(|e| anyhow::anyhow!("gossipsub init: {e}"))?;

        // Subscribe to the event topic.
        let topic = gossipsub::IdentTopic::new(EVENT_TOPIC);
        gossipsub
            .subscribe(&topic)
            .map_err(|e| anyhow::anyhow!("gossipsub subscribe: {e}"))?;

        // Kademlia for peer discovery.
        let store = kad::store::MemoryStore::new(local_peer_id);
        let kademlia = kad::Behaviour::new(local_peer_id, store);

        // Identify for peer metadata (node_id embedded in agent_version).
        // Additional advertise addrs flow through the standard `listen_addrs`
        // field (populated by `add_external_address` below), not through
        // agent_version — we still ship one primary here for old code paths.
        let primary_advertise = config.advertise_addrs.first().cloned();
        let identify = identify::Behaviour::new(
            identify::Config::new(PROTOCOL_VERSION.to_string(), keypair.public())
                .with_agent_version(encode_agent_version(&SharddPeerMetadata {
                    node_id: config.node_id.clone(),
                    epoch: config.epoch,
                    advertise_addr: primary_advertise.clone(),
                })),
        );

        let ping = ping::Behaviour::new(
            ping::Config::new()
                .with_interval(Duration::from_secs(1))
                .with_timeout(Duration::from_secs(5)),
        );

        // Request-response for quorum acks (§12.3).
        let ack_rr = request_response::json::Behaviour::<AckRequest, AckResponse>::new(
            [(StreamProtocol::new(ACK_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Request-response for heads exchange (§4.2).
        let heads_rr = request_response::json::Behaviour::<HeadsRequest, HeadsResponse>::new(
            [(StreamProtocol::new(HEADS_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Request-response for event range fetch (§4.2).
        let range_rr = request_response::json::Behaviour::<RangeRequest, RangeResponse>::new(
            [(StreamProtocol::new(RANGE_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
        );

        let members_rr = request_response::json::Behaviour::<MembersRequest, MembersResponse>::new(
            [(StreamProtocol::new(MEMBERS_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );
        let client_rr = request_response::json::Behaviour::<NodeRpcRequest, NodeRpcResult>::new(
            [(StreamProtocol::new(CLIENT_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
        );

        let behaviour = ShardBehaviour {
            gossipsub,
            kademlia,
            identify,
            ping,
            ack_rr,
            heads_rr,
            range_rr,
            members_rr,
            client_rr,
        };

        // Build the transport. If a PSK is configured, wrap TCP with pnet encryption.
        let noise_config =
            noise::Config::new(&keypair).map_err(|e| anyhow::anyhow!("noise config: {e}"))?;

        let transport: Boxed<(PeerId, StreamMuxerBox)> = if let Some(psk_bytes) = config.psk {
            let psk = PreSharedKey::new(psk_bytes);
            info!(fingerprint = %psk.fingerprint(), "PSK private mesh enabled");
            dns::tokio::Transport::system(tcp::tokio::Transport::new(tcp::Config::default()))
                .map_err(|e| anyhow::anyhow!("dns transport: {e}"))?
                .and_then(move |socket, _| async move {
                    PnetConfig::new(psk)
                        .handshake(socket)
                        .await
                        .map_err(std::io::Error::other)
                })
                .upgrade(upgrade::Version::V1)
                .authenticate(noise_config)
                .multiplex(yamux::Config::default())
                .boxed()
        } else {
            dns::tokio::Transport::system(tcp::tokio::Transport::new(tcp::Config::default()))
                .map_err(|e| anyhow::anyhow!("dns transport: {e}"))?
                .upgrade(upgrade::Version::V1)
                .authenticate(noise_config)
                .multiplex(yamux::Config::default())
                .boxed()
        };

        let mut swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_other_transport(|_| Ok(transport))
            .map_err(|e| anyhow::anyhow!("transport: {e}"))?
            .with_behaviour(|_| behaviour)
            .map_err(|e| anyhow::anyhow!("behaviour: {e}"))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        // Listen on the configured address.
        let listen_multiaddr: Multiaddr = format!(
            "/ip4/{}/tcp/{}",
            config.listen_addr.ip(),
            config.listen_addr.port()
        )
        .parse()?;
        swarm.listen_on(listen_multiaddr)?;

        // Publish each advertised multiaddr as an external address. libp2p
        // includes these in Identify responses alongside the bound listen
        // addr, so peers learn every way to reach us (public, VPC-private,
        // Tailscale) and dial them in parallel — the fastest reachable one
        // wins. Cross-VPC peers fail-fast on the private IP and fall back.
        for raw in &config.advertise_addrs {
            match raw.parse::<Multiaddr>() {
                Ok(addr) => {
                    swarm.add_external_address(addr.clone());
                    info!(addr = %addr, "advertising external address");
                }
                Err(e) => warn!(raw = %raw, error = %e, "ignoring unparseable advertise_addr"),
            }
        }

        // Dial bootstrap peers.
        for peer_addr in &config.bootstrap_peers {
            match swarm.dial(peer_addr.clone()) {
                Ok(()) => info!(peer = %peer_addr, "dialing bootstrap peer"),
                Err(e) => warn!(peer = %peer_addr, error = %e, "failed to dial bootstrap peer"),
            }
        }

        // Channels
        let (cmd_tx, cmd_rx) = mpsc::channel::<SwarmCommand>(1024);
        let (event_tx, event_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (membership_tx, membership_rx) = mpsc::unbounded_channel::<MembershipEvent>();
        let (incoming_ack_tx, incoming_ack_rx) = mpsc::unbounded_channel::<IncomingAckRequest>();
        let (incoming_heads_tx, incoming_heads_rx) =
            mpsc::unbounded_channel::<IncomingHeadsRequest>();
        let (incoming_range_tx, incoming_range_rx) =
            mpsc::unbounded_channel::<IncomingRangeRequest>();
        let (incoming_client_tx, incoming_client_rx) =
            mpsc::unbounded_channel::<IncomingClientRpcRequest>();
        let peer_count = Arc::new(AtomicUsize::new(0));

        // Spawn the swarm event loop.
        let peer_count_clone = peer_count.clone();
        // Event-loop advertise_addr — used in self-identification replies to
        // peers that query our membership view. Falls back to the listen addr
        // when no advertise_addrs were provided (dev/test).
        let event_loop_advertise = primary_advertise.clone().unwrap_or_else(|| {
            format!(
                "/ip4/{}/tcp/{}",
                config.listen_addr.ip(),
                config.listen_addr.port()
            )
        });
        tokio::spawn(swarm_event_loop(
            swarm,
            config.node_id.clone(),
            event_loop_advertise,
            cmd_rx,
            event_tx,
            membership_tx,
            incoming_ack_tx,
            incoming_heads_tx,
            incoming_range_tx,
            incoming_client_tx,
            peer_count_clone,
            topic,
        ));

        Ok((
            Self {
                cmd_tx,
                peer_count,
                local_peer_id,
            },
            LibP2pChannels {
                event_rx,
                membership_rx,
                incoming_ack_rx,
                incoming_heads_rx,
                incoming_range_rx,
                incoming_client_rx,
            },
        ))
    }
}

// ── Swarm event loop ───────────────────────────────────────────────

/// State for an in-flight quorum ack request.
struct PendingAck {
    min_acks: u32,
    received: u32,
    outstanding: u32,
    reply_tx: Option<oneshot::Sender<AckInfo>>,
    deadline: tokio::time::Instant,
}

#[derive(Debug, Clone)]
struct KnownPeer {
    node_id: String,
    advertise_addr: Option<String>,
    listen_addrs: Vec<Multiaddr>,
}

#[allow(clippy::too_many_arguments)]
async fn swarm_event_loop(
    mut swarm: Swarm<ShardBehaviour>,
    local_node_id: String,
    local_advertise_addr: String,
    mut cmd_rx: mpsc::Receiver<SwarmCommand>,
    event_tx: mpsc::UnboundedSender<Vec<u8>>,
    membership_tx: mpsc::UnboundedSender<MembershipEvent>,
    incoming_ack_tx: mpsc::UnboundedSender<IncomingAckRequest>,
    incoming_heads_tx: mpsc::UnboundedSender<IncomingHeadsRequest>,
    incoming_range_tx: mpsc::UnboundedSender<IncomingRangeRequest>,
    incoming_client_tx: mpsc::UnboundedSender<IncomingClientRpcRequest>,
    peer_count: Arc<AtomicUsize>,
    topic: gossipsub::IdentTopic,
) {
    // Track peer_id → metadata (populated via Identify).
    let mut known_peers: HashMap<PeerId, KnownPeer> = HashMap::new();
    let mut local_listen_addrs: Vec<Multiaddr> = Vec::new();
    // Track outbound request-response for acks: maps group_id → pending state.
    let mut req_to_group: HashMap<request_response::OutboundRequestId, u64> = HashMap::new();
    let mut pending_groups: HashMap<u64, PendingAck> = HashMap::new();
    let mut next_group_id: u64 = 0;

    // Pending outbound sync queries: map OutboundRequestId → oneshot sender.
    let mut pending_heads: HashMap<
        request_response::OutboundRequestId,
        oneshot::Sender<Option<HeadsResponse>>,
    > = HashMap::new();
    let mut pending_range: HashMap<
        request_response::OutboundRequestId,
        oneshot::Sender<Option<RangeResponse>>,
    > = HashMap::new();

    // Pending inbound requests awaiting handler responses — keyed by a monotonic id
    // because ResponseChannel cannot be hashed. We use sequential ids and match by order.
    // For inbound, we buffer the ResponseChannel + associated reply channel, poll every tick.
    let mut inbound_heads_pending: Vec<(
        request_response::ResponseChannel<HeadsResponse>,
        oneshot::Receiver<HeadsResponse>,
    )> = Vec::new();
    let mut inbound_range_pending: Vec<(
        request_response::ResponseChannel<RangeResponse>,
        oneshot::Receiver<RangeResponse>,
    )> = Vec::new();
    let mut inbound_ack_pending: Vec<(
        request_response::ResponseChannel<AckResponse>,
        oneshot::Receiver<AckResponse>,
    )> = Vec::new();
    let mut inbound_client_pending: Vec<(
        request_response::ResponseChannel<NodeRpcResult>,
        oneshot::Receiver<NodeRpcResult>,
    )> = Vec::new();

    loop {
        tokio::select! {
            // Commands from LibP2pBroadcaster
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SwarmCommand::GossipPublish(event)) => {
                        match serde_json::to_vec(&event) {
                            Ok(data) => {
                                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic.clone(), data) {
                                    debug!(error = %e, "gossipsub publish failed (no peers?)");
                                }
                            }
                            Err(e) => error!(error = %e, "event serialize failed"),
                        }
                    }
                    Some(SwarmCommand::RequestAcks { event, min_acks, timeout_ms, reply_tx }) => {
                        // §12.3: Send AckRequest to connected peers and wait for a quorum.
                        let connected: Vec<PeerId> = swarm.connected_peers().copied().collect();
                        let targets = connected;

                        if targets.is_empty() {
                            // No peers to ask — immediately return with 0 acks.
                            let _ = reply_tx.send(AckInfo {
                                received: 0,
                                requested: min_acks,
                                timeout: true,
                            });
                            continue;
                        }

                        let group_id = next_group_id;
                        next_group_id += 1;
                        let outstanding = targets.len() as u32;
                        pending_groups.insert(group_id, PendingAck {
                            min_acks,
                            received: 0,
                            outstanding,
                            reply_tx: Some(reply_tx),
                            deadline: tokio::time::Instant::now() + Duration::from_millis(timeout_ms),
                        });

                        for peer_id in targets {
                            let req_id = swarm.behaviour_mut().ack_rr.send_request(
                                &peer_id,
                                AckRequest { event: event.clone() },
                            );
                            req_to_group.insert(req_id, group_id);
                        }
                    }
                    Some(SwarmCommand::ConnectedPeers { reply_tx }) => {
                        let peers: Vec<PeerId> = swarm.connected_peers().copied().collect();
                        let _ = reply_tx.send(peers);
                    }
                    Some(SwarmCommand::QueryHeads { peer, reply_tx }) => {
                        let req_id = swarm.behaviour_mut().heads_rr.send_request(&peer, HeadsRequest);
                        pending_heads.insert(req_id, reply_tx);
                    }
                    Some(SwarmCommand::QueryRange { peer, request, reply_tx }) => {
                        let req_id = swarm.behaviour_mut().range_rr.send_request(&peer, request);
                        pending_range.insert(req_id, reply_tx);
                    }
                    None => {
                        info!("swarm event loop shutting down (cmd_tx dropped)");
                        break;
                    }
                }
            }

            // Timeout sweep for pending ack groups
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let now = tokio::time::Instant::now();
                let expired: Vec<u64> = pending_groups.iter()
                    .filter(|(_, p)| p.deadline <= now)
                    .map(|(id, _)| *id)
                    .collect();
                for group_id in expired {
                    if let Some(mut pending) = pending_groups.remove(&group_id)
                        && let Some(tx) = pending.reply_tx.take() {
                        let _ = tx.send(AckInfo {
                            received: pending.received,
                            requested: pending.min_acks,
                            timeout: pending.received < pending.min_acks,
                        });
                    }
                }
            }

            // Events from the swarm
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        if !local_listen_addrs.contains(&address) {
                            local_listen_addrs.push(address.clone());
                        }
                        info!(addr = %address, "libp2p listening");
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, endpoint, num_established, .. } => {
                        // Always register every endpoint with Kad — it wants the full address list.
                        swarm.behaviour_mut().kademlia.add_address(&peer_id, endpoint.get_remote_address().clone());
                        // libp2p allows multiple parallel connections to the same peer
                        // (simultaneous dial from both sides, Kad re-dial, etc). Only the
                        // 0→1 transition is a real peer-level connect.
                        if num_established.get() == 1 {
                            peer_count.fetch_add(1, Ordering::Relaxed);
                            info!(peer = %peer_id, "peer connected");
                        } else {
                            debug!(
                                peer = %peer_id,
                                num_established = num_established.get(),
                                "additional connection to already-connected peer"
                            );
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, num_established, endpoint, cause, .. } => {
                        // `num_established` here is the number of OTHER surviving
                        // connections to this peer. Only 1→0 is a real peer disconnect.
                        let direction = if endpoint.is_dialer() { "dialer" } else { "listener" };
                        let cause_str = match &cause {
                            Some(ConnectionError::IO(e)) => format!("io: {e}"),
                            Some(ConnectionError::KeepAliveTimeout) => "keep-alive timeout".to_string(),
                            None => "active-close".to_string(),
                        };
                        if num_established == 0 {
                            peer_count.fetch_sub(1, Ordering::Relaxed);
                            if let Some(known_peer) = known_peers.remove(&peer_id) {
                                let _ = membership_tx.send(MembershipEvent::Down {
                                    peer_id,
                                    node_id: known_peer.node_id,
                                });
                            }
                            info!(
                                peer = %peer_id,
                                direction,
                                cause = %cause_str,
                                "peer disconnected"
                            );
                        } else {
                            debug!(
                                peer = %peer_id,
                                num_remaining = num_established,
                                direction,
                                cause = %cause_str,
                                "redundant connection closed"
                            );
                        }
                    }
                    SwarmEvent::OutgoingConnectionError { peer_id, error, connection_id, .. } => {
                        // Debug-level: outgoing dial failures are normal during
                        // libp2p churn (WrongPeerId after restart, transient
                        // timeouts, etc). Operators tracing a real outage can
                        // bump shardd_broadcast=debug temporarily.
                        debug!(
                            peer = ?peer_id,
                            connection_id = ?connection_id,
                            error = %error,
                            "outgoing connection error"
                        );
                    }
                    SwarmEvent::IncomingConnectionError { error, send_back_addr, peer_id, .. } => {
                        debug!(
                            peer = ?peer_id,
                            from = %send_back_addr,
                            error = %error,
                            "incoming connection error"
                        );
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. }
                    )) => {
                        // Forward raw bytes to the worker pool. Deserialization +
                        // state.insert_event happen off the swarm task's hot path.
                        let _ = event_tx.send(message.data);
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::Identify(
                        identify::Event::Received { peer_id, info, .. }
                    )) => {
                        if let Some(metadata) = parse_agent_version(&info.agent_version) {
                            let listen_addrs = info.listen_addrs.clone();
                            known_peers.insert(peer_id, KnownPeer {
                                node_id: metadata.node_id.clone(),
                                advertise_addr: metadata.advertise_addr.clone(),
                                listen_addrs: listen_addrs.clone(),
                            });
                            if let Some(addr) = info.listen_addrs.first() {
                                let _ = membership_tx.send(MembershipEvent::Up {
                                    peer_id,
                                    node_id: metadata.node_id,
                                    addr: addr.clone(),
                                    advertise_addr: metadata.advertise_addr,
                                });
                            }
                        }
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::Ping(event)) => {
                        match event.result {
                            Ok(rtt) => debug!(peer = %event.peer, rtt_ms = rtt.as_millis(), "ping succeeded"),
                            Err(error) => debug!(peer = %event.peer, error = %error, "ping failed"),
                        }
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::AckRr(
                        request_response::Event::Message { message, .. }
                    )) => {
                        match message {
                            request_response::Message::Request { request, channel, .. } => {
                                // Forward to server.rs handler via channel, buffer the response channel.
                                let (tx, rx) = oneshot::channel();
                                if incoming_ack_tx.send(IncomingAckRequest {
                                    event: request.event,
                                    response_tx: tx,
                                }).is_ok() {
                                    inbound_ack_pending.push((channel, rx));
                                }
                            }
                            request_response::Message::Response { request_id, response } => {
                                if let Some(group_id) = req_to_group.remove(&request_id)
                                    && let Some(pending) = pending_groups.get_mut(&group_id) {
                                    if response.inserted {
                                        pending.received += 1;
                                    }
                                    pending.outstanding = pending.outstanding.saturating_sub(1);
                                    let done = pending.received >= pending.min_acks
                                        || pending.outstanding == 0;
                                    if done {
                                        if let Some(tx) = pending.reply_tx.take() {
                                            let _ = tx.send(AckInfo {
                                                received: pending.received,
                                                requested: pending.min_acks,
                                                timeout: pending.received < pending.min_acks,
                                            });
                                        }
                                        pending_groups.remove(&group_id);
                                    }
                                }
                            }
                        }
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::AckRr(
                        request_response::Event::OutboundFailure { request_id, .. }
                    )) => {
                        if let Some(group_id) = req_to_group.remove(&request_id)
                            && let Some(pending) = pending_groups.get_mut(&group_id) {
                            pending.outstanding = pending.outstanding.saturating_sub(1);
                            if pending.outstanding == 0 {
                                if let Some(tx) = pending.reply_tx.take() {
                                    let _ = tx.send(AckInfo {
                                        received: pending.received,
                                        requested: pending.min_acks,
                                        timeout: pending.received < pending.min_acks,
                                    });
                                }
                                pending_groups.remove(&group_id);
                            }
                        }
                    }

                    // §4.2: Heads request-response
                    SwarmEvent::Behaviour(ShardBehaviourEvent::HeadsRr(
                        request_response::Event::Message { message, .. }
                    )) => {
                        match message {
                            request_response::Message::Request { request: _, channel, .. } => {
                                let (tx, rx) = oneshot::channel();
                                if incoming_heads_tx.send(IncomingHeadsRequest {
                                    response_tx: tx,
                                }).is_ok() {
                                    inbound_heads_pending.push((channel, rx));
                                }
                            }
                            request_response::Message::Response { request_id, response } => {
                                if let Some(tx) = pending_heads.remove(&request_id) {
                                    let _ = tx.send(Some(response));
                                }
                            }
                        }
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::HeadsRr(
                        request_response::Event::OutboundFailure { request_id, .. }
                    )) => {
                        if let Some(tx) = pending_heads.remove(&request_id) {
                            let _ = tx.send(None);
                        }
                    }

                    // §4.2: Range request-response
                    SwarmEvent::Behaviour(ShardBehaviourEvent::RangeRr(
                        request_response::Event::Message { message, .. }
                    )) => {
                        match message {
                            request_response::Message::Request { request, channel, .. } => {
                                let (tx, rx) = oneshot::channel();
                                if incoming_range_tx.send(IncomingRangeRequest {
                                    request,
                                    response_tx: tx,
                                }).is_ok() {
                                    inbound_range_pending.push((channel, rx));
                                }
                            }
                            request_response::Message::Response { request_id, response } => {
                                if let Some(tx) = pending_range.remove(&request_id) {
                                    let _ = tx.send(Some(response));
                                }
                            }
                        }
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::RangeRr(
                        request_response::Event::OutboundFailure { request_id, .. }
                    )) => {
                        if let Some(tx) = pending_range.remove(&request_id) {
                            let _ = tx.send(None);
                        }
                    }

                    SwarmEvent::Behaviour(ShardBehaviourEvent::MembersRr(
                        request_response::Event::Message {
                            message: request_response::Message::Request { channel, .. },
                            ..
                        }
                    )) => {
                        let mut members = Vec::with_capacity(known_peers.len() + 1);
                        members.push(MemberInfo {
                            peer_id: swarm.local_peer_id().to_string(),
                            node_id: local_node_id.clone(),
                            advertise_addr: Some(local_advertise_addr.clone()),
                            listen_addrs: local_listen_addrs.iter().map(ToString::to_string).collect(),
                        });
                        for (peer_id, known_peer) in &known_peers {
                            members.push(MemberInfo {
                                peer_id: peer_id.to_string(),
                                node_id: known_peer.node_id.clone(),
                                advertise_addr: known_peer.advertise_addr.clone(),
                                listen_addrs: known_peer.listen_addrs.iter().map(ToString::to_string).collect(),
                            });
                        }
                        let _ = swarm.behaviour_mut().members_rr.send_response(
                            channel,
                            MembersResponse { members },
                        );
                    }
                    SwarmEvent::Behaviour(ShardBehaviourEvent::ClientRr(
                        request_response::Event::Message {
                            message: request_response::Message::Request { request, channel, .. },
                            ..
                        }
                    )) => {
                        let (tx, rx) = oneshot::channel();
                        if incoming_client_tx.send(IncomingClientRpcRequest {
                            request,
                            response_tx: tx,
                        }).is_ok() {
                            inbound_client_pending.push((channel, rx));
                        }
                    }

                    _ => {}
                }
            }
        }

        // Drain inbound pending requests where handlers have responded.
        // send_response needs &mut swarm so we do it outside select!.
        let mut i = 0;
        while i < inbound_ack_pending.len() {
            match inbound_ack_pending[i].1.try_recv() {
                Ok(resp) => {
                    let (channel, _) = inbound_ack_pending.swap_remove(i);
                    let _ = swarm.behaviour_mut().ack_rr.send_response(channel, resp);
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    inbound_ack_pending.swap_remove(i);
                }
            }
        }
        let mut i = 0;
        while i < inbound_heads_pending.len() {
            match inbound_heads_pending[i].1.try_recv() {
                Ok(resp) => {
                    let (channel, _) = inbound_heads_pending.swap_remove(i);
                    let _ = swarm.behaviour_mut().heads_rr.send_response(channel, resp);
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    inbound_heads_pending.swap_remove(i);
                }
            }
        }
        let mut i = 0;
        while i < inbound_range_pending.len() {
            match inbound_range_pending[i].1.try_recv() {
                Ok(resp) => {
                    let (channel, _) = inbound_range_pending.swap_remove(i);
                    let _ = swarm.behaviour_mut().range_rr.send_response(channel, resp);
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    inbound_range_pending.swap_remove(i);
                }
            }
        }
        let mut i = 0;
        while i < inbound_client_pending.len() {
            match inbound_client_pending[i].1.try_recv() {
                Ok(resp) => {
                    let (channel, _) = inbound_client_pending.swap_remove(i);
                    let _ = swarm.behaviour_mut().client_rr.send_response(channel, resp);
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    inbound_client_pending.swap_remove(i);
                }
            }
        }
    }
}

#[async_trait]
impl Broadcaster for LibP2pBroadcaster {
    async fn broadcast_event(&self, event: &Event, min_acks: u32, ack_timeout_ms: u64) -> AckInfo {
        if min_acks > 0 {
            let (tx, rx) = oneshot::channel();
            let _ = self
                .cmd_tx
                .send(SwarmCommand::RequestAcks {
                    event: event.clone(),
                    min_acks,
                    timeout_ms: ack_timeout_ms,
                    reply_tx: tx,
                })
                .await;
            let _ = self
                .cmd_tx
                .send(SwarmCommand::GossipPublish(event.clone()))
                .await;
            rx.await.unwrap_or(AckInfo {
                received: 0,
                requested: min_acks,
                timeout: true,
            })
        } else {
            let _ = self
                .cmd_tx
                .send(SwarmCommand::GossipPublish(event.clone()))
                .await;
            AckInfo::fire_and_forget()
        }
    }

    async fn broadcast_persisted(&self, _keys: &[shardd_types::OriginKey]) {
        // Persistence notifications are best-effort; not implemented in v1.
    }

    async fn peer_count(&self) -> usize {
        self.peer_count.load(Ordering::Relaxed)
    }
}
