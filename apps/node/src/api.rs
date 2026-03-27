use axum::extract::{Path, State};
use axum::Json;
use tracing::{debug, info};

use shardd_types::*;

use crate::error::AppError;
use crate::state::SharedState;
use crate::sync;

type Result<T> = std::result::Result<T, AppError>;

// ── GET /health ──

pub async fn health(State(state): State<SharedState>) -> Result<Json<HealthResponse>> {
    let peer_count = state.peers.lock().await.len();
    Ok(Json(HealthResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        peer_count,
        event_count: state.event_count(),
        balance: state.balance(),
    }))
}

// ── GET /state ──

pub async fn get_state(State(state): State<SharedState>) -> Result<Json<StateResponse>> {
    let peers = state.peers.lock().await.to_vec();
    Ok(Json(StateResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        next_seq: state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
        peers,
        event_count: state.event_count(),
        balance: state.balance(),
        contiguous_heads: state.get_heads(),
        checksum: state.checksum(),
    }))
}

// ── GET /peers ──

pub async fn get_peers(State(state): State<SharedState>) -> Result<Json<Vec<String>>> {
    let peers = state.peers.lock().await;
    Ok(Json(peers.to_vec()))
}

// ── POST /peers/add ──

pub async fn add_peer(
    State(state): State<SharedState>,
    Json(req): Json<AddPeerRequest>,
) -> Result<Json<serde_json::Value>> {
    let added = state.peers.lock().await.add(&req.addr);
    if added {
        info!(addr = %req.addr, "peer added");
        state.persist_peers().await;
    }
    Ok(Json(serde_json::json!({ "added": added })))
}

// ── POST /join ──

pub async fn join(
    State(state): State<SharedState>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>> {
    let (added, peers) = {
        let mut p = state.peers.lock().await;
        let added = p.add(&req.addr);
        (added, p.to_vec())
    };
    if added {
        info!(node_id = %req.node_id, addr = %req.addr, "peer joined");
        state.persist_peers().await;
    }
    Ok(Json(JoinResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        peers,
        heads: state.get_heads(),
    }))
}

// ── POST /events ──

pub async fn create_event(
    State(state): State<SharedState>,
    Json(req): Json<CreateEventRequest>,
) -> Result<Json<CreateEventResponse>> {
    let event = state.create_local_event(req.amount, req.note);

    info!(
        event_id = %event.event_id,
        seq = event.origin_seq,
        amount = event.amount,
        "local event created"
    );

    // Eager push — no locks held
    sync::eager_push(&state, &event).await;

    Ok(Json(CreateEventResponse {
        event,
        event_count: state.event_count(),
        balance: state.balance(),
    }))
}

// ── POST /events/replicate ──

pub async fn replicate_event(
    State(state): State<SharedState>,
    Json(event): Json<Event>,
) -> Result<Json<ReplicateResponse>> {
    let inserted = state.insert_event(event.clone());
    if inserted {
        debug!(
            event_id = %event.event_id,
            origin = %event.origin_node_id,
            seq = event.origin_seq,
            "replicated event inserted"
        );
        sync::eager_push(&state, &event).await;
    }
    Ok(Json(ReplicateResponse {
        status: "ok".into(),
        inserted,
    }))
}

// ── GET /events ──

pub async fn list_events(State(state): State<SharedState>) -> Result<Json<Vec<Event>>> {
    Ok(Json(state.all_events_sorted()))
}

// ── GET /heads ──

pub async fn get_heads(
    State(state): State<SharedState>,
) -> Result<Json<std::collections::BTreeMap<String, u64>>> {
    Ok(Json(state.get_heads()))
}

// ── POST /events/range ──

pub async fn events_range(
    State(state): State<SharedState>,
    Json(req): Json<RangeRequest>,
) -> Result<Json<Vec<Event>>> {
    Ok(Json(
        state.get_events_range(&req.origin_node_id, req.from_seq, req.to_seq),
    ))
}

// ── POST /sync ──

pub async fn trigger_sync(
    State(state): State<SharedState>,
) -> Result<Json<SyncTriggerResponse>> {
    let peers = state.peers.lock().await.to_vec();
    let client = reqwest::Client::new();
    let mut contacted = 0usize;
    let mut applied = 0usize;

    for peer in &peers {
        let base = format!("http://{peer}");
        let heads_resp = client
            .get(format!("{base}/heads"))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
        let Ok(resp) = heads_resp else { continue };
        let Ok(peer_heads) = resp
            .json::<std::collections::BTreeMap<String, u64>>()
            .await
        else {
            continue;
        };
        contacted += 1;

        let local_heads = state.get_heads();
        let mut all_events = Vec::new();

        for (origin, peer_head) in &peer_heads {
            let my_head = local_heads.get(origin).copied().unwrap_or(0);
            if *peer_head <= my_head {
                continue;
            }
            let resp = client
                .post(format!("{base}/events/range"))
                .json(&serde_json::json!({
                    "origin_node_id": origin,
                    "from_seq": my_head + 1,
                    "to_seq": peer_head,
                }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            let Ok(resp) = resp else { continue };
            let Ok(events) = resp.json::<Vec<Event>>().await else {
                continue;
            };
            all_events.extend(events);
        }
        applied += state.insert_events_batch(all_events);
    }

    info!(peers_contacted = contacted, events_applied = applied, "manual sync complete");
    Ok(Json(SyncTriggerResponse {
        status: "ok".into(),
        peers_contacted: contacted,
        events_applied: applied,
    }))
}

// ── GET /debug/origin/:origin_node_id ──

pub async fn debug_origin(
    State(state): State<SharedState>,
    Path(origin_node_id): Path<String>,
) -> Result<Json<DebugOriginResponse>> {
    let (head, present_seqs, min_seq, max_seq, count) =
        if let Some(entry) = state.origins.get(&origin_node_id) {
            let keys: Vec<u64> = entry.events.keys().copied().collect();
            let min = keys.first().copied();
            let max = keys.last().copied();
            (entry.contiguous_head, keys.clone(), min, max, keys.len())
        } else {
            (0, vec![], None, None, 0)
        };
    Ok(Json(DebugOriginResponse {
        origin_node_id,
        contiguous_head: head,
        present_seqs,
        min_seq,
        max_seq,
        count,
    }))
}
