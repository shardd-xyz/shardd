use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use reqwest::{Method, header::HeaderMap as ReqwestHeaderMap};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::form_urlencoded::{Serializer, byte_serialize};

use crate::{
    adapters::http::{app_state::AppState, extractors::Authenticated},
    app_error::{AppError, AppResult},
    infra::config::PublicEdgeConfig,
    use_cases::buckets_registry::{BucketStatusFilter, OwnedBucket, validate_bucket_name},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/buckets", get(list_buckets).post(create_bucket))
        .route(
            "/buckets/{bucket}/events",
            get(list_bucket_events).post(create_bucket_event),
        )
        .route(
            "/buckets/{bucket}",
            get(get_bucket_detail).delete(archive_bucket),
        )
        .route(
            "/buckets/{bucket}/purge",
            axum::routing::delete(purge_bucket),
        )
        .route("/events", get(list_events))
        .route("/edges", get(list_edges))
}

#[derive(Debug, Deserialize)]
struct PurgeBucketQuery {
    #[serde(default)]
    confirm: Option<String>,
}

/// §3.5: the user-facing "permanuke". Hard-deletes a bucket cluster-wide
/// by routing a `BucketDelete` meta event through the gateway. Requires:
///   1. Authenticated user session.
///   2. `?confirm={bucket}` query — backend guard against accidental
///      click-through. The UI also enforces a typed-confirmation modal.
///   3. Ownership: the bucket must belong to this user. 404 otherwise
///      so we don't leak existence to non-owners.
///
/// Success returns 204. The cascade is eventually consistent across
/// the cluster as the meta event gossips; the dashboard's own
/// `developer_buckets` row is also archived so the UI hides it
/// immediately.
async fn purge_bucket(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(bucket): Path<String>,
    Query(query): Query<PurgeBucketQuery>,
) -> AppResult<StatusCode> {
    if query.confirm.as_deref() != Some(bucket.as_str()) {
        return Err(AppError::InvalidInput(
            "confirm query must equal the bucket name".into(),
        ));
    }

    // 404 (not 403) so non-owners can't probe bucket existence.
    let exists = state.bucket_registry.exists(user.id, &bucket).await?;
    if !exists {
        return Err(AppError::NotFound);
    }

    // Fire the meta event via the gateway internal route.
    let response = proxy_gateway_value(
        &state,
        Method::POST,
        "/internal/meta/bucket-delete".to_string(),
        Some(json!({
            "bucket": bucket,
            "reason": format!("user:{}", user.id),
        })),
    )
    .await?;
    if !response.status.is_success() {
        return Err(AppError::Internal(format!(
            "gateway rejected bucket-delete: {} {}",
            response.status,
            response
                .payload
                .as_ref()
                .and_then(|v| v.get("error").and_then(|s| s.as_str()))
                .unwrap_or("unknown")
        )));
    }

    // Archive the dashboard-side row so the UI hides the bucket right
    // away, even before the meta event has propagated to every node.
    // If the row is already archived (e.g. double-click) the second
    // archive is a no-op.
    let _ = state.bucket_registry.archive(user.id, &bucket).await?;

    tracing::info!(
        user_id = %user.id,
        bucket = %bucket,
        "user-triggered bucket purge (permanuke) dispatched"
    );

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct CreateBucketRequest {
    name: String,
}

#[derive(Debug, Serialize)]
struct CreatedBucketDto {
    name: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl From<OwnedBucket> for CreatedBucketDto {
    fn from(b: OwnedBucket) -> Self {
        Self {
            name: b.name,
            created_at: b.created_at,
        }
    }
}

async fn create_bucket(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Json(req): Json<CreateBucketRequest>,
) -> AppResult<(StatusCode, Json<CreatedBucketDto>)> {
    let name = req.name.trim();
    validate_bucket_name(name)?;
    let created = state.bucket_registry.create(user.id, name).await?;
    Ok((StatusCode::CREATED, Json(created.into())))
}

async fn archive_bucket(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(bucket): Path<String>,
) -> AppResult<StatusCode> {
    // Archive, not purge. Event history on the mesh is untouched — gaps in
    // per-node seq space would break catch-up, and archiving is the right
    // semantic for a user-initiated "delete" anyway. Can be un-archived
    // later if we add that UI.
    let updated = state.bucket_registry.archive(user.id, &bucket).await?;
    if !updated {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Per-user events viewer. Same query shape as the admin-only
/// `/api/admin/events` route, but proxies to the gateway's per-user
/// endpoint (`/internal/users/{user_id}/events`) — the gateway scopes
/// the listing to this user's bucket namespace, so no dashboard-side
/// ownership check is required.
#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    bucket: Option<String>,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    event_type: Option<String>,
    #[serde(default)]
    since_ms: Option<u64>,
    #[serde(default)]
    until_ms: Option<u64>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    replication: Option<bool>,
}

async fn list_events(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> AppResult<Response> {
    // Same 200-row dashboard cap as the admin endpoint; the node clamps
    // at 500.
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let limit_s = limit.to_string();
    let offset_s = offset.to_string();
    let since_s = q.since_ms.map(|v| v.to_string());
    let until_s = q.until_ms.map(|v| v.to_string());
    let replication_s = match q.replication {
        Some(true) => Some("true".to_string()),
        _ => None,
    };
    let path = path_with_query(
        &format!(
            "/internal/users/{}/events",
            encode_path_segment(&user.id.to_string())
        ),
        &[
            ("bucket", q.bucket.as_deref()),
            ("account", q.account.as_deref()),
            ("origin", q.origin.as_deref()),
            ("event_type", q.event_type.as_deref()),
            ("since_ms", since_s.as_deref()),
            ("until_ms", until_s.as_deref()),
            ("search", q.search.as_deref()),
            ("limit", Some(&limit_s)),
            ("offset", Some(&offset_s)),
            ("replication", replication_s.as_deref()),
        ],
    );
    proxy_gateway_json(&state, Method::GET, path, None).await
}

async fn list_edges(State(state): State<AppState>) -> Json<Vec<EdgeSummary>> {
    Json(
        state
            .config
            .public_edges
            .iter()
            .map(|edge| EdgeSummary {
                edge_id: edge.edge_id.clone(),
                region: edge.region.clone(),
                base_url: edge.base_url.clone(),
                node_id: edge.node_id.clone(),
                label: edge.label.clone(),
                node_label: edge.node_label.clone(),
            })
            .collect(),
    )
}

#[derive(Debug, Serialize)]
struct EdgeSummary {
    edge_id: String,
    region: String,
    base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BucketsQuery {
    q: Option<String>,
    page: Option<usize>,
    limit: Option<usize>,
    #[serde(default)]
    status: Option<String>,
}

fn parse_status_filter(raw: Option<&str>) -> BucketStatusFilter {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("active") => BucketStatusFilter::Active,
        Some("archived") => BucketStatusFilter::Archived,
        Some("nuked") => BucketStatusFilter::Nuked,
        _ => BucketStatusFilter::All,
    }
}

/// Wire representation of a bucket's lifecycle stage. The set is closed:
/// anything not in the dashboard registry simply isn't shown, and mesh
/// rows without a registry entry (legacy data) never reach this path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BucketStatus {
    Active,
    Archived,
    Nuked,
}

/// Gateway's per-bucket mesh summary — a subset of its
/// `InternalBucketSummary` that we actually consume here. Archived
/// buckets still appear here (the mesh keeps their events); nuked
/// buckets do not (the `BucketDelete` cascade wiped them).
#[derive(Debug, Deserialize)]
struct GatewayBucketSummary {
    bucket: String,
    total_balance: i64,
    available_balance: i64,
    account_count: usize,
    event_count: usize,
    #[serde(default)]
    last_event_at_unix_ms: Option<u64>,
}

/// Payload shape returned by `GET /internal/buckets/deleted`.
#[derive(Debug, Deserialize)]
struct GatewayDeletedBucketsResponse {
    #[serde(default)]
    buckets: Vec<GatewayDeletedBucket>,
}

#[derive(Debug, Deserialize)]
struct GatewayDeletedBucket {
    name: String,
    deleted_at_unix_ms: u64,
}

/// What the dashboard UI consumes. Balances/accounts/event_count are
/// `None` for nuked buckets since their mesh data has been purged;
/// `archived_at_unix_ms` is set for archived+nuked; `deleted_at_unix_ms`
/// is only set for nuked.
#[derive(Debug, Serialize)]
struct BucketSummaryDto {
    bucket: String,
    status: BucketStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_balance: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_balance: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_event_at_unix_ms: Option<u64>,
    created_at_unix_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived_at_unix_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct BucketListDto {
    buckets: Vec<BucketSummaryDto>,
    total: usize,
    page: usize,
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct BucketEventsQuery {
    q: Option<String>,
    account: Option<String>,
    page: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateBucketEventRequest {
    account: String,
    amount: i64,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    idempotency_nonce: Option<String>,
    #[serde(default)]
    max_overdraft: Option<u64>,
    #[serde(default)]
    min_acks: Option<u32>,
    #[serde(default)]
    ack_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct EdgeHealth {
    ready: bool,
}

async fn list_buckets(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Query(query): Query<BucketsQuery>,
) -> AppResult<Response> {
    let filter = parse_status_filter(query.status.as_deref());

    // Registry is the authoritative per-user set (including archived
    // rows when the filter allows). For `All`/`Archived`/`Nuked` the SQL
    // returns archived rows; the mesh `deleted_buckets` set splits
    // archived vs nuked below.
    let registry_rows: Vec<OwnedBucket> = state.bucket_registry.list(user.id, filter).await?;

    // Mesh summaries (balances, account/event counts, last activity).
    // Gateway only returns buckets that still exist on the mesh —
    // nuked buckets will simply be absent from this map.
    let summary_path = format!(
        "/internal/users/{}/buckets",
        encode_path_segment(&user.id.to_string())
    );
    let summaries_fut = proxy_gateway_value(&state, Method::GET, summary_path, None);

    // Cluster-wide tombstone set. Used to mark nuked rows and to prevent
    // classifying a still-archived-but-not-nuked bucket as nuked.
    let deleted_fut = proxy_gateway_value(
        &state,
        Method::GET,
        "/internal/buckets/deleted".to_string(),
        None,
    );

    let (summaries_resp, deleted_resp) = tokio::try_join!(summaries_fut, deleted_fut)?;

    let mut summaries: std::collections::HashMap<String, GatewayBucketSummary> =
        std::collections::HashMap::new();
    if let Some(payload) = summaries_resp.payload.as_ref()
        && let Some(Value::Array(list)) = payload.get("buckets")
    {
        for item in list {
            if let Ok(summary) = serde_json::from_value::<GatewayBucketSummary>(item.clone()) {
                summaries.insert(summary.bucket.clone(), summary);
            }
        }
    }

    let mut deleted: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    if let Some(payload) = deleted_resp.payload.as_ref()
        && let Ok(parsed) = serde_json::from_value::<GatewayDeletedBucketsResponse>(payload.clone())
    {
        for entry in parsed.buckets {
            deleted.insert(entry.name, entry.deleted_at_unix_ms);
        }
    }

    // Build the unified list: classify each registry row, attach mesh
    // data when present.
    let mut rows: Vec<BucketSummaryDto> = registry_rows
        .into_iter()
        .map(|bucket| {
            let deleted_at = deleted.get(&bucket.name).copied();
            let status = match (bucket.archived_at.is_some(), deleted_at.is_some()) {
                (_, true) => BucketStatus::Nuked,
                (true, false) => BucketStatus::Archived,
                (false, false) => BucketStatus::Active,
            };
            let summary = if matches!(status, BucketStatus::Nuked) {
                None
            } else {
                summaries.get(&bucket.name)
            };
            BucketSummaryDto {
                bucket: bucket.name,
                status,
                total_balance: summary.map(|s| s.total_balance),
                available_balance: summary.map(|s| s.available_balance),
                account_count: summary.map(|s| s.account_count),
                event_count: summary.map(|s| s.event_count),
                last_event_at_unix_ms: summary.and_then(|s| s.last_event_at_unix_ms),
                created_at_unix_ms: bucket.created_at.timestamp_millis(),
                archived_at_unix_ms: bucket.archived_at.map(|t| t.timestamp_millis()),
                deleted_at_unix_ms: deleted_at,
            }
        })
        .collect();

    // Drop rows that don't match the requested filter — SQL returns
    // `Archived` and `Nuked` together, and `All` should still respect a
    // narrow `status=active` request.
    if filter != BucketStatusFilter::All {
        rows.retain(|row| match filter {
            BucketStatusFilter::Active => row.status == BucketStatus::Active,
            BucketStatusFilter::Archived => row.status == BucketStatus::Archived,
            BucketStatusFilter::Nuked => row.status == BucketStatus::Nuked,
            BucketStatusFilter::All => true,
        });
    }

    if let Some(needle) = query.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let needle_lc = needle.to_ascii_lowercase();
        rows.retain(|row| row.bucket.to_ascii_lowercase().contains(&needle_lc));
    }

    let total = rows.len();
    let limit = query.limit.unwrap_or(25).clamp(1, 200);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1).saturating_mul(limit);
    let buckets = rows.into_iter().skip(offset).take(limit).collect();

    let payload = serde_json::to_value(&BucketListDto {
        buckets,
        total,
        page,
        limit,
    })
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(proxied_response(
        StatusCode::OK,
        summaries_resp.headers,
        payload,
    ))
}

async fn get_bucket_detail(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(bucket): Path<String>,
) -> AppResult<Response> {
    proxy_gateway_json(
        &state,
        Method::GET,
        format!(
            "/internal/users/{}/buckets/{}",
            encode_path_segment(&user.id.to_string()),
            encode_path_segment(&bucket)
        ),
        None,
    )
    .await
}

async fn list_bucket_events(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(bucket): Path<String>,
    Query(query): Query<BucketEventsQuery>,
) -> AppResult<Response> {
    let page = query.page.map(|value| value.to_string());
    let limit = query.limit.map(|value| value.to_string());
    let path = path_with_query(
        &format!(
            "/internal/users/{}/buckets/{}/events",
            encode_path_segment(&user.id.to_string()),
            encode_path_segment(&bucket)
        ),
        &[
            ("q", query.q.as_deref()),
            ("account", query.account.as_deref()),
            ("page", page.as_deref()),
            ("limit", limit.as_deref()),
        ],
    );
    proxy_gateway_json(&state, Method::GET, path, None).await
}

async fn create_bucket_event(
    Authenticated(user): Authenticated,
    State(state): State<AppState>,
    Path(bucket): Path<String>,
    Json(request): Json<CreateBucketEventRequest>,
) -> AppResult<Response> {
    proxy_gateway_json(
        &state,
        Method::POST,
        format!(
            "/internal/users/{}/buckets/{}/events",
            encode_path_segment(&user.id.to_string()),
            encode_path_segment(&bucket)
        ),
        Some(serde_json::to_value(request).expect("event request serializes")),
    )
    .await
}

pub(crate) struct GatewayValueResponse {
    pub status: StatusCode,
    pub headers: ReqwestHeaderMap,
    pub payload: Option<Value>,
}

pub(crate) async fn proxy_gateway_value(
    state: &AppState,
    method: Method,
    path: String,
    body: Option<Value>,
) -> AppResult<GatewayValueResponse> {
    let Some(secret) = state.config.machine_auth_shared_secret.as_ref() else {
        return Err(crate::app_error::AppError::Internal(
            "machine auth is not configured".into(),
        ));
    };

    let edges = prioritized_edges(state).await;
    if edges.is_empty() {
        return Err(crate::app_error::AppError::Internal(
            "no public edges configured".into(),
        ));
    }

    let mut last_error: Option<String> = None;
    for edge in edges {
        let url = format!("{}{}", edge.base_url.trim_end_matches('/'), path);
        let mut request = state
            .edge_http
            .request(method.clone(), url)
            .header("x-machine-auth-secret", secret.expose_secret());
        if let Some(payload) = body.as_ref() {
            request = request.json(payload);
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                let payload = parse_gateway_payload(response).await;
                if status.is_server_error() {
                    last_error = Some(format!("edge {} returned {status}", edge.edge_id));
                    continue;
                }
                return Ok(GatewayValueResponse {
                    status,
                    headers,
                    payload: Some(payload),
                });
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
    }
    Err(crate::app_error::AppError::Internal(
        last_error.unwrap_or_else(|| "no public edges responded".to_string()),
    ))
}

pub(crate) async fn proxy_gateway_json(
    state: &AppState,
    method: Method,
    path: String,
    body: Option<Value>,
) -> AppResult<Response> {
    let Some(secret) = state.config.machine_auth_shared_secret.as_ref() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "bucket explorer is unavailable because machine auth is not configured",
        ));
    };

    let edges = prioritized_edges(state).await;
    if edges.is_empty() {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "bucket explorer is unavailable because no public edges are configured",
        ));
    }

    let mut last_error = None;
    let mut last_server_response = None;

    for edge in edges {
        let url = format!("{}{}", edge.base_url.trim_end_matches('/'), path);
        let mut request = state
            .edge_http
            .request(method.clone(), url)
            .header("x-machine-auth-secret", secret.expose_secret());
        if let Some(payload) = body.as_ref() {
            request = request.json(payload);
        }

        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                let payload = parse_gateway_payload(response).await;
                let proxied = proxied_response(status, headers, payload);
                if status.is_server_error() {
                    last_server_response = Some(proxied);
                    continue;
                }
                return Ok(proxied);
            }
            Err(error) => {
                tracing::warn!(edge_id = %edge.edge_id, region = %edge.region, error = %error, "gateway proxy request failed");
                last_error = Some(error.to_string());
            }
        }
    }

    if let Some(response) = last_server_response {
        return Ok(response);
    }

    Ok(error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &format!(
            "bucket explorer is unavailable: {}",
            last_error.unwrap_or_else(|| "no public edges responded".to_string())
        ),
    ))
}

