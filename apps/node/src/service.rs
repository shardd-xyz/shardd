use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use shardd_storage::StorageBackend;
use shardd_types::*;
use tracing::info;

use crate::NodePhase;
use crate::app_state::AppState;

pub async fn handle_rpc<S: StorageBackend + Clone>(
    app: &AppState<S>,
    request: NodeRpcRequest,
) -> NodeRpcResult {
    match request {
        NodeRpcRequest::CreateEvent(request) => create_event(app, request)
            .await
            .map(NodeRpcResponse::CreateEvent),
        NodeRpcRequest::Health => Ok(NodeRpcResponse::Health(health(app).await)),
        NodeRpcRequest::State => Ok(NodeRpcResponse::State(state(app).await)),
        NodeRpcRequest::Events => Ok(NodeRpcResponse::Events(events(app).await)),
        NodeRpcRequest::Heads => Ok(NodeRpcResponse::Heads(heads(app))),
        NodeRpcRequest::Balances => Ok(NodeRpcResponse::Balances(balances(app))),
        NodeRpcRequest::Collapsed => Ok(NodeRpcResponse::Collapsed(collapsed(app))),
        NodeRpcRequest::CollapsedAccount { bucket, account } => Ok(
            NodeRpcResponse::CollapsedAccount(collapsed_account(app, &bucket, &account)),
        ),
        NodeRpcRequest::Persistence => Ok(NodeRpcResponse::Persistence(persistence(app))),
        NodeRpcRequest::Digests => Ok(NodeRpcResponse::Digests(digests(app))),
        NodeRpcRequest::DebugOrigin { origin_id } => {
            Ok(NodeRpcResponse::DebugOrigin(debug_origin(app, &origin_id)))
        }
        NodeRpcRequest::Registry => Ok(NodeRpcResponse::Registry(registry(app).await)),
        NodeRpcRequest::DeleteBucket { bucket, reason } => delete_bucket(app, bucket, reason)
            .await
            .map(NodeRpcResponse::DeleteBucket),
        NodeRpcRequest::EventsFilter(request) => events_filter(app, request)
            .await
            .map(NodeRpcResponse::EventsFilter),
        NodeRpcRequest::DeletedBuckets => Ok(NodeRpcResponse::DeletedBuckets(deleted_buckets(app))),
    }
}

pub fn deleted_buckets<S: StorageBackend>(app: &AppState<S>) -> Vec<DeletedBucketEntry> {
    app.shared
        .deleted_buckets
        .iter()
        .map(|entry| DeletedBucketEntry {
            name: entry.key().clone(),
            deleted_at_unix_ms: *entry.value(),
        })
        .collect()
}

/// §3.5: emit a `BucketDelete` meta event for `bucket`. Callers must
/// already have validated authorization — the node trusts the
/// machine-auth layer on the gateway's internal route. The event is
/// broadcast via the usual gossipsub path; every node that receives it
/// applies the cascade.
pub async fn delete_bucket<S: StorageBackend>(
    app: &AppState<S>,
    bucket: String,
    reason: Option<String>,
) -> Result<Event, NodeRpcError> {
    if bucket.is_empty() {
        return Err(NodeRpcError::invalid_input("bucket is required"));
    }
    if shardd_types::is_reserved_bucket_name(&bucket) {
        return Err(NodeRpcError::invalid_input(format!(
            "bucket '{}' is reserved and cannot be deleted",
            bucket
        )));
    }

    let event = match app.shared.create_meta_bucket_delete(&bucket, reason).await {
        Ok(event) => event,
        Err(crate::state::CreateLocalEventError::BucketReserved(name)) => {
            return Err(NodeRpcError::invalid_input(format!(
                "bucket '{}' is reserved",
                name
            )));
        }
        Err(crate::state::CreateLocalEventError::BucketDeleted(name)) => {
            return Err(NodeRpcError::invalid_input(format!(
                "bucket '{}' was already deleted",
                name
            )));
        }
        Err(crate::state::CreateLocalEventError::InsufficientFunds(..)) => {
            return Err(NodeRpcError::internal(
                "meta allocator failed; retry shortly",
            ));
        }
        // The meta-delete path never speaks the reservation grammar, so
        // these are unreachable in practice — surface as 500s rather
        // than guessing at a translation.
        Err(other) => {
            return Err(NodeRpcError::internal(format!(
                "unexpected create_meta_bucket_delete failure: {:?}",
                other
            )));
        }
    };

    // Broadcast the meta event via the normal broadcaster path (§4.1).
    // Fire-and-forget acks — we don't want to block the delete on slow
    // peers; they'll catch up via the standard range-fetch loop.
    app.broadcaster.broadcast_event(&event, 0, 0).await;
    Ok(event)
}

