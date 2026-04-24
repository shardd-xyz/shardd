//! Node server lifecycle — extracted for testability.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tracing::{info, warn};

use shardd_broadcast::Broadcaster;
use shardd_broadcast::discovery::derive_psk_from_cluster_key;
use shardd_broadcast::libp2p_broadcaster::{
    HeadsResponse, LibP2pBroadcaster, LibP2pConfig, MembershipEvent as LibP2pMembershipEvent,
    RangeResponse,
};
use shardd_storage::StorageBackend;
use shardd_storage::postgres::PostgresStorage;
use shardd_types::{Event, NodeMeta, NodeRpcErrorCode};

use crate::{app_state, batch_writer, orphan_detector, service, state};

pub struct NodeConfig {
    pub host: String,
    /// Multiaddrs advertised to peers via libp2p external-address manager. Empty
    /// means fall back to the listen addr. First entry is the "primary" — used
    /// wherever a singular address is required (node registry, health headers).
    pub advertise_addrs: Vec<String>,
    pub database_url: String,
    /// libp2p bootstrap peer multiaddrs (e.g., /ip4/1.2.3.4/tcp/9000).
    pub bootstrap: Vec<String>,
    pub batch_flush_interval_ms: u64,
    pub batch_flush_size: usize,
    pub matview_refresh_ms: u64,
    pub orphan_check_interval_ms: u64,
    pub orphan_age_ms: u64,
    pub hold_multiplier: u64,
    pub hold_duration_ms: u64,
    /// libp2p TCP port for the node's mesh listener.
    pub libp2p_port: u16,
    /// Path to 32-byte PSK file for libp2p private mesh encryption.
    pub psk_file: Option<String>,
    /// Arbitrary shared cluster key. Derived into the mesh PSK.
    pub cluster_key: Option<String>,
    /// Number of parallel workers draining gossipsub events (JSON decode + state.insert_event).
    pub event_worker_count: usize,
}

pub struct NodeHandle {
    libp2p_addr: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl NodeHandle {
    pub fn libp2p_addr(&self) -> &str {
        &self.libp2p_addr
    }

    pub fn shutdown(mut self) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.join_handle
    }
}