async fn prioritized_edges(state: &AppState) -> Vec<PublicEdgeConfig> {
    let mut ready = Vec::new();
    let mut fallback = Vec::new();

    for edge in &state.config.public_edges {
        if edge_ready(state, edge).await {
            ready.push(edge.clone());
        } else {
            fallback.push(edge.clone());
        }
    }

    ready.extend(fallback);
    ready
}

async fn edge_ready(state: &AppState, edge: &PublicEdgeConfig) -> bool {
    let url = format!("{}/gateway/health", edge.base_url.trim_end_matches('/'));
    match state.edge_http.get(url).send().await {
        Ok(response) if response.status().is_success() => response
            .json::<EdgeHealth>()
            .await
            .map(|health| health.ready)
            .unwrap_or(false),
        _ => false,
    }
}

async fn parse_gateway_payload(response: reqwest::Response) -> Value {
    let body = response.text().await.unwrap_or_default();
    if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&body).unwrap_or_else(|_| json!({ "error": body }))
    }
}

fn proxied_response(status: StatusCode, headers: ReqwestHeaderMap, payload: Value) -> Response {
    let mut response = (status, Json(payload)).into_response();
    copy_gateway_headers(response.headers_mut(), &headers);
    response
}

fn copy_gateway_headers(target: &mut axum::http::HeaderMap, source: &ReqwestHeaderMap) {
    for (name, value) in source {
        if name.as_str().starts_with("x-shardd-") {
            target.insert(name.clone(), value.clone());
        }
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn encode_path_segment(value: &str) -> String {
    byte_serialize(value.as_bytes()).collect()
}

pub(crate) fn path_with_query(path: &str, pairs: &[(&str, Option<&str>)]) -> String {
    let mut serializer = Serializer::new(String::new());
    for &(key, value) in pairs {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            serializer.append_pair(key, value);
        }
    }
    let query = serializer.finish();
    if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    }
}