pub async fn create_event<S: StorageBackend>(
    app: &AppState<S>,
    req: CreateEventRequest,
) -> Result<CreateEventResponse, NodeRpcError> {
    shardd_types::validate_event_note(req.note.as_deref()).map_err(NodeRpcError::invalid_input)?;

    // §3.5: clients must never be able to write to reserved buckets
    // (the meta log, billing buckets, etc.). The gateway's internal
    // billing route opts in via `allow_reserved_bucket=true`; that
    // flag isn't deserialized on the public `POST /events` path, so
    // external clients cannot smuggle it in.
    if !req.allow_reserved_bucket && shardd_types::is_reserved_bucket_name(&req.bucket) {
        return Err(NodeRpcError::invalid_input(format!(
            "bucket name '{}' is reserved",
            req.bucket
        )));
    }

    let state = &app.shared;
    let phase = NodePhase::from_u8(state.phase.load(Ordering::Relaxed));
    match phase {
        NodePhase::ShuttingDown => {
            return Err(NodeRpcError::service_unavailable("node shutting down"));
        }
        NodePhase::Warming => {
            // Every write now carries a nonce, so every write is
            // retry-safe — rejecting here lets the client retry after
            // the node finishes warming, and dedupe will make the retry
            // a no-op if the first attempt actually landed.
            return Err(NodeRpcError::service_unavailable("node warming up"));
        }
        NodePhase::Ready => {}
    }

    let max_overdraft = req.max_overdraft.unwrap_or(0);
    let input = crate::state::LocalCreateInput {
        bucket: req.bucket.clone(),
        account: req.account.clone(),
        amount: req.amount,
        note: req.note,
        max_overdraft,
        idempotency_nonce: req.idempotency_nonce.clone(),
        allow_reserved_bucket: req.allow_reserved_bucket,
        hold_amount: req.hold_amount,
        hold_expires_at_unix_ms: req.hold_expires_at_unix_ms,
        settle_reservation: req.settle_reservation,
        release_reservation: req.release_reservation,
        skip_hold: req.skip_hold.unwrap_or(false),
    };
    match state.create_local_events(input).await {
        Ok(result) => {
            let event = result.primary_event.clone();
            let balance = state.account_balance(&event.bucket, &event.account);
            let available = state.account_available_balance(&event.bucket, &event.account);
            let deduplicated = result.emitted_events.is_empty();

            let acks = if !deduplicated {
                let min_acks = req.min_acks.unwrap_or(0);
                let ack_timeout = req.ack_timeout_ms.unwrap_or(500);
                info!(
                    event_id = %event.event_id,
                    seq = event.origin_seq,
                    bucket = %event.bucket,
                    account = %event.account,
                    amount = event.amount,
                    event_type = %event.r#type,
                    emitted_events = result.emitted_events.len(),
                    "event created"
                );
                let mut ack_results = Vec::new();
                for emitted_event in &result.emitted_events {
                    ack_results.push(
                        app.broadcaster
                            .broadcast_event(emitted_event, min_acks, ack_timeout)
                            .await,
                    );
                }
                AckInfo {
                    received: ack_results
                        .iter()
                        .map(|ack| ack.received)
                        .min()
                        .unwrap_or(0),
                    requested: ack_results
                        .iter()
                        .map(|ack| ack.requested)
                        .max()
                        .unwrap_or(0),
                    timeout: ack_results.iter().any(|ack| ack.timeout),
                }
            } else {
                AckInfo::fire_and_forget()
            };

            Ok(CreateEventResponse {
                event,
                emitted_events: result.emitted_events,
                balance,
                available_balance: available,
                deduplicated,
                acks,
            })
        }
        Err(crate::state::CreateLocalEventError::InsufficientFunds(
            balance,
            available,
            projected,
            hold_blocking,
        )) => Err(NodeRpcError::insufficient_funds(InsufficientFundsError {
            error: "insufficient_funds".into(),
            balance,
            available_balance: available,
            projected_available_balance: projected,
            limit: -(max_overdraft as i64),
            hold_blocking,
            hint: if hold_blocking {
                Some(
                    "implicit hold (hold_multiplier × |amount|) pushed projected_available below the floor; \
                     the bare debit math would clear. Retry with `skip_hold: true` for one-shot administrative \
                     writes, or pass an explicit smaller `hold_amount`. See protocol.md §11.4."
                        .into(),
                )
            } else {
                None
            },
        })),
        Err(crate::state::CreateLocalEventError::BucketReserved(name)) => Err(
            NodeRpcError::invalid_input(format!("bucket name '{}' is reserved", name)),
        ),
        Err(crate::state::CreateLocalEventError::BucketDeleted(name)) => {
            Err(NodeRpcError::invalid_input(format!(
                "bucket '{}' was permanently deleted and cannot be written to",
                name
            )))
        }
        Err(crate::state::CreateLocalEventError::InvalidRequest(message)) => {
            Err(NodeRpcError::invalid_input(message))
        }
        Err(crate::state::CreateLocalEventError::ReservationNotFound(id)) => Err(
            NodeRpcError::invalid_input(format!("reservation '{}' not found", id)),
        ),
        Err(crate::state::CreateLocalEventError::ReservationExpired(id)) => Err(
            NodeRpcError::invalid_input(format!("reservation '{}' has expired", id)),
        ),
        Err(crate::state::CreateLocalEventError::ReservationAlreadyReleased(id)) => Err(
            NodeRpcError::invalid_input(format!("reservation '{}' has already been released", id)),
        ),
        Err(crate::state::CreateLocalEventError::ReservationOverspend(reserved, attempted)) => {
            Err(NodeRpcError::invalid_input(format!(
                "settle attempted {} against a reservation of {}",
                attempted, reserved
            )))
        }
    }
}