pub async fn start_node(config: NodeConfig) -> anyhow::Result<NodeHandle> {
    let libp2p_addr = format!("/ip4/{}/tcp/{}", config.host, config.libp2p_port);
    let advertise_addrs: Vec<String> = if config.advertise_addrs.is_empty() {
        vec![libp2p_addr.clone()]
    } else {
        config.advertise_addrs.clone()
    };
    // Primary addr is what we pass wherever the API is still singular (node
    // registry entries, Identify agent_version, log output). Additional addrs
    // ride along as libp2p external addresses and show up in each peer's
    // Identify listen_addrs.
    let advertise_addr = advertise_addrs[0].clone();
    let listen_addr: SocketAddr = format!("{}:{}", config.host, config.libp2p_port)
        .parse()
        .unwrap_or_else(|_| format!("0.0.0.0:{}", config.libp2p_port).parse().unwrap());

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await?;
    let storage = Arc::new(PostgresStorage::new(pool));
    storage.run_migrations().await?;
    info!("database connected, migrations applied");

    let node_id = {
        let rows = sqlx::query_as::<_, (String,)>("SELECT node_id FROM node_meta LIMIT 1")
            .fetch_optional(storage.pool())
            .await?;
        match rows {
            Some((id,)) => {
                info!(node_id = %id, "loaded existing node");
                id
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                storage
                    .save_node_meta(&NodeMeta {
                        node_id: id.clone(),
                        host: config.host.clone(),
                        port: config.libp2p_port,
                    })
                    .await?;
                info!(node_id = %id, "created new node");
                id
            }
        }
    };

    // §13.1: epochs are now per-`(bucket, node_id)`; the node itself no
    // longer has a single epoch that bumps on restart. `SharedState::new`
    // flags every existing `bucket_seq_allocator` row `needs_bump=TRUE`,
    // and the first write to each bucket after startup handles the bump.
    let (batch_tx, batch_rx) = tokio::sync::mpsc::unbounded_channel();
    let (correction_tx, mut correction_rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = state::SharedState::new(
        node_id.clone(),
        advertise_addr.clone(),
        (*storage).clone(),
        batch_tx,
        correction_tx,
        state::HoldConfig {
            multiplier: config.hold_multiplier,
            duration_ms: config.hold_duration_ms,
        },
    )
    .await;
    info!(events = shared.event_count(), "state rebuilt from database");

    let bootstrap_peers: Vec<shardd_broadcast::libp2p_crate::Multiaddr> = config
        .bootstrap
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    let psk = if let Some(ref cluster_key) = config.cluster_key {
        match derive_psk_from_cluster_key(cluster_key) {
            Ok(key) => Some(key),
            Err(error) => {
                warn!(error = %error, "failed to derive PSK from cluster key, proceeding without PSK");
                None
            }
        }
    } else if let Some(ref path) = config.psk_file {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() >= 32 => {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes[..32]);
                Some(key)
            }
            _ => {
                warn!(path, "failed to load PSK file, proceeding without PSK");
                None
            }
        }
    } else {
        None
    };

    let libp2p_config = LibP2pConfig {
        node_id: node_id.clone(),
        // Informational only — identity metadata on libp2p Identify. Node
        // no longer has a single "current epoch"; keep 0 as a placeholder.
        epoch: 0,
        advertise_addrs: advertise_addrs.clone(),
        listen_addr,
        bootstrap_peers,
        psk,
    };

    let (lp2p, channels) = LibP2pBroadcaster::start(libp2p_config).await?;
    let lp2p = Arc::new(lp2p);
    let broadcaster: Arc<dyn Broadcaster> = lp2p.clone();

    let mut bg_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    let bridge_storage = storage.clone();
    let bridge_node_id = node_id.clone();
    let mut membership_rx = channels.membership_rx;
    bg_tasks.push(tokio::spawn(async move {
        while let Some(event) = membership_rx.recv().await {
            let (nid, addr, status) = match &event {
                LibP2pMembershipEvent::Up {
                    node_id,
                    addr,
                    advertise_addr,
                    ..
                } => (
                    node_id.clone(),
                    advertise_addr.clone().unwrap_or_else(|| addr.to_string()),
                    shardd_types::NodeStatus::Active,
                ),
                LibP2pMembershipEvent::Down { node_id, .. } => (
                    node_id.clone(),
                    String::new(),
                    shardd_types::NodeStatus::Unreachable,
                ),
            };
            if nid == bridge_node_id {
                continue;
            }
            let now_ms = Event::now_ms();
            let _ = bridge_storage
                .upsert_registry_entry(&shardd_types::NodeRegistryEntry {
                    node_id: nid,
                    addr,
                    first_seen_at_unix_ms: now_ms,
                    last_seen_at_unix_ms: now_ms,
                    status,
                })
                .await;
        }
    }));

    let workers = config.event_worker_count.max(1);
    let event_rx_shared = Arc::new(tokio::sync::Mutex::new(channels.event_rx));
    for worker_id in 0..workers {
        let rx = event_rx_shared.clone();
        let state = shared.clone();
        bg_tasks.push(tokio::spawn(async move {
            loop {
                let data = { rx.lock().await.recv().await };
                match data {
                    Some(bytes) => match serde_json::from_slice::<shardd_types::Event>(&bytes) {
                        Ok(event) => {
                            state.insert_event(&event).await;
                        }
                        Err(e) => {
                            tracing::warn!(worker = worker_id, error = %e, "failed to decode gossipsub event")
                        }
                    },
                    None => break,
                }
            }
        }));
    }
    info!(workers, "gossipsub event consumer pool started");

    let ack_state = shared.clone();
    let mut incoming_ack_rx = channels.incoming_ack_rx;
    bg_tasks.push(tokio::spawn(async move {
        while let Some(req) = incoming_ack_rx.recv().await {
            let inserted = if ack_state.insert_event(&req.event).await {
                true
            } else {
                ack_state.event_is_present(&req.event)
            };
            let _ = req
                .response_tx
                .send(shardd_broadcast::libp2p_broadcaster::AckResponse { inserted });
        }
    }));

    let heads_state = shared.clone();
    let mut incoming_heads_rx = channels.incoming_heads_rx;
    bg_tasks.push(tokio::spawn(async move {
        while let Some(req) = incoming_heads_rx.recv().await {
            let heads = heads_state.get_heads();
            let _ = req.response_tx.send(HeadsResponse { heads });
        }
    }));

    let range_state = shared.clone();
    let mut incoming_range_rx = channels.incoming_range_rx;
    bg_tasks.push(tokio::spawn(async move {
        while let Some(req) = incoming_range_rx.recv().await {
            let r = &req.request;
            let mut events = range_state.get_events_from_buffer(
                &r.bucket,
                &r.origin_node_id,
                r.origin_epoch,
                r.from_seq,
                r.to_seq,
            );
            let expected = (r.to_seq - r.from_seq + 1) as usize;
            if events.len() < expected
                && let Ok(stored) = range_state
                    .storage
                    .query_events_range(
                        &r.bucket,
                        &r.origin_node_id,
                        r.origin_epoch,
                        r.from_seq,
                        r.to_seq,
                    )
                    .await
            {
                let have: HashSet<u64> = events.iter().map(|e| e.origin_seq).collect();
                for e in stored {
                    if !have.contains(&e.origin_seq) {
                        events.push(e);
                    }
                }
                events.sort_by_key(|e| e.origin_seq);
            }
            let _ = req.response_tx.send(RangeResponse { events });
        }
    }));

    let mut incoming_client_rx = channels.incoming_client_rx;

    let catchup_lp2p = lp2p.clone();
    let catchup_state = shared.clone();
    bg_tasks.push(tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            let peers = catchup_lp2p.connected_peers().await;
            if peers.is_empty() {
                continue;
            }

            for peer in peers {
                let Some(heads_resp) = catchup_lp2p.query_heads(peer).await else {
                    continue;
                };

                let local_heads = catchup_state.get_heads();
                for (key, peer_head) in heads_resp.heads {
                    let my_head = local_heads.get(&key).copied().unwrap_or(0);
                    if peer_head <= my_head {
                        continue;
                    }

                    // Wire key is "{bucket}\t{origin}:{epoch}" per v2.
                    let Some((bucket, origin, epoch)) =
                        shardd_broadcast::libp2p_broadcaster::decode_head_key(&key)
                    else {
                        continue;
                    };

                    let from_seq = my_head + 1;
                    let to_seq = (my_head + 5000).min(peer_head);

                    let range_req = shardd_broadcast::libp2p_broadcaster::RangeRequest {
                        bucket,
                        origin_node_id: origin,
                        origin_epoch: epoch,
                        from_seq,
                        to_seq,
                    };
                    if let Some(resp) = catchup_lp2p.query_range(peer, range_req).await {
                        let n = resp.events.len();
                        if n > 0 {
                            catchup_state.insert_events_batch(&resp.events).await;
                            tracing::debug!(peer = %peer, events = n, "catch-up fetched");
                        }
                    }
                }
            }
        }
    }));

    info!(listen_addr = %listen_addr, peer_id = %lp2p.peer_id(), "libp2p broadcaster activated");

    let app_state = app_state::AppState {
        shared: shared.clone(),
        broadcaster: broadcaster.clone(),
        metrics: Arc::new(app_state::RequestMetrics::default()),
    };
    let rpc_app = app_state.clone();
    bg_tasks.push(tokio::spawn(async move {
        while let Some(req) = incoming_client_rx.recv().await {
            rpc_app.metrics.on_request_start();
            let response = service::handle_rpc(&rpc_app, req.request).await;
            let failed = matches!(
                &response,
                Err(error)
                    if matches!(
                        error.code,
                        NodeRpcErrorCode::ServiceUnavailable | NodeRpcErrorCode::Internal
                    )
            );
            rpc_app.metrics.on_request_finish(failed);
            let _ = req.response_tx.send(response);
        }
    }));

    let mut tasks = JoinSet::new();
    for handle in bg_tasks {
        tasks.spawn(async move {
            handle.await.ok();
        });
    }

    let persist_state = shared.clone();
    let persist_broadcaster = broadcaster.clone();
    let bw = batch_writer::BatchWriter::new(
        batch_rx,
        storage.clone(),
        config.batch_flush_interval_ms,
        config.batch_flush_size,
        config.matview_refresh_ms,
    )
    .with_on_persisted(move |keys: &[shardd_types::OriginKey]| {
        persist_state.mark_persisted(keys);
        let bc = persist_broadcaster.clone();
        let keys = keys.to_vec();
        tokio::spawn(async move {
            bc.broadcast_persisted(&keys).await;
        });
    });
    tasks.spawn(bw.run());

    let shared_for_orphan: Arc<dyn orphan_detector::UnpersistedSource> = Arc::new(shared.clone());
    let od = orphan_detector::OrphanDetector::new(
        shared_for_orphan,
        storage.clone(),
        config.orphan_check_interval_ms,
        config.orphan_age_ms,
    );
    tasks.spawn(od.run());

    let sweep_state = shared.clone();
    tasks.spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        interval.tick().await;
        loop {
            interval.tick().await;
            sweep_state.sweep_expired_holds().await;
        }
    });

    let correction_broadcaster = broadcaster.clone();
    tasks.spawn(async move {
        while let Some(event) = correction_rx.recv().await {
            correction_broadcaster.broadcast_event(&event, 0, 0).await;
        }
    });

    let now_ms = Event::now_ms();
    let _ = storage
        .upsert_registry_entry(&shardd_types::NodeRegistryEntry {
            node_id: node_id.clone(),
            addr: advertise_addr.clone(),
            first_seen_at_unix_ms: now_ms,
            last_seen_at_unix_ms: now_ms,
            status: shardd_types::NodeStatus::Active,
        })
        .await;

    if !config.bootstrap.is_empty() {
        info!(
            peers = config.bootstrap.len(),
            "waiting for libp2p peer connections"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }

    shared.phase.store(1, Ordering::Relaxed);
    info!("node ready");

    let shutdown_phase = shared.phase.clone();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    info!(
        listen = %listen_addr,
        advertise = %advertise_addrs.join(","),
        "starting shardd-node v2"
    );
    let join_handle = tokio::spawn(async move {
        let _ = shutdown_rx.await;
        info!("shutdown signal received");
        shutdown_phase.store(2, Ordering::Relaxed);
        drain_background_tasks(tasks).await;
        Ok(())
    });

    Ok(NodeHandle {
        libp2p_addr,
        shutdown_tx: Some(shutdown_tx),
        join_handle,
    })
}

async fn drain_background_tasks(mut tasks: JoinSet<()>) {
    info!("server stopped, waiting for batch writer flush...");
    let flush_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        tokio::select! {
            result = tasks.join_next() => {
                match result {
                    Some(Ok(())) => info!("background task exited cleanly"),
                    Some(Err(error)) => warn!(error = %error, "background task failed"),
                    None => break,
                }
            }
            _ = tokio::time::sleep_until(flush_deadline) => {
                info!("flush deadline reached, aborting remaining tasks");
                tasks.shutdown().await;
                break;
            }
        }
    }
    info!("all tasks drained, goodbye");
}
