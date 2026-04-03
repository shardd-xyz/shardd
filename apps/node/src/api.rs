//! HTTP API per protocol.md v1.7 §7.

use std::sync::Arc;

use axum::extract::{FromRef, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use tracing::info;

use shardd_broadcast::Broadcaster;
use shardd_storage::StorageBackend;
use shardd_types::*;

use crate::state::SharedState;
use crate::NodePhase;

// ── Composite app state ─────────────────────────────────────────────

/// Composite state holding SharedState + Broadcaster for axum handlers.
#[derive(Clone)]
pub struct AppState<S: StorageBackend> {
    pub shared: SharedState<S>,
    pub broadcaster: Arc<dyn Broadcaster>,
}

impl<S: StorageBackend + Clone> FromRef<AppState<S>> for SharedState<S> {
    fn from_ref(app: &AppState<S>) -> Self {
        app.shared.clone()
    }
}

impl<S: StorageBackend + Clone> FromRef<AppState<S>> for Arc<dyn Broadcaster> {
    fn from_ref(app: &AppState<S>) -> Self {
        app.broadcaster.clone()
    }
}

// ── Error response ───────────────────────────────────────────────────

pub struct AppError(StatusCode, String);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

// ── POST /events (§7.1) ─────────────────────────────────────────────

pub async fn create_event<S: StorageBackend>(
    State(app): State<AppState<S>>,
    Json(req): Json<CreateEventRequest>,
) -> Result<impl IntoResponse, axum::response::Response> {
    let state = &app.shared;

    // §13.2: Readiness gate — reject protected traffic when not Ready
    let phase = NodePhase::from_u8(state.phase.load(std::sync::atomic::Ordering::Relaxed));
    match phase {
        NodePhase::ShuttingDown => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "node shutting down"})),
            ).into_response());
        }
        NodePhase::Warming => {
            let is_protected = req.amount < 0 || req.idempotency_nonce.is_some();
            if is_protected {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "node warming up"})),
                ).into_response());
            }
        }
        NodePhase::Ready => {}
    }

    let max_overdraft = req.max_overdraft.unwrap_or(0);

    // §10.3: Check idempotency — if deduplicated, return 200 not 201
    match state
        .create_local_event(
            req.bucket.clone(),
            req.account.clone(),
            req.amount,
            req.note,
            max_overdraft,
            req.idempotency_nonce.clone(),
        )
        .await
    {
        Ok(event) => {
            let balance = state.account_balance(&event.bucket, &event.account);
            let available = state.account_available_balance(&event.bucket, &event.account);
            // §7.1: deduplicated = true if event was returned from cache (not newly created)
            let deduplicated = req.idempotency_nonce.is_some()
                && event.origin_node_id != state.node_id.as_ref();
            // A more accurate check: if the event's seq < our next_seq, it's from cache
            let deduplicated = deduplicated || (req.idempotency_nonce.is_some()
                && event.origin_seq < state.next_seq.load(std::sync::atomic::Ordering::Relaxed));

            let status = if deduplicated { StatusCode::OK } else { StatusCode::CREATED };

            // §4.1 + §12.3: Broadcast event to peers (with optional quorum acks)
            let acks = if !deduplicated {
                let min_acks = req.min_acks.unwrap_or(0);
                let ack_timeout = req.ack_timeout_ms.unwrap_or(500);
                info!(
                    event_id = %event.event_id, seq = event.origin_seq,
                    bucket = %event.bucket, account = %event.account, amount = event.amount,
                    "event created"
                );
                app.broadcaster.broadcast_event(&event, min_acks, ack_timeout).await
            } else {
                AckInfo::fire_and_forget()
            };

            Ok((
                status,
                Json(CreateEventResponse {
                    event,
                    balance,
                    available_balance: available,
                    deduplicated,
                    acks,
                }),
            ))
        }
        Err((balance, available, projected)) => {
            let limit = -(max_overdraft as i64);
            Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(InsufficientFundsError {
                    error: "insufficient_funds".into(),
                    balance,
                    available_balance: available,
                    projected_available_balance: projected,
                    limit,
                }),
            ).into_response())
        }
    }
}

// ── POST /events/replicate (§7.2) ───────────────────────────────────

pub async fn replicate_event<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(event): Json<Event>,
) -> Json<ReplicateResponse> {
    let inserted = state.insert_event(&event).await;
    Json(ReplicateResponse { status: "ok".into(), inserted })
}

// ── GET /events (§7.1) ──────────────────────────────────────────────

pub async fn list_events<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<serde_json::Value> {
    let events = state.storage.query_all_events_sorted().await.unwrap_or_default();
    Json(serde_json::json!({"events": events}))
}

// ── GET /heads (§7.1) ───────────────────────────────────────────────

