use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use dashmap::DashMap;
use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, Transport,
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade},
    dns, identify, identity, kad, noise, ping,
    pnet::{PnetConfig, PreSharedKey},
    request_response::{self, ProtocolSupport},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::libp2p_broadcaster::{CLIENT_PROTOCOL, MembersRequest, MembersResponse};
use crate::metadata::{PROTOCOL_VERSION, parse_agent_version};
use shardd_types::{
    Event, HealthResponse, NodeRpcError, NodeRpcErrorCode, NodeRpcRequest, NodeRpcResponse,
    NodeRpcResult,
};

#[derive(Debug, Clone)]
pub struct MeshClientConfig {
    pub bootstrap_peers: Vec<Multiaddr>,
    pub psk: Option<[u8; 32]>,
    /// Stable seed used to derive the mesh client's libp2p identity. Giving
    /// the same seed across restarts yields the same PeerId, which keeps
    /// peer caches and Kademlia routing tables valid across redeploys.
    /// Typically set to the gateway's `public_edge_id` (e.g. "use1").
    pub identity_seed: String,
    pub request_timeout: Duration,
    pub health_interval: Duration,
    pub peer_ttl: Duration,
    pub health_ttl: Duration,
    pub cooldown: Duration,
    pub cache_flush_interval: Duration,
    pub cache_path: Option<PathBuf>,
    pub top_k: usize,
    pub max_sync_gap: u64,
}

