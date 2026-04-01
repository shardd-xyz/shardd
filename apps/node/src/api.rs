use axum::extract::{Path, State};
use axum::Json;
use tracing::{debug, info};

use shardd_storage::StorageBackend;
use shardd_types::*;

use crate::error::AppError;
use crate::state::{CollapsedState, PersistenceStats, SharedState};

type Result<T> = std::result::Result<T, AppError>;

// ── GET /health ──

pub async fn health<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<HealthResponse>> {
    let peer_count = state.peers.lock().await.len();
    Ok(Json(HealthResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        peer_count,
        event_count: state.event_count(),
        total_balance: state.total_balance(),
    }))
}

// ── GET /state ──

pub async fn get_state<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<StateResponse>> {
    let peers = state.peers.lock().await.to_vec();
    Ok(Json(StateResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        next_seq: state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
        peers,
        event_count: state.event_count(),
        total_balance: state.total_balance(),
        contiguous_heads: state.get_heads(),
        checksum: state.checksum().await,
    }))
}

// ── GET /peers ──

pub async fn get_peers<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<Vec<String>>> {
    Ok(Json(state.peers.lock().await.to_vec()))
}

// ── POST /peers/add ──

pub async fn add_peer<S: StorageBackend>(
    State(state): State<SharedState<S>>,
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

pub async fn join<S: StorageBackend>(
    State(state): State<SharedState<S>>,
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

pub async fn create_event<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(req): Json<CreateEventRequest>,
) -> Result<Json<CreateEventResponse>> {
    let max_overdraft = req.max_overdraft;
    let min_acks = req.min_acks.unwrap_or(0);
    let ack_timeout = req.ack_timeout_ms.unwrap_or(500);

    let (event, ack_info) = state
        .create_local_event(
            req.bucket, req.account, req.amount, req.note,
            max_overdraft, min_acks, ack_timeout,
        )
        .await
        .map_err(|(balance, projected)| {
            let limit = -(max_overdraft.unwrap_or(0) as i64);
            AppError::InsufficientFunds { balance, projected_balance: projected, limit }
        })?;

    let balance = state.account_balance(&event.bucket, &event.account);

    info!(
        event_id = %event.event_id, seq = event.origin_seq,
        bucket = %event.bucket, account = %event.account, amount = event.amount,
        "local event created"
    );

    Ok(Json(CreateEventResponse {
        event,
        event_count: state.event_count(),
        balance,
        acks: AckInfo {
            received: ack_info.received,
            requested: ack_info.requested,
            timeout: ack_info.timeout,
        },
    }))
}

// ── POST /events/replicate ──

pub async fn replicate_event<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(event): Json<Event>,
) -> Result<Json<ReplicateResponse>> {
    let inserted = state.insert_event(event.clone());
    if inserted {
        debug!(
            event_id = %event.event_id, origin = %event.origin_node_id, seq = event.origin_seq,
            "replicated event inserted"
        );
    }
    Ok(Json(ReplicateResponse { status: "ok".into(), inserted }))
}

// ── GET /events ──

pub async fn list_events<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<Vec<Event>>> {
    Ok(Json(state.all_events_sorted().await))
}

// ── GET /heads ──

pub async fn get_heads<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<std::collections::BTreeMap<String, u64>>> {
    Ok(Json(state.get_heads()))
}

// ── POST /events/range ──

pub async fn events_range<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(req): Json<RangeRequest>,
) -> Result<Json<Vec<Event>>> {
    Ok(Json(state.get_events_range(&req.origin_node_id, req.from_seq, req.to_seq).await))
}

// ── GET /balances ──

pub async fn get_balances<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<BalancesResponse>> {
    Ok(Json(BalancesResponse {
        accounts: state.all_balances(),
        total_balance: state.total_balance(),
    }))
}

// ── GET /debug/origin/:origin_node_id ──

pub async fn debug_origin<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Path(origin_node_id): Path<String>,
) -> Result<Json<DebugOriginResponse>> {
    let (head, present_seqs, min_seq, max_seq, count) = state.debug_origin(&origin_node_id).await;
    Ok(Json(DebugOriginResponse { origin_node_id, contiguous_head: head, present_seqs, min_seq, max_seq, count }))
}

// ── GET /collapsed ──

pub async fn get_collapsed<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<CollapsedState>> {
    Ok(Json(state.collapsed_state()))
}

// ── GET /collapsed/:bucket/:account ──

pub async fn get_collapsed_account<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Path((bucket, account)): Path<(String, String)>,
) -> Result<Json<crate::state::CollapsedBalance>> {
    Ok(Json(state.collapsed_balance(&bucket, &account)))
}

// ── GET /persistence ──

pub async fn get_persistence<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Result<Json<PersistenceStats>> {
    Ok(Json(state.persistence_stats()))
}