pub async fn health<S: StorageBackend + Clone>(app: &AppState<S>) -> HealthResponse {
    let state = &app.shared;
    let ready = state.phase.load(Ordering::Relaxed) == NodePhase::Ready as u8;
    let peer_count = app.broadcaster.peer_count().await;
    let known_nodes = state
        .storage
        .load_registry()
        .await
        .map(|registry| registry.len())
        .unwrap_or(peer_count + 1)
        .max(peer_count + 1);
    let metrics = app.metrics.snapshot();
    HealthResponse {
        observed_at_unix_ms: Event::now_ms(),
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        ready,
        peer_count,
        known_nodes,
        sync_gap: state.sync_gap(),
        sync_gap_per_bucket: state.sync_gap_per_bucket(),
        inflight_requests: metrics.inflight_requests,
        completed_requests: metrics.completed_requests,
        failed_requests: metrics.failed_requests,
        overloaded: metrics.overloaded,
        event_count: state.event_count(),
        total_balance: state.total_balance(),
    }
}

pub async fn state<S: StorageBackend>(app: &AppState<S>) -> StateResponse {
    let state = &app.shared;
    StateResponse {
        node_id: state.node_id.to_string(),
        addr: state.addr.to_string(),
        ready: state.phase.load(Ordering::Relaxed) == NodePhase::Ready as u8,
        event_count: state.event_count(),
        total_balance: state.total_balance(),
        contiguous_heads: state.get_heads(),
        checksum: state.storage.checksum_data().await.unwrap_or_default(),
    }
}