impl MeshClientConfig {
    pub fn new(bootstrap_peers: Vec<Multiaddr>) -> Self {
        Self {
            bootstrap_peers,
            psk: None,
            identity_seed: String::new(),
            request_timeout: Duration::from_secs(5),
            // How often we kick off a health probe per peer. Each tick
            // re-probes any peer we haven't heard from in this long —
            // this keeps `last_health_at_unix_ms` within ~interval of
            // now, which means `health_ttl` almost never expires in a
            // healthy mesh.
            health_interval: Duration::from_secs(1),
            peer_ttl: Duration::from_secs(20),
            // How long a health observation stays visible. Large enough
            // to absorb a dropped probe or two without flipping
            // `node_is_healthy` to false — readers are protected from
            // the "just sent a probe, waiting for the response" gap.
            health_ttl: Duration::from_secs(15),
            cooldown: Duration::from_secs(2),
            cache_flush_interval: Duration::from_secs(2),
            cache_path: None,
            top_k: 3,
            max_sync_gap: 64,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MeshNode {
    pub node_id: String,
    pub peer_id: String,
    pub advertise_addr: Option<String>,
    pub ping_rtt: Option<Duration>,
    pub listen_addrs: Vec<Multiaddr>,
    pub last_discovered_at_unix_ms: u64,
    pub last_health_at_unix_ms: Option<u64>,
    pub health: Option<HealthResponse>,
    pub failure_count: u32,
    pub cooldown_until_unix_ms: u64,
}

pub struct MeshClient {
    inner: Arc<MeshInner>,
    tasks: Vec<JoinHandle<()>>,
}

struct MeshInner {
    config: MeshClientConfig,
    peers: DashMap<String, PeerRecord>,
    cmd_tx: mpsc::Sender<ClientCommand>,
}

#[derive(Debug, Clone)]
struct PeerRecord {
    node_id: Option<String>,
    peer_id: String,
    advertise_addr: Option<String>,
    ping_rtt: Option<Duration>,
    listen_addrs: Vec<Multiaddr>,
    last_discovered_at_unix_ms: u64,
    last_health_at_unix_ms: Option<u64>,
    health: Option<HealthResponse>,
    failure_count: u32,
    cooldown_until_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedPeerRecord {
    node_id: Option<String>,
    peer_id: String,
    advertise_addr: Option<String>,
    ping_rtt_ms: Option<u64>,
    listen_addrs: Vec<String>,
    last_discovered_at_unix_ms: u64,
    last_health_at_unix_ms: Option<u64>,
    health: Option<HealthResponse>,
    failure_count: u32,
    cooldown_until_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    peers: Vec<CachedPeerRecord>,
}

enum ClientCommand {
    Request {
        peer_id: PeerId,
        request: NodeRpcRequest,
        reply_tx: oneshot::Sender<Result<NodeRpcResult>>,
    },
}

#[derive(NetworkBehaviour)]
struct MeshClientBehaviour {
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    members_rr: request_response::json::Behaviour<MembersRequest, MembersResponse>,
    client_rr: request_response::json::Behaviour<NodeRpcRequest, NodeRpcResult>,
}

impl MeshClient {
    pub fn start(config: MeshClientConfig) -> Result<Self> {
        // PSK is the secret half of the libp2p identity derivation AND the
        // private-mesh pnet key. Every mesh client needs it — no exceptions.
        if config.psk.is_none() {
            bail!(
                "mesh client requires a cluster_key / PSK — pass --cluster-key or \
                 SHARDD_CLUSTER_KEY. This is the secret half of the libp2p identity."
            );
        }
        // identity_seed makes the derived PeerId stable across restarts.
        // That stability is ONLY load-bearing when a persistent peer cache
        // is configured (long-lived gateways). Short-lived clients like the
        // CLI and the bench can safely run with an ephemeral keypair since
        // they have no cached state to invalidate. Enforce the invariant:
        // cache persistence requires stable identity.
        if config.cache_path.is_some() && config.identity_seed.trim().is_empty() {
            bail!(
                "mesh client cache_path is set without identity_seed: cached peer \
                 entries would become invalid on every restart because a random \
                 PeerId would be generated. Either provide identity_seed \
                 (e.g. SHARDD_PUBLIC_EDGE_ID for gateways) or clear cache_path."
            );
        }
        let peers = DashMap::new();
        let cached_bootstrap = load_cache(config.cache_path.as_ref(), &peers);

        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let inner = Arc::new(MeshInner {
            config,
            peers,
            cmd_tx,
        });

        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(mesh_event_loop(
            inner.clone(),
            cmd_rx,
            cached_bootstrap,
        )));
        if inner.config.cache_path.is_some() {
            tasks.push(tokio::spawn(cache_flush_loop(inner.clone())));
        }

        Ok(Self { inner, tasks })
    }

    pub async fn wait_for_min_candidates(
        &self,
        min_candidates: usize,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.all_nodes().len() >= min_candidates {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("timed out waiting for {min_candidates} discovered nodes");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub fn all_nodes(&self) -> Vec<MeshNode> {
        let now_ms = Event::now_ms();
        let mut nodes = self.collect_nodes(now_ms, true);
        if nodes.is_empty() {
            nodes = self.collect_nodes(now_ms, false);
        }
        nodes.sort_by(compare_for_list);
        nodes
    }

    pub fn best_node(&self) -> Option<MeshNode> {
        let now_ms = Event::now_ms();
        let mut candidates = self.collect_nodes(now_ms, true);
        if candidates.is_empty() {
            candidates = self.collect_nodes(now_ms, false);
        }
        if candidates.is_empty() {
            return None;
        }

        candidates.sort_by(compare_for_list);
        let top_k = self.inner.config.top_k.max(1).min(candidates.len());
        let mut shortlist = candidates.into_iter().take(top_k).collect::<Vec<_>>();
        shortlist.sort_by(|left, right| compare_for_selection(left, right, &self.inner.config));
        shortlist.into_iter().next()
    }

    pub async fn request_best(&self, request: NodeRpcRequest) -> Result<NodeRpcResult> {
        let (_, result) = self.request_best_with_node(request).await?;
        Ok(result)
    }

    pub async fn request_best_with_node(
        &self,
        request: NodeRpcRequest,
    ) -> Result<(MeshNode, NodeRpcResult)> {
        let mut candidates = self.all_nodes();
        if candidates.is_empty() {
            bail!("no libp2p nodes discovered");
        }
        candidates.sort_by(|left, right| compare_for_selection(left, right, &self.inner.config));

        let mut last_transport_error = None;
        for node in candidates {
            let peer_id = match node.peer_id.parse::<PeerId>() {
                Ok(peer_id) => peer_id,
                Err(error) => {
                    last_transport_error =
                        Some(anyhow!("invalid peer id {}: {error}", node.peer_id));
                    continue;
                }
            };
            match self.request_to(peer_id, request.clone()).await {
                Ok(result) => match &result {
                    Ok(_) => {
                        self.mark_success(&node.peer_id);
                        return Ok((node, result));
                    }
                    Err(error) if retryable_rpc_error(error) => {
                        self.mark_failure(&node.peer_id);
                    }
                    Err(_) => {
                        self.mark_success(&node.peer_id);
                        return Ok((node, result));
                    }
                },
                Err(error) => {
                    self.mark_failure(&node.peer_id);
                    last_transport_error = Some(error);
                }
            }
        }

        Err(last_transport_error.unwrap_or_else(|| anyhow!("all candidate libp2p nodes failed")))
    }

    pub async fn request_to(
        &self,
        peer_id: PeerId,
        request: NodeRpcRequest,
    ) -> Result<NodeRpcResult> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inner
            .cmd_tx
            .send(ClientCommand::Request {
                peer_id,
                request,
                reply_tx,
            })
            .await
            .context("mesh client command channel closed")?;
        reply_rx.await.context("mesh client request task dropped")?
    }

    fn collect_nodes(&self, now_ms: u64, fresh_only: bool) -> Vec<MeshNode> {
        self.inner
            .peers
            .iter()
            .filter_map(|entry| {
                let record = entry.value();
                let Some(node_id) = &record.node_id else {
                    return None;
                };
                if record.cooldown_until_unix_ms > now_ms {
                    return None;
                }
                if fresh_only
                    && now_ms.saturating_sub(record.last_discovered_at_unix_ms)
                        > duration_ms(self.inner.config.peer_ttl)
                {
                    return None;
                }
                Some(
                    record
                        .clone()
                        .into_node(node_id.clone(), now_ms, &self.inner.config),
                )
            })
            .collect()
    }

    fn mark_failure(&self, peer_id: &str) {
        if let Some(mut peer) = self.inner.peers.get_mut(peer_id) {
            peer.failure_count = peer.failure_count.saturating_add(1);
            peer.cooldown_until_unix_ms = Event::now_ms() + duration_ms(self.inner.config.cooldown);
        }
    }

    fn mark_success(&self, peer_id: &str) {
        if let Some(mut peer) = self.inner.peers.get_mut(peer_id) {
            peer.failure_count = 0;
            peer.cooldown_until_unix_ms = 0;
        }
    }

    pub async fn health(&self) -> Result<NodeRpcResult> {
        self.request_best(NodeRpcRequest::Health).await
    }

    pub async fn state(&self) -> Result<NodeRpcResult> {
        self.request_best(NodeRpcRequest::State).await
    }

    pub async fn create_event(
        &self,
        request: shardd_types::CreateEventRequest,
    ) -> Result<NodeRpcResult> {
        self.request_best(NodeRpcRequest::CreateEvent(request))
            .await
    }
}

impl Drop for MeshClient {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl PeerRecord {
    fn from_cached(record: CachedPeerRecord) -> Option<Self> {
        let mut listen_addrs = Vec::new();
        for addr in record.listen_addrs {
            listen_addrs.push(addr.parse().ok()?);
        }
        Some(Self {
            node_id: record.node_id,
            peer_id: record.peer_id,
            advertise_addr: record.advertise_addr,
            ping_rtt: record.ping_rtt_ms.map(Duration::from_millis),
            listen_addrs,
            last_discovered_at_unix_ms: record.last_discovered_at_unix_ms,
            last_health_at_unix_ms: record.last_health_at_unix_ms,
            health: record.health,
            failure_count: record.failure_count,
            cooldown_until_unix_ms: record.cooldown_until_unix_ms,
        })
    }

    fn to_cached(&self) -> CachedPeerRecord {
        CachedPeerRecord {
            node_id: self.node_id.clone(),
            peer_id: self.peer_id.clone(),
            advertise_addr: self.advertise_addr.clone(),
            ping_rtt_ms: self.ping_rtt.map(|value| value.as_millis() as u64),
            listen_addrs: self.listen_addrs.iter().map(ToString::to_string).collect(),
            last_discovered_at_unix_ms: self.last_discovered_at_unix_ms,
            last_health_at_unix_ms: self.last_health_at_unix_ms,
            health: self.health.clone(),
            failure_count: self.failure_count,
            cooldown_until_unix_ms: self.cooldown_until_unix_ms,
        }
    }

    fn into_node(self, node_id: String, _now_ms: u64, _config: &MeshClientConfig) -> MeshNode {
        // Always surface the last-known health snapshot. Readers that
        // care about staleness use `last_health_at_unix_ms` to apply
        // their own age policy (the gateway's `node_is_healthy` does,
        // gated on `health_ttl`). Clearing here would create a false-
        // unhealthy window every time a probe is in flight.
        MeshNode {
            node_id,
            peer_id: self.peer_id,
            advertise_addr: self.advertise_addr,
            ping_rtt: self.ping_rtt,
            listen_addrs: self.listen_addrs,
            last_discovered_at_unix_ms: self.last_discovered_at_unix_ms,
            last_health_at_unix_ms: self.last_health_at_unix_ms,
            health: self.health,
            failure_count: self.failure_count,
            cooldown_until_unix_ms: self.cooldown_until_unix_ms,
        }
    }
}

pub fn default_cache_path(file_name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut path = PathBuf::from(home);
    path.push(".cache");
    path.push("shardd");
    path.push(file_name);
    Some(path)
}

async fn mesh_event_loop(
    inner: Arc<MeshInner>,
    mut cmd_rx: mpsc::Receiver<ClientCommand>,
    cached_bootstrap: Vec<Multiaddr>,
) {
    // libp2p identity: derive a stable keypair from the mesh PSK + identity_seed
    // when a seed is provided (long-lived gateways with a persistent cache); fall
    // back to an ephemeral random keypair for short-lived clients without a seed.
    // Both paths validated upstream in MeshClient::start.
    let psk = inner
        .config
        .psk
        .expect("psk validated in MeshClient::start");
    let seed = inner.config.identity_seed.trim();
    let keypair = if seed.is_empty() {
        tracing::info!("mesh_client using ephemeral keypair (no identity_seed set)");
        identity::Keypair::generate_ed25519()
    } else {
        match crate::discovery::derive_keypair_from_seed(&psk, seed) {
            Ok(kp) => kp,
            Err(error) => {
                tracing::error!(error = %error, "mesh client keypair derivation failed");
                return;
            }
        }
    };
    let local_peer_id = PeerId::from(keypair.public());
    tracing::info!(peer_id = %local_peer_id, seed = %seed, "mesh_client libp2p identity");
    let noise_config = match noise::Config::new(&keypair) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(error = %error, "mesh client noise config failed");
            return;
        }
    };

    let dns_transport =
        match dns::tokio::Transport::system(tcp::tokio::Transport::new(tcp::Config::default())) {
            Ok(transport) => transport,
            Err(error) => {
                tracing::error!(error = %error, "mesh client dns transport failed");
                return;
            }
        };

    let transport: Boxed<(PeerId, StreamMuxerBox)> = if let Some(psk_bytes) = inner.config.psk {
        let psk = PreSharedKey::new(psk_bytes);
        dns_transport
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
        dns_transport
            .upgrade(upgrade::Version::V1)
            .authenticate(noise_config)
            .multiplex(yamux::Config::default())
            .boxed()
    };

    let kademlia = kad::Behaviour::new(local_peer_id, kad::store::MemoryStore::new(local_peer_id));
    let identify = identify::Behaviour::new(
        identify::Config::new(PROTOCOL_VERSION.to_string(), keypair.public())
            .with_agent_version("shardd-client".into()),
    );
    let ping = ping::Behaviour::new(
        ping::Config::new()
            .with_interval(Duration::from_secs(1))
            .with_timeout(Duration::from_secs(5)),
    );
    let members_rr = request_response::json::Behaviour::<MembersRequest, MembersResponse>::new(
        [(
            libp2p::StreamProtocol::new("/shardd/members/1"),
            ProtocolSupport::Full,
        )],
        request_response::Config::default().with_request_timeout(inner.config.request_timeout),
    );
    let client_rr = request_response::json::Behaviour::<NodeRpcRequest, NodeRpcResult>::new(
        [(
            libp2p::StreamProtocol::new(CLIENT_PROTOCOL),
            ProtocolSupport::Full,
        )],
        request_response::Config::default().with_request_timeout(inner.config.request_timeout),
    );

    let mut swarm = match SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_other_transport(|_| Ok(transport))
    {
        Ok(builder) => match builder.with_behaviour(|_| MeshClientBehaviour {
            kademlia,
            identify,
            ping,
            members_rr,
            client_rr,
        }) {
            Ok(builder) => builder
                .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
                .build(),
            Err(error) => {
                tracing::error!(error = %error, "mesh client behaviour build failed");
                return;
            }
        },
        Err(error) => {
            tracing::error!(error = %error, "mesh client transport build failed");
            return;
        }
    };

    if let Err(error) = swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse().expect("valid listen addr")) {
        tracing::error!(error = %error, "mesh client listen failed");
        return;
    }

    let mut dialed_addrs = HashSet::new();
    for addr in inner
        .config
        .bootstrap_peers
        .iter()
        .chain(cached_bootstrap.iter())
        .cloned()
    {
        maybe_dial(&mut swarm, &mut dialed_addrs, addr);
    }

    let mut pending_rpc: HashMap<
        request_response::OutboundRequestId,
        oneshot::Sender<Result<NodeRpcResult>>,
    > = HashMap::new();
    let mut pending_health: HashMap<request_response::OutboundRequestId, String> = HashMap::new();
    let mut health_tick = tokio::time::interval(inner.config.health_interval);
    health_tick.tick().await;

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    ClientCommand::Request { peer_id, request, reply_tx } => {
                        let request_id = swarm.behaviour_mut().client_rr.send_request(&peer_id, request);
                        pending_rpc.insert(request_id, reply_tx);
                    }
                }
            }
            _ = health_tick.tick() => {
                let now_ms = Event::now_ms();
                prune_stale(&inner, now_ms);
                let peer_ids = inner
                    .peers
                    .iter()
                    .filter_map(|entry| {
                        let record = entry.value();
                        record.node_id.as_ref()?;
                        if now_ms.saturating_sub(record.last_discovered_at_unix_ms)
                            > duration_ms(inner.config.peer_ttl)
                        {
                            return None;
                        }
                        if record.cooldown_until_unix_ms > now_ms {
                            return None;
                        }
                        // Re-probe any peer whose last observation is
                        // older than `health_interval`. This keeps
                        // `last_health_at_unix_ms` fresh, so the
                        // `health_ttl` safety-net in `node_is_healthy`
                        // almost never kicks in during normal operation
                        // and there is no visible gap between
                        // "data stale" and "fresh data arrived".
                        let needs_probe = record
                            .last_health_at_unix_ms
                            .map(|last| now_ms.saturating_sub(last) >= duration_ms(inner.config.health_interval))
                            .unwrap_or(true);
                        if needs_probe {
                            record.peer_id.parse::<PeerId>().ok()
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                for peer_id in peer_ids {
                    let request_id = swarm
                        .behaviour_mut()
                        .client_rr
                        .send_request(&peer_id, NodeRpcRequest::Health);
                    pending_health.insert(request_id, peer_id.to_string());
                }
            }
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::ConnectionEstablished { peer_id, endpoint, num_established, .. } => {
                        swarm.behaviour_mut().kademlia.add_address(&peer_id, endpoint.get_remote_address().clone());
                        tracing::debug!(
                            peer = %peer_id,
                            num_established = num_established.get(),
                            direction = if endpoint.is_dialer() { "dialer" } else { "listener" },
                            remote = %endpoint.get_remote_address(),
                            "mesh_client connection established"
                        );
                        // libp2p opens multiple parallel connections to a peer during
                        // simultaneous dial. Only probe health / fetch members on the
                        // 0→1 transition; duplicate RPCs on soon-to-be-pruned
                        // connections hit OutboundFailure and trigger spurious
                        // cooldowns that hide otherwise-healthy peers from all_nodes().
                        if num_established.get() == 1 {
                            swarm.behaviour_mut().members_rr.send_request(&peer_id, MembersRequest);
                            let request_id = swarm
                                .behaviour_mut()
                                .client_rr
                                .send_request(&peer_id, NodeRpcRequest::Health);
                            pending_health.insert(request_id, peer_id.to_string());
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, num_established, endpoint, cause, .. } => {
                        let direction = if endpoint.is_dialer() { "dialer" } else { "listener" };
                        let cause_str = match &cause {
                            Some(libp2p::swarm::ConnectionError::IO(e)) => format!("io: {e}"),
                            Some(libp2p::swarm::ConnectionError::KeepAliveTimeout) => "keep-alive timeout".to_string(),
                            None => "active-close".to_string(),
                        };
                        tracing::debug!(
                            peer = %peer_id,
                            num_remaining = num_established,
                            direction,
                            cause = %cause_str,
                            "mesh_client connection closed"
                        );
                    }
                    SwarmEvent::OutgoingConnectionError { peer_id, error, connection_id, .. } => {
                        // Debug-level: dial failures are routine during normal
                        // libp2p churn. Bump shardd_broadcast=debug to surface.
                        tracing::debug!(
                            peer = ?peer_id,
                            connection_id = ?connection_id,
                            error = %error,
                            "mesh_client outgoing connection error"
                        );
                    }
                    SwarmEvent::IncomingConnectionError { error, send_back_addr, peer_id, .. } => {
                        tracing::debug!(
                            peer = ?peer_id,
                            from = %send_back_addr,
                            error = %error,
                            "mesh_client incoming connection error"
                        );
                    }
                    SwarmEvent::Behaviour(MeshClientBehaviourEvent::Identify(
                        identify::Event::Received { peer_id, info, .. }
                    )) => {
                        let mut entry = inner
                            .peers
                            .entry(peer_id.to_string())
                            .or_insert_with(|| PeerRecord {
                                node_id: None,
                                peer_id: peer_id.to_string(),
                                advertise_addr: None,
                                ping_rtt: None,
                                listen_addrs: Vec::new(),
                                last_discovered_at_unix_ms: Event::now_ms(),
                                last_health_at_unix_ms: None,
                                health: None,
                                failure_count: 0,
                                cooldown_until_unix_ms: 0,
                            });
                        if let Some(metadata) = parse_agent_version(&info.agent_version) {
                            entry.node_id = Some(metadata.node_id);
                            entry.advertise_addr = metadata.advertise_addr;
                        }
                        entry.last_discovered_at_unix_ms = Event::now_ms();
                        entry.listen_addrs = merge_listen_addrs(&entry.listen_addrs, &info.listen_addrs);
                        for addr in info.listen_addrs {
                            swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                            maybe_dial(&mut swarm, &mut dialed_addrs, addr);
                        }
                    }
                    SwarmEvent::Behaviour(MeshClientBehaviourEvent::Kademlia(
                        kad::Event::RoutingUpdated { peer, addresses, .. }
                    )) => {
                        let now_ms = Event::now_ms();
                        let discovered_addrs = addresses.iter().cloned().collect::<Vec<_>>();
                        let mut entry = inner
                            .peers
                            .entry(peer.to_string())
                            .or_insert_with(|| PeerRecord {
                                node_id: None,
                                peer_id: peer.to_string(),
                                advertise_addr: None,
                                ping_rtt: None,
                                listen_addrs: Vec::new(),
                                last_discovered_at_unix_ms: now_ms,
                                last_health_at_unix_ms: None,
                                health: None,
                                failure_count: 0,
                                cooldown_until_unix_ms: 0,
                            });
                        entry.last_discovered_at_unix_ms = now_ms;
                        entry.listen_addrs = merge_listen_addrs(&entry.listen_addrs, &discovered_addrs);
                        for addr in discovered_addrs {
                            maybe_dial(&mut swarm, &mut dialed_addrs, addr);
                        }
                    }
                    SwarmEvent::Behaviour(MeshClientBehaviourEvent::Ping(event)) => {
                        if let Ok(rtt) = event.result
                            && let Some(mut peer) = inner.peers.get_mut(&event.peer.to_string()) {
                            peer.ping_rtt = Some(rtt);
                            peer.last_discovered_at_unix_ms = Event::now_ms();
                        }
                    }
                    SwarmEvent::Behaviour(MeshClientBehaviourEvent::MembersRr(
                        request_response::Event::Message {
                            message: request_response::Message::Response { response, .. },
                            ..
                        }
                    )) => {
                        for member in response.members {
                            let Ok(peer_id) = member.peer_id.parse::<PeerId>() else {
                                continue;
                            };
                            if peer_id == local_peer_id {
                                continue;
                            }
                            let now_ms = Event::now_ms();
                            let mut entry = inner
                                .peers
                                .entry(peer_id.to_string())
                                .or_insert_with(|| PeerRecord {
                                    node_id: Some(member.node_id.clone()),
                                    peer_id: peer_id.to_string(),
                                    advertise_addr: member.advertise_addr.clone(),
                                    ping_rtt: None,
                                    listen_addrs: Vec::new(),
                                    last_discovered_at_unix_ms: now_ms,
                                    last_health_at_unix_ms: None,
                                    health: None,
                                    failure_count: 0,
                                    cooldown_until_unix_ms: 0,
                                });
                            entry.node_id = Some(member.node_id);
                            entry.advertise_addr = member.advertise_addr;
                            entry.last_discovered_at_unix_ms = now_ms;
                            for raw_addr in member.listen_addrs {
                                let Ok(addr) = raw_addr.parse::<Multiaddr>() else {
                                    continue;
                                };
                                if !entry.listen_addrs.contains(&addr) {
                                    entry.listen_addrs.push(addr.clone());
                                }
                                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                                maybe_dial(&mut swarm, &mut dialed_addrs, addr);
                            }
                        }
                    }
                    SwarmEvent::Behaviour(MeshClientBehaviourEvent::ClientRr(
                        request_response::Event::Message {
                            message:
                                request_response::Message::Response {
                                    request_id,
                                    response,
                                },
                            ..
                        }
                    )) => {
                        if let Some(reply_tx) = pending_rpc.remove(&request_id) {
                            let _ = reply_tx.send(Ok(response));
                            continue;
                        }
                        if let Some(peer_id) = pending_health.remove(&request_id) {
                            if let Ok(NodeRpcResponse::Health(health)) = response {
                                if let Some(mut peer) = inner.peers.get_mut(&peer_id) {
                                    peer.health = Some(health);
                                    peer.last_health_at_unix_ms = Some(Event::now_ms());
                                    peer.cooldown_until_unix_ms = 0;
                                }
                            } else if let Err(error) = response {
                                apply_rpc_error(&inner, &peer_id, &error);
                            }
                        }
                    }
                    SwarmEvent::Behaviour(MeshClientBehaviourEvent::ClientRr(
                        request_response::Event::OutboundFailure { request_id, error, .. }
                    )) => {
                        if let Some(reply_tx) = pending_rpc.remove(&request_id) {
                            let _ = reply_tx.send(Err(anyhow!("client RPC failed: {error}")));
                            continue;
                        }
                        if let Some(peer_id) = pending_health.remove(&request_id) {
                            mark_transport_failure(&inner, &peer_id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn cache_flush_loop(inner: Arc<MeshInner>) {
    loop {
        if let Err(error) = write_cache(&inner) {
            tracing::warn!(error = %error, "failed to flush mesh client cache");
        }
        tokio::time::sleep(inner.config.cache_flush_interval).await;
    }
}

fn load_cache(path: Option<&PathBuf>, peers: &DashMap<String, PeerRecord>) -> Vec<Multiaddr> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(cache) = serde_json::from_slice::<CacheFile>(&bytes) else {
        return Vec::new();
    };

    let mut addrs = Vec::new();
    for cached in cache.peers {
        if let Some(record) = PeerRecord::from_cached(cached) {
            addrs.extend(record.listen_addrs.iter().cloned());
            peers.insert(record.peer_id.clone(), record);
        }
    }
    addrs
}

fn write_cache(inner: &MeshInner) -> Result<()> {
    let Some(path) = &inner.config.cache_path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let cache = CacheFile {
        peers: inner
            .peers
            .iter()
            .map(|entry| entry.value().to_cached())
            .collect(),
    };
    let data = serde_json::to_vec_pretty(&cache)?;
    fs::write(path, data).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn maybe_dial(
    swarm: &mut Swarm<MeshClientBehaviour>,
    dialed_addrs: &mut HashSet<String>,
    addr: Multiaddr,
) {
    if !dialed_addrs.insert(addr.to_string()) {
        return;
    }
    let _ = swarm.dial(addr);
}

fn merge_listen_addrs(existing: &[Multiaddr], incoming: &[Multiaddr]) -> Vec<Multiaddr> {
    let mut merged = existing.to_vec();
    for addr in incoming {
        if !merged.contains(addr) {
            merged.push(addr.clone());
        }
    }
    merged.sort();
    merged
}

fn mark_transport_failure(inner: &MeshInner, peer_id: &str) {
    if let Some(mut peer) = inner.peers.get_mut(peer_id) {
        peer.failure_count = peer.failure_count.saturating_add(1);
        peer.cooldown_until_unix_ms = Event::now_ms() + duration_ms(inner.config.cooldown);
    }
}

fn apply_rpc_error(inner: &MeshInner, peer_id: &str, error: &NodeRpcError) {
    if let Some(mut peer) = inner.peers.get_mut(peer_id) {
        if retryable_rpc_error(error) {
            peer.failure_count = peer.failure_count.saturating_add(1);
            peer.cooldown_until_unix_ms = Event::now_ms() + duration_ms(inner.config.cooldown);
        } else {
            peer.cooldown_until_unix_ms = 0;
        }
    }
}

fn retryable_rpc_error(error: &NodeRpcError) -> bool {
    matches!(
        error.code,
        NodeRpcErrorCode::ServiceUnavailable | NodeRpcErrorCode::Internal
    )
}

fn prune_stale(inner: &MeshInner, now_ms: u64) {
    let stale_after = duration_ms(inner.config.peer_ttl) * 3;
    let stale_peers = inner
        .peers
        .iter()
        .filter_map(|entry| {
            let record = entry.value();
            if now_ms.saturating_sub(record.last_discovered_at_unix_ms) > stale_after {
                Some(entry.key().clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for peer_id in stale_peers {
        inner.peers.remove(&peer_id);
    }
}

fn compare_for_list(left: &MeshNode, right: &MeshNode) -> Ordering {
    left.ping_rtt
        .unwrap_or(Duration::MAX)
        .cmp(&right.ping_rtt.unwrap_or(Duration::MAX))
        .then_with(|| health_rank(left).cmp(&health_rank(right)))
        .then_with(|| left.node_id.cmp(&right.node_id))
}

fn compare_for_selection(left: &MeshNode, right: &MeshNode, config: &MeshClientConfig) -> Ordering {
    let left_ready = preferred_health(left, config);
    let right_ready = preferred_health(right, config);
    right_ready
        .cmp(&left_ready)
        .then_with(|| {
            left.ping_rtt
                .unwrap_or(Duration::MAX)
                .cmp(&right.ping_rtt.unwrap_or(Duration::MAX))
        })
        .then_with(|| health_rank(left).cmp(&health_rank(right)))
        .then_with(|| left.failure_count.cmp(&right.failure_count))
        .then_with(|| left.node_id.cmp(&right.node_id))
}

fn preferred_health(node: &MeshNode, config: &MeshClientConfig) -> bool {
    let Some(health) = node.health.as_ref() else {
        return false;
    };
    if !(health.ready && !health.overloaded && health.sync_gap <= config.max_sync_gap) {
        return false;
    }
    // Reject a snapshot that's older than health_ttl — even a
    // previously-ready node shouldn't stay "preferred" forever if
    // the probe loop has gone silent for it.
    let now = Event::now_ms();
    match node.last_health_at_unix_ms {
        Some(last) => now.saturating_sub(last) <= duration_ms(config.health_ttl),
        None => false,
    }
}

fn health_rank(node: &MeshNode) -> (u8, u8, u64, u64, u64) {
    match &node.health {
        Some(health) => (
            if health.ready { 0 } else { 1 },
            if health.overloaded { 1 } else { 0 },
            health.sync_gap,
            health.inflight_requests,
            health.failed_requests,
        ),
        None => (2, 1, u64::MAX, u64::MAX, u64::MAX),
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(peer_id: &str, ping_ms: u64, ready: bool, overloaded: bool, inflight: u64) -> MeshNode {
        // Use wall-clock "now" so freshness checks in `preferred_health`
        // don't reject the fixture as stale.
        let now = Event::now_ms();
        MeshNode {
            node_id: peer_id.into(),
            peer_id: peer_id.into(),
            advertise_addr: None,
            ping_rtt: Some(Duration::from_millis(ping_ms)),
            listen_addrs: Vec::new(),
            last_discovered_at_unix_ms: now,
            last_health_at_unix_ms: Some(now),
            health: Some(HealthResponse {
                observed_at_unix_ms: now,
                node_id: peer_id.into(),
                addr: peer_id.into(),
                ready,
                peer_count: 3,
                known_nodes: 3,
                sync_gap: 0,
                sync_gap_per_bucket: Default::default(),
                inflight_requests: inflight,
                completed_requests: 0,
                failed_requests: 0,
                overloaded,
                event_count: 0,
                total_balance: 0,
            }),
            failure_count: 0,
            cooldown_until_unix_ms: 0,
        }
    }

    #[test]
    fn healthiest_node_wins_within_top_k_ping() {
        let config = MeshClientConfig::new(Vec::new());
        let fast_busy = node("fast-busy", 5, true, true, 100);
        let slower_healthy = node("slower-healthy", 7, true, false, 1);
        let slowest = node("slowest", 20, true, false, 0);

        let mut candidates = vec![slowest, slower_healthy.clone(), fast_busy];
        candidates.sort_by(compare_for_list);
        let mut shortlist = candidates.into_iter().take(2).collect::<Vec<_>>();
        shortlist.sort_by(|left, right| compare_for_selection(left, right, &config));

        assert_eq!(shortlist[0].peer_id, slower_healthy.peer_id);
    }
}
