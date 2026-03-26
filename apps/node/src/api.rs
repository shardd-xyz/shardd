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
    let st = state.lock().await;
    Ok(Json(HealthResponse {
        node_id: st.node_id.clone(),
        addr: st.addr.clone(),
        peer_count: st.peers.len(),
        event_count: st.event_count(),
        balance: st.balance(),
    }))
}

// ── GET /state ──

pub async fn get_state(State(state): State<SharedState>) -> Result<Json<StateResponse>> {
    let st = state.lock().await;
    Ok(Json(StateResponse {
        node_id: st.node_id.clone(),
        addr: st.addr.clone(),
        next_seq: st.next_seq,
        peers: st.peers.to_vec(),
        event_count: st.event_count(),
        balance: st.balance(),
        contiguous_heads: st.contiguous_heads.clone(),
        checksum: st.checksum(),
    }))
}

// ── GET /peers ──

pub async fn get_peers(State(state): State<SharedState>) -> Result<Json<Vec<String>>> {
    let st = state.lock().await;
    Ok(Json(st.peers.to_vec()))
}

// ── POST /peers/add ──

pub async fn add_peer(
    State(state): State<SharedState>,
    Json(req): Json<AddPeerRequest>,
) -> Result<Json<serde_json::Value>> {
    let mut st = state.lock().await;
    let added = st.peers.add(&req.addr);
    if added {
        info!(addr = %req.addr, "peer added");
        let _ = st.persist_peers().await;
    }
    Ok(Json(serde_json::json!({ "added": added })))
}

// ── POST /join ──

pub async fn join(
    State(state): State<SharedState>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>> {
    let mut st = state.lock().await;
    let added = st.peers.add(&req.addr);
    if added {
        info!(node_id = %req.node_id, addr = %req.addr, "peer joined");
        let _ = st.persist_peers().await;
    }
    Ok(Json(JoinResponse {
        node_id: st.node_id.clone(),
        addr: st.addr.clone(),
        peers: st.peers.to_vec(),
        heads: st.contiguous_heads.clone(),
    }))
}

// ── POST /events ──

pub async fn create_event(
    State(state): State<SharedState>,
    Json(req): Json<CreateEventRequest>,
) -> Result<Json<CreateEventResponse>> {
    let (event, event_count, balance, fanout);
    {
        let mut st = state.lock().await;
        let e = st
            .create_local_event(req.amount, req.note)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        event_count = st.event_count();
        balance = st.balance();
        fanout = 3;
        event = e;
        info!(
            event_id = %event.event_id,
            seq = event.origin_seq,
            amount = event.amount,
            "local event created"
        );
    }

    // Eager push outside the lock.
    sync::eager_push(&state, &event, fanout).await;

    Ok(Json(CreateEventResponse {
        event,
        event_count,
        balance,
    }))
}

// ── POST /events/replicate ──

pub async fn replicate_event(
    State(state): State<SharedState>,
    Json(event): Json<Event>,
) -> Result<Json<ReplicateResponse>> {
    let inserted;
    {
        let mut st = state.lock().await;
        inserted = st
            .insert_event(event.clone())
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        if inserted {
            debug!(
                event_id = %event.event_id,
                origin = %event.origin_node_id,
                seq = event.origin_seq,
                "replicated event inserted"
            );
        }
    }
    // Cascade: forward new events to our peers
    if inserted {
        sync::eager_push(&state, &event, 3).await;
    }
    Ok(Json(ReplicateResponse {
        status: "ok".into(),
        inserted,
    }))
}

// ── GET /events ──

pub async fn list_events(State(state): State<SharedState>) -> Result<Json<Vec<Event>>> {
    let st = state.lock().await;
    Ok(Json(st.all_events_sorted()))
}

// ── GET /heads ──

pub async fn get_heads(
    State(state): State<SharedState>,
) -> Result<Json<std::collections::BTreeMap<String, u64>>> {
    let st = state.lock().await;
    Ok(Json(st.contiguous_heads.clone()))
}

// ── POST /events/range ──

pub async fn events_range(
    State(state): State<SharedState>,
    Json(req): Json<RangeRequest>,
) -> Result<Json<Vec<Event>>> {
    let st = state.lock().await;
    Ok(Json(
        st.get_events_range(&req.origin_node_id, req.from_seq, req.to_seq),
    ))
}

// ── POST /sync ──

pub async fn trigger_sync(
    State(state): State<SharedState>,
) -> Result<Json<SyncTriggerResponse>> {
    let peers = {
        let st = state.lock().await;
        st.peers.to_vec()
    };
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

        for (origin, peer_head) in &peer_heads {
            let my_head = {
                let st = state.lock().await;
                st.contiguous_heads.get(origin).copied().unwrap_or(0)
            };
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
            let mut st = state.lock().await;
            for event in events {
                if let Ok(true) = st.insert_event(event).await {
                    applied += 1;
                }
            }
        }
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
    let st = state.lock().await;
    let head = st
        .contiguous_heads
        .get(&origin_node_id)
        .copied()
        .unwrap_or(0);
    let (present_seqs, min_seq, max_seq, count) =
        if let Some(seqs) = st.events_by_origin.get(&origin_node_id) {
            let keys: Vec<u64> = seqs.keys().copied().collect();
            let min = keys.first().copied();
            let max = keys.last().copied();
            let count = keys.len();
            (keys, min, max, count)
        } else {
            (vec![], None, None, 0)
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