pub async fn get_heads<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<std::collections::BTreeMap<String, u64>> {
    Json(state.get_heads())
}

// ── POST /events/range (§7.2) ───────────────────────────────────────

pub async fn events_range<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(req): Json<RangeRequest>,
) -> Json<Vec<Event>> {
    // Check event_buffer first, then storage
    let mut events = state.get_events_from_buffer(&req.origin_node_id, req.origin_epoch, req.from_seq, req.to_seq);
    if events.len() < (req.to_seq - req.from_seq + 1) as usize {
        let stored = state.storage.query_events_range(&req.origin_node_id, req.origin_epoch, req.from_seq, req.to_seq).await.unwrap_or_default();
        let buffered_seqs: std::collections::HashSet<u64> = events.iter().map(|e| e.origin_seq).collect();
        for e in stored {
            if !buffered_seqs.contains(&e.origin_seq) {
                events.push(e);
            }
        }
        events.sort_by_key(|e| e.origin_seq);
    }
    Json(events)
}

// ── GET /health (§7.1) ──────────────────────────────────────────────

pub async fn health<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<HealthResponse> {
    let ready = state.phase.load(std::sync::atomic::Ordering::Relaxed) == 1; // 1 = Ready
    Json(HealthResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        current_epoch: state.current_epoch,
        ready,
        peer_count: 0,
        event_count: state.event_count(),
        total_balance: state.total_balance(),
    })
}

// ── GET /state (§7.1) ───────────────────────────────────────────────

pub async fn get_state<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<StateResponse> {
    let ready = state.phase.load(std::sync::atomic::Ordering::Relaxed) == 1;
    Json(StateResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        current_epoch: state.current_epoch,
        next_seq: state.next_seq.load(std::sync::atomic::Ordering::Relaxed),
        ready,
        peers: vec![],
        event_count: state.event_count(),
        total_balance: state.total_balance(),
        contiguous_heads: state.get_heads(),
        checksum: state.storage.checksum_data().await.unwrap_or_default(),
    })
}

// ── GET /balances (§7.1) ────────────────────────────────────────────

pub async fn get_balances<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<BalancesResponse> {
    Json(BalancesResponse {
        total_balance: state.total_balance(),
        accounts: state.get_all_balances(),
    })
}

// ── GET /collapsed (§7.1) ───────────────────────────────────────────

pub async fn get_collapsed<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<std::collections::BTreeMap<String, CollapsedBalance>> {
    Json(state.collapsed_state())
}

pub async fn get_collapsed_account<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Path((bucket, account)): Path<(String, String)>,
) -> Json<CollapsedBalance> {
    let collapsed = state.collapsed_state();
    let key = format!("{bucket}:{account}");
    Json(collapsed.get(&key).cloned().unwrap_or(CollapsedBalance {
        balance: 0, available_balance: 0, status: "unknown".into(),
        contributing_origins: std::collections::BTreeMap::new(),
    }))
}

// ── GET /persistence (§7.1) ─────────────────────────────────────────

pub async fn get_persistence<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<PersistenceStats> {
    Json(state.persistence_stats())
}

// ── POST /join (§7.2) ───────────────────────────────────────────────

pub async fn join<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(req): Json<JoinRequest>,
) -> Json<JoinResponse> {
    // §7.2: Register the joining node in our registry
    let now_ms = shardd_types::Event::now_ms();
    let _ = state.storage.upsert_registry_entry(&NodeRegistryEntry {
        node_id: req.node_id.clone(),
        addr: req.addr.clone(),
        first_seen_at_unix_ms: now_ms,
        last_seen_at_unix_ms: now_ms,
        status: shardd_types::NodeStatus::Active,
    }).await;

    // §7.2: Return full registry + heads
    let registry = state.storage.load_registry().await.unwrap_or_default();
    Json(JoinResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        registry,
        heads: state.get_heads(),
    })
}

// ── GET /registry (§7.2) ────────────────────────────────────────────

pub async fn get_registry<S: StorageBackend>(
    State(state): State<SharedState<S>>,
) -> Json<Vec<NodeRegistryEntry>> {
    Json(state.storage.load_registry().await.unwrap_or_default())
}

// ── POST /registry/decommission (§7.2) ──────────────────────────────

#[derive(serde::Deserialize)]
pub struct DecommissionRequest {
    pub node_id: String,
}

pub async fn decommission<S: StorageBackend>(
    State(state): State<SharedState<S>>,
    Json(req): Json<DecommissionRequest>,
) -> Json<serde_json::Value> {
    let _ = state.storage.decommission_node(&req.node_id).await;
    Json(serde_json::json!({"status": "decommissioned", "node_id": req.node_id}))
}

// ── Match all for 404 ───────────────────────────────────────────────

pub async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not_found"})))
}