pub async fn events<S: StorageBackend>(app: &AppState<S>) -> EventsResponse {
    EventsResponse {
        events: app
            .shared
            .storage
            .query_all_events_sorted()
            .await
            .unwrap_or_default(),
    }
}

/// Paginated + filtered cluster-wide event listing. Hits storage with
/// the filter, pairs the page with this node's current head /
/// max_known snapshots so the admin UI can annotate each row with its
/// replication status. The limit is clamped at `[1, 500]` to bound
/// response size; the dashboard layer clamps tighter (200) on top of
/// this.
pub async fn events_filter<S: StorageBackend>(
    app: &AppState<S>,
    request: EventsFilterRequest,
) -> Result<EventsFilterResponse, NodeRpcError> {
    let limit = request.limit.clamp(1, 500);
    let offset = request.offset;

    let filter = shardd_storage::EventsFilter {
        bucket: request.bucket.clone(),
        bucket_prefix: request.bucket_prefix.clone(),
        account: request.account.clone(),
        origin: request.origin.clone(),
        event_type: request.event_type.clone(),
        since_unix_ms: request.since_unix_ms,
        until_unix_ms: request.until_unix_ms,
        search: request.search.clone(),
    };
    let (events, total) = app
        .shared
        .storage
        .query_events_filtered(&filter, limit, offset)
        .await
        .map_err(|error| NodeRpcError::internal(error.to_string()))?;

    Ok(EventsFilterResponse {
        events,
        total,
        limit,
        offset,
        heads: app.shared.get_heads(),
        max_known_seqs: app.shared.get_max_known_seqs(),
    })
}

pub fn heads<S: StorageBackend>(app: &AppState<S>) -> BTreeMap<String, u64> {
    app.shared.get_heads()
}

pub fn balances<S: StorageBackend>(app: &AppState<S>) -> BalancesResponse {
    BalancesResponse {
        total_balance: app.shared.total_balance(),
        accounts: app.shared.get_all_balances(),
    }
}

pub fn collapsed<S: StorageBackend>(app: &AppState<S>) -> BTreeMap<String, CollapsedBalance> {
    app.shared.collapsed_state()
}

pub fn collapsed_account<S: StorageBackend>(
    app: &AppState<S>,
    bucket: &str,
    account: &str,
) -> CollapsedBalance {
    let collapsed = app.shared.collapsed_state();
    let key = format!("{bucket}:{account}");
    collapsed.get(&key).cloned().unwrap_or(CollapsedBalance {
        balance: 0,
        available_balance: 0,
        status: "unknown".into(),
        reserved_by_origin: BTreeMap::new(),
        contributing_origins: BTreeMap::new(),
    })
}

pub fn persistence<S: StorageBackend>(app: &AppState<S>) -> PersistenceStats {
    app.shared.persistence_stats()
}

pub fn digests<S: StorageBackend>(app: &AppState<S>) -> BTreeMap<String, DigestInfo> {
    app.shared
        .get_digests()
        .into_iter()
        .map(|(k, (head, digest))| (k, DigestInfo { head, digest }))
        .collect()
}

pub fn debug_origin<S: StorageBackend>(app: &AppState<S>, origin_id: &str) -> DebugOriginResponse {
    app.shared.debug_origin(origin_id)
}

pub async fn registry<S: StorageBackend>(app: &AppState<S>) -> Vec<NodeRegistryEntry> {
    app.shared.storage.load_registry().await.unwrap_or_default()
}
