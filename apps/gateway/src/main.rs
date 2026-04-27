use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, serve};
use clap::Parser;
use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shardd_broadcast::discovery::{
    derive_psk_from_cluster_key, load_psk_file, parse_bootstrap_peers,
};
use shardd_broadcast::mesh_client::{MeshClient, MeshClientConfig, MeshNode, default_cache_path};
use shardd_types::{
    BalancesResponse, CollapsedBalance, CreateEventRequest, DebugOriginResponse,
    DeletedBucketEntry, DigestInfo, Event, EventsFilterRequest, EventsFilterResponse,
    EventsResponse, HealthResponse, NodeRegistryEntry, NodeRpcError, NodeRpcErrorCode,
    NodeRpcRequest, NodeRpcResponse, PersistenceStats, PublicEdgeDirectoryResponse,
    PublicEdgeHealthResponse, PublicEdgeSummary, StateResponse,
};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "shardd-gateway",
    about = "Edge HTTP gateway backed by a libp2p shardd mesh client"
)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 8080)]
    port: u16,
    #[arg(long = "bootstrap-peer")]
    bootstrap_peer: Vec<String>,
    #[arg(long)]
    psk_file: Option<String>,
    #[arg(long, env = "SHARDD_CLUSTER_KEY")]
    cluster_key: Option<String>,
    #[arg(long, default_value = "5000")]
    discovery_timeout_ms: u64,
    #[arg(long, default_value = "5000")]
    request_timeout_ms: u64,
    #[arg(long, default_value_t = 3)]
    top_k: usize,
    #[arg(long, default_value_t = 64)]
    max_sync_gap: u64,
    #[arg(long)]
    peer_cache_file: Option<PathBuf>,
    #[arg(long, env = "SHARDD_DASHBOARD_URL")]
    dashboard_url: Option<String>,
    #[arg(long, env = "SHARDD_DASHBOARD_MACHINE_AUTH_SECRET")]
    dashboard_machine_auth_secret: Option<String>,
    #[arg(long, env = "SHARDD_PUBLIC_EDGE_ID")]
    public_edge_id: Option<String>,
    #[arg(long, env = "SHARDD_PUBLIC_EDGE_REGION")]
    public_edge_region: Option<String>,
    #[arg(long, env = "SHARDD_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
    #[arg(long, env = "SHARDD_PUBLIC_EDGES_JSON")]
    public_edges_json: Option<String>,
}

#[derive(Clone)]
struct AppState {
    mesh: Arc<MeshClient>,
    auth: Option<Arc<GatewayAuthClient>>,
    public_edges: Option<Arc<PublicEdgeDirectory>>,
}

#[derive(Debug, Serialize)]
struct GatewayNodeSummary {
    node_id: String,
    peer_id: String,
    advertise_addr: Option<String>,
    /// Every multiaddr the peer advertises (public, VPC-private, Tailscale).
    /// libp2p dials these in parallel; the fastest reachable one wins.
    listen_addrs: Vec<String>,
    ping_rtt_ms: Option<u64>,
    ready: Option<bool>,
    sync_gap: Option<u64>,
    overloaded: Option<bool>,
    inflight_requests: Option<u64>,
    failure_count: u32,
    /// True if this is the node the gateway would currently route a new
    /// request to (highest in `MeshClient::best_node()`'s ranking).
    #[serde(default)]
    is_best: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PublicEdgeConfig {
    edge_id: String,
    region: String,
    base_url: String,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    node_label: Option<String>,
}

#[derive(Clone)]
struct PublicEdgeDirectory {
    self_edge: PublicEdgeConfig,
    known_edges: Vec<PublicEdgeConfig>,
    http: Client,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GatewayMachineAction {
    Read,
    Write,
}

#[derive(Debug, Clone)]
struct GatewayAuthClient {
    base_url: String,
    shared_secret: String,
    http: Client,
    cache: DashMap<String, CachedDecision>,
}

#[derive(Debug, Clone)]
struct CachedDecision {
    decision: DashboardDecision,
    expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DashboardIntrospectResponse {
    decision: DashboardDecision,
}

#[derive(Debug, Clone, Deserialize)]
struct DashboardDecision {
    valid: bool,
    allowed: bool,
    user_id: Option<Uuid>,
    cache_ttl_ms: u64,
    denial_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct DashboardIntrospectRequest<'a> {
    api_key: &'a str,
    action: GatewayMachineAction,
    bucket: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
struct GatewayBucketEventRequest {
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
    /// Reserve fields — caller-driven hold. See `CreateEventRequest`.
    #[serde(default)]
    hold_amount: Option<u64>,
    #[serde(default)]
    hold_expires_at_unix_ms: Option<u64>,
    /// Settle (one-shot capture) against this reservation id.
    #[serde(default)]
    settle_reservation: Option<String>,
    /// Cancel this reservation outright.
    #[serde(default)]
    release_reservation: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GatewayCreateEventRequest {
    owner: Option<String>,
    bucket: String,
    #[serde(flatten)]
    event: GatewayBucketEventRequest,
}

#[derive(Debug, Deserialize)]
struct OwnerBucketQuery {
    owner: Option<String>,
    bucket: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OwnerQuery {
    owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BucketListQuery {
    q: Option<String>,
    page: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct BucketEventsQuery {
    q: Option<String>,
    account: Option<String>,
    page: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Default)]
struct BucketAggregate {
    total_balance: i64,
    available_balance: i64,
    active_hold_total: i64,
    account_count: usize,
    balance_event_count: usize,
    observed_event_count: usize,
    last_event_at_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct InternalBucketSummary {
    bucket: String,
    total_balance: i64,
    available_balance: i64,
    active_hold_total: i64,
    account_count: usize,
    event_count: usize,
    last_event_at_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct InternalBucketListResponse {
    buckets: Vec<InternalBucketSummary>,
    total: usize,
    page: usize,
    limit: usize,
}

#[derive(Debug, Serialize)]
struct InternalBucketAccountSummary {
    account: String,
    balance: i64,
    available_balance: i64,
    active_hold_total: i64,
    event_count: usize,
    last_event_at_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct InternalBucketDetailResponse {
    summary: InternalBucketSummary,
    accounts: Vec<InternalBucketAccountSummary>,
}

#[derive(Debug, Serialize)]
struct InternalBucketEventsResponse {
    events: Vec<Event>,
    total: usize,
    page: usize,
    limit: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "shardd_gateway=info,shardd_broadcast=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    if cli.bootstrap_peer.is_empty() {
        return Err(anyhow!("at least one --bootstrap-peer is required"));
    }

    let mut config = MeshClientConfig::new(parse_bootstrap_peers(&cli.bootstrap_peer)?);
    config.request_timeout = Duration::from_millis(cli.request_timeout_ms);
    config.top_k = cli.top_k.max(1);
    config.max_sync_gap = cli.max_sync_gap;
    config.cache_path = cli
        .peer_cache_file
        .clone()
        .or_else(|| default_cache_path("gateway-peers.json"));
    config.psk = match (&cli.cluster_key, &cli.psk_file) {
        (Some(key), _) => Some(derive_psk_from_cluster_key(key)?),
        (None, Some(path)) => Some(load_psk_file(path)?),
        (None, None) => None,
    };
    // Stable mesh_client identity seed. `public_edge_id` is unique per
    // regional edge and already in the deployment config — reuse it.
    // The gateway always configures a persistent peer cache, and
    // MeshClient::start refuses to start when a cache is set without an
    // identity_seed, so omitting `--public-edge-id` in prod is a hard
    // failure, not a silent degradation. Two gateways in the same mesh
    // MUST have distinct public_edge_ids or their PeerIds collide.
    if let Some(edge_id) = cli.public_edge_id.as_ref()
        && !edge_id.trim().is_empty()
    {
        config.identity_seed = edge_id.clone();
    }

    let auth = match (
        cli.dashboard_url.as_ref(),
        cli.dashboard_machine_auth_secret.as_ref(),
    ) {
        (Some(url), Some(secret)) => Some(Arc::new(GatewayAuthClient::new(
            url.clone(),
            secret.clone(),
        )?)),
        (None, None) => None,
        _ => {
            return Err(anyhow!(
                "--dashboard-url and --dashboard-machine-auth-secret must be provided together"
            ));
        }
    };

    let mesh = Arc::new(MeshClient::start(config)?);
    if let Err(error) = mesh
        .wait_for_min_candidates(1, Duration::from_millis(cli.discovery_timeout_ms))
        .await
    {
        warn!(error = %error, "gateway starting before mesh discovery completed");
    }

    let public_edges = build_public_edge_directory(&cli)?;
    let state = AppState {
        mesh,
        auth,
        public_edges,
    };
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind((cli.host.as_str(), cli.port)).await?;
    info!(listen = %listener.local_addr()?, "starting shardd edge gateway");
    serve(listener, app).await?;
    Ok(())
}

fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/gateway/health", get(gateway_health))
        .route("/gateway/edges", get(gateway_edges))
        .route("/gateway/nodes", get(gateway_nodes))
        .route("/metrics", get(prometheus_metrics))
        .route("/internal/users/:user_id/events", get(internal_user_events))
        .route(
            "/internal/users/:user_id/buckets/:bucket/events",
            get(internal_bucket_events).post(internal_create_bucket_event),
        )
        .route(
            "/internal/users/:user_id/buckets/:bucket",
            get(internal_bucket_detail),
        )
        .route(
            "/internal/users/:user_id/buckets",
            get(internal_bucket_list),
        )
        .route(
            "/internal/billing/events",
            axum::routing::post(internal_billing_create_event),
        )
        .route("/internal/billing/balance", get(internal_billing_balance))
        .route("/internal/mesh/nodes", get(internal_mesh_nodes))
        .route(
            "/internal/meta/bucket-delete",
            axum::routing::post(internal_meta_bucket_delete),
        )
        .route("/internal/buckets/deleted", get(internal_deleted_buckets))
        .route("/internal/admin/events", get(internal_admin_events))
        .route("/health", get(proxy_health))
        .route("/state", get(proxy_state))
        .route("/events", get(proxy_events).post(proxy_create_event))
        .route("/heads", get(proxy_heads))
        .route("/balances", get(proxy_balances))
        .route("/collapsed/:bucket/:account", get(proxy_collapsed_account))
        .route("/collapsed", get(proxy_collapsed))
        .route("/persistence", get(proxy_persistence))
        .route("/digests", get(proxy_digests))
        .route("/debug/origin/:id", get(proxy_debug_origin))
        .route("/registry", get(proxy_registry))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

impl GatewayAuthClient {
    fn new(base_url: String, shared_secret: String) -> Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            shared_secret,
            http: Client::builder().timeout(Duration::from_secs(5)).build()?,
            cache: DashMap::new(),
        })
    }

    async fn authorize(
        &self,
        api_key: &str,
        action: GatewayMachineAction,
        bucket: &str,
    ) -> Result<DashboardDecision> {
        let now_ms = now_ms();
        let cache_key = auth_cache_key(api_key, &action, bucket);
        if let Some(entry) = self.cache.get(&cache_key)
            && entry.expires_at_unix_ms > now_ms
        {
            return Ok(entry.decision.clone());
        }
        self.cache.remove(&cache_key);

        let response = self
            .http
            .post(format!("{}/api/machine/introspect", self.base_url))
            .header("x-machine-auth-secret", &self.shared_secret)
            .json(&DashboardIntrospectRequest {
                api_key,
                action,
                bucket,
            })
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "dashboard introspection failed with {status}: {body}"
            ));
        }

        let payload: DashboardIntrospectResponse = response.json().await?;
        if payload.decision.allowed && payload.decision.cache_ttl_ms > 0 {
            self.cache.insert(
                cache_key,
                CachedDecision {
                    decision: payload.decision.clone(),
                    expires_at_unix_ms: now_ms + payload.decision.cache_ttl_ms,
                },
            );
        }
        Ok(payload.decision)
    }
}

impl PublicEdgeConfig {
    fn new(edge_id: String, region: String, base_url: String) -> Result<Self> {
        let parsed = reqwest::Url::parse(base_url.trim_end_matches('/'))
            .map_err(|error| anyhow!("invalid public edge base URL `{base_url}`: {error}"))?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            return Err(anyhow!(
                "public edge base URL `{base_url}` must use http or https"
            ));
        }
        Ok(Self {
            edge_id,
            region,
            base_url: parsed.to_string().trim_end_matches('/').to_string(),
            node_id: None,
            label: None,
            node_label: None,
        })
    }

    fn health_url(&self) -> String {
        format!("{}/gateway/health", self.base_url)
    }
}

impl PublicEdgeDirectory {
    fn new(
        mut self_edge: PublicEdgeConfig,
        configured_edges: Vec<PublicEdgeConfig>,
    ) -> Result<Self> {
        if let Some(json_self) = configured_edges
            .iter()
            .find(|e| e.edge_id == self_edge.edge_id)
        {
            if self_edge.node_id.is_none() {
                self_edge.node_id = json_self.node_id.clone();
            }
            if self_edge.label.is_none() {
                self_edge.label = json_self.label.clone();
            }
            if self_edge.node_label.is_none() {
                self_edge.node_label = json_self.node_label.clone();
            }
        }

        let mut known_edges = Vec::new();
        let mut seen = std::collections::BTreeSet::new();

        for edge in std::iter::once(self_edge.clone()).chain(configured_edges.into_iter()) {
            let key = (edge.edge_id.clone(), edge.base_url.clone());
            if seen.insert(key) {
                known_edges.push(edge);
            }
        }

        Ok(Self {
            self_edge,
            known_edges,
            http: Client::builder().timeout(Duration::from_secs(2)).build()?,
        })
    }
}

fn build_public_edge_directory(cli: &Cli) -> Result<Option<Arc<PublicEdgeDirectory>>> {
    let self_edge = match (
        cli.public_edge_id.clone(),
        cli.public_edge_region.clone(),
        cli.public_base_url.clone(),
    ) {
        (None, None, None) => None,
        (Some(edge_id), Some(region), Some(base_url)) => {
            Some(PublicEdgeConfig::new(edge_id, region, base_url)?)
        }
        _ => {
            return Err(anyhow!(
                "--public-edge-id, --public-edge-region, and --public-base-url must be provided together"
            ));
        }
    };

    let configured_edges = match &cli.public_edges_json {
        Some(raw) => serde_json::from_str::<Vec<PublicEdgeConfig>>(raw)
            .map_err(|error| anyhow!("invalid SHARDD_PUBLIC_EDGES_JSON: {error}"))?,
        None => Vec::new(),
    };

    match self_edge {
        Some(self_edge) => Ok(Some(Arc::new(PublicEdgeDirectory::new(
            self_edge,
            configured_edges,
        )?))),
        None if configured_edges.is_empty() => Ok(None),
        None => Err(anyhow!(
            "SHARDD_PUBLIC_EDGES_JSON requires --public-edge-id, --public-edge-region, and --public-base-url"
        )),
    }
}

async fn gateway_health(State(state): State<AppState>) -> Json<PublicEdgeHealthResponse> {
    Json(build_gateway_health(&state))
}

async fn gateway_edges(State(state): State<AppState>) -> Response {
    let Some(directory) = &state.public_edges else {
        return gateway_unavailable_response("public edge directory is not configured".to_string());
    };

    Json(build_public_edge_directory_response(&state, directory).await).into_response()
}

async fn gateway_nodes(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("gateway nodes");
    }
    Json(summarize_nodes_with_best(&state)).into_response()
}

/// Machine-authed mirror of `/gateway/nodes`. Lets the dashboard's admin
/// panel inspect every advertised address per node without exposing the
/// same data to unauthenticated clients.
async fn internal_mesh_nodes(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }
    Json(summarize_nodes_with_best(&state)).into_response()
}

async fn proxy_health(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return Json(build_gateway_health(&state)).into_response();
    }
    proxy_read(State(state), NodeRpcRequest::Health, expect_health).await
}

async fn proxy_state(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("state");
    }
    proxy_read(State(state), NodeRpcRequest::State, expect_state).await
}

async fn proxy_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Query<OwnerBucketQuery>,
) -> Response {
    if let Err(message) = reject_legacy_owner(
        query.0.owner.as_deref(),
        "owner query parameter is no longer supported; API keys identify the user",
    ) {
        return bad_request(message);
    }
    let Some(auth) = &state.auth else {
        return proxy_read(State(state), NodeRpcRequest::Events, expect_events).await;
    };

    let bucket = match require_bucket_query(&query.0) {
        Ok(bucket) => bucket,
        Err(message) => return bad_request(message),
    };
    let authz = match authorize_request(auth, &headers, GatewayMachineAction::Read, &bucket).await {
        Ok(authz) => authz,
        Err(response) => return response,
    };
    if let Err(response) = billing_check_and_deduct(&state, authz.user_id, 1, "read").await {
        return response;
    }
    let internal_bucket = internal_bucket_for_user(authz.user_id, &bucket);
    match state
        .mesh
        .request_best_with_node(NodeRpcRequest::Events)
        .await
    {
        Ok((node, Ok(response))) => match expect_events(response) {
            Ok(body) => {
                let filtered = EventsResponse {
                    events: body
                        .events
                        .into_iter()
                        .filter(|event| event.bucket == internal_bucket)
                        .map(|mut event| {
                            event.bucket = bucket.clone();
                            event
                        })
                        .collect(),
                };
                json_response(StatusCode::OK, &node, &filtered)
            }
            Err(error) => gateway_internal_response(Some(&node), error.to_string()),
        },
        Ok((node, Err(error))) => rpc_error_response(&node, error),
        Err(error) => gateway_unavailable_response(error.to_string()),
    }
}

async fn proxy_heads(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("heads");
    }
    proxy_read(State(state), NodeRpcRequest::Heads, expect_heads).await
}

async fn proxy_balances(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Query<OwnerBucketQuery>,
) -> Response {
    if let Err(message) = reject_legacy_owner(
        query.0.owner.as_deref(),
        "owner query parameter is no longer supported; API keys identify the user",
    ) {
        return bad_request(message);
    }
    let Some(auth) = &state.auth else {
        return proxy_read(State(state), NodeRpcRequest::Balances, expect_balances).await;
    };

    let bucket = match require_bucket_query(&query.0) {
        Ok(bucket) => bucket,
        Err(message) => return bad_request(message),
    };
    let authz = match authorize_request(auth, &headers, GatewayMachineAction::Read, &bucket).await {
        Ok(authz) => authz,
        Err(response) => return response,
    };
    if let Err(response) = billing_check_and_deduct(&state, authz.user_id, 1, "read").await {
        return response;
    }
    let internal_bucket = internal_bucket_for_user(authz.user_id, &bucket);
    match state
        .mesh
        .request_best_with_node(NodeRpcRequest::Balances)
        .await
    {
        Ok((node, Ok(response))) => match expect_balances(response) {
            Ok(body) => json_response(
                StatusCode::OK,
                &node,
                &filter_balances(body, &internal_bucket, &bucket),
            ),
            Err(error) => gateway_internal_response(Some(&node), error.to_string()),
        },
        Ok((node, Err(error))) => rpc_error_response(&node, error),
        Err(error) => gateway_unavailable_response(error.to_string()),
    }
}

async fn proxy_collapsed(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Query<OwnerBucketQuery>,
) -> Response {
    if let Err(message) = reject_legacy_owner(
        query.0.owner.as_deref(),
        "owner query parameter is no longer supported; API keys identify the user",
    ) {
        return bad_request(message);
    }
    let Some(auth) = &state.auth else {
        return proxy_read(State(state), NodeRpcRequest::Collapsed, expect_collapsed).await;
    };

    let bucket = match require_bucket_query(&query.0) {
        Ok(bucket) => bucket,
        Err(message) => return bad_request(message),
    };
    let authz = match authorize_request(auth, &headers, GatewayMachineAction::Read, &bucket).await {
        Ok(authz) => authz,
        Err(response) => return response,
    };
    if let Err(response) = billing_check_and_deduct(&state, authz.user_id, 1, "read").await {
        return response;
    }
    let internal_bucket = internal_bucket_for_user(authz.user_id, &bucket);
    match state
        .mesh
        .request_best_with_node(NodeRpcRequest::Collapsed)
        .await
    {
        Ok((node, Ok(response))) => match expect_collapsed(response) {
            Ok(body) => json_response(
                StatusCode::OK,
                &node,
                &filter_collapsed(body, &internal_bucket, &bucket),
            ),
            Err(error) => gateway_internal_response(Some(&node), error.to_string()),
        },
        Ok((node, Err(error))) => rpc_error_response(&node, error),
        Err(error) => gateway_unavailable_response(error.to_string()),
    }
}

async fn proxy_collapsed_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((bucket, account)): Path<(String, String)>,
    query: Query<OwnerQuery>,
) -> Response {
    if let Err(message) = reject_legacy_owner(
        query.0.owner.as_deref(),
        "owner query parameter is no longer supported; API keys identify the user",
    ) {
        return bad_request(message);
    }
    let Some(auth) = &state.auth else {
        return proxy_read(
            State(state),
            NodeRpcRequest::CollapsedAccount { bucket, account },
            expect_collapsed_account,
        )
        .await;
    };

    let authz = match authorize_request(auth, &headers, GatewayMachineAction::Read, &bucket).await {
        Ok(authz) => authz,
        Err(response) => return response,
    };
    if let Err(response) = billing_check_and_deduct(&state, authz.user_id, 1, "read").await {
        return response;
    }
    let internal_bucket = internal_bucket_for_user(authz.user_id, &bucket);
    proxy_read(
        State(state),
        NodeRpcRequest::CollapsedAccount {
            bucket: internal_bucket,
            account,
        },
        expect_collapsed_account,
    )
    .await
}

async fn proxy_persistence(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("persistence");
    }
    proxy_read(
        State(state),
        NodeRpcRequest::Persistence,
        expect_persistence,
    )
    .await
}

async fn proxy_digests(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("digests");
    }
    proxy_read(State(state), NodeRpcRequest::Digests, expect_digests).await
}

async fn proxy_debug_origin(
    State(state): State<AppState>,
    Path(origin_id): Path<String>,
) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("debug origin");
    }
    proxy_read(
        State(state),
        NodeRpcRequest::DebugOrigin { origin_id },
        expect_debug_origin,
    )
    .await
}

async fn proxy_registry(State(state): State<AppState>) -> Response {
    if state.auth.is_some() {
        return not_exposed_response("registry");
    }
    proxy_read(State(state), NodeRpcRequest::Registry, expect_registry).await
}

async fn proxy_create_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<GatewayCreateEventRequest>,
) -> Response {
    if let Err(message) = reject_legacy_owner(
        request.owner.as_deref(),
        "owner is no longer supported; API keys identify the user",
    ) {
        return bad_request(message);
    }
    let (bucket, internal_bucket, rewrite_bucket) = if let Some(auth) = &state.auth {
        let authz =
            match authorize_request(auth, &headers, GatewayMachineAction::Write, &request.bucket)
                .await
            {
                Ok(authz) => authz,
                Err(response) => return response,
            };
        if let Err(response) = billing_check_and_deduct(&state, authz.user_id, 10, "write").await {
            return response;
        }
        let internal_bucket = internal_bucket_for_user(authz.user_id, &request.bucket);
        (request.bucket.clone(), internal_bucket, true)
    } else {
        (request.bucket.clone(), request.bucket.clone(), false)
    };

    submit_create_event(
        &state,
        &bucket,
        internal_bucket,
        request.event,
        rewrite_bucket,
        false,
    )
    .await
}

async fn internal_bucket_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Query(query): Query<BucketListQuery>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    let (node, balances) =
        match request_best_typed(&state, NodeRpcRequest::Balances, expect_balances).await {
            Ok(values) => values,
            Err(response) => return response,
        };
    let events = match request_best_typed(&state, NodeRpcRequest::Events, expect_events).await {
        Ok((_, events)) => events,
        Err(response) => return response,
    };

    let q = normalized_query(query.q.as_deref());
    let (page, limit, offset) = pagination(query.page, query.limit, 25, 100);
    let mut buckets = summarize_user_buckets(user_id, balances, events);
    if let Some(needle) = q.as_deref() {
        buckets.retain(|summary| contains_case_insensitive(&summary.bucket, needle));
    }
    buckets.sort_by(|left, right| {
        right
            .last_event_at_unix_ms
            .unwrap_or(0)
            .cmp(&left.last_event_at_unix_ms.unwrap_or(0))
            .then_with(|| left.bucket.cmp(&right.bucket))
    });
    let total = buckets.len();
    let buckets = buckets
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    json_response(
        StatusCode::OK,
        &node,
        &InternalBucketListResponse {
            buckets,
            total,
            page,
            limit,
        },
    )
}

async fn internal_bucket_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((user_id, bucket)): Path<(Uuid, String)>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    let (node, balances) =
        match request_best_typed(&state, NodeRpcRequest::Balances, expect_balances).await {
            Ok(values) => values,
            Err(response) => return response,
        };
    let events = match request_best_typed(&state, NodeRpcRequest::Events, expect_events).await {
        Ok((_, events)) => events,
        Err(response) => return response,
    };

    let internal_bucket = internal_bucket_for_user(user_id, &bucket);
    let mut event_totals_by_account = BTreeMap::<String, usize>::new();
    let mut last_event_by_account = BTreeMap::<String, u64>::new();
    let mut total_event_count = 0usize;
    let mut last_event_at_unix_ms: Option<u64> = None;

    for event in events.events {
        if event.bucket != internal_bucket {
            continue;
        }
        total_event_count += 1;
        last_event_at_unix_ms = Some(
            last_event_at_unix_ms.map_or(event.created_at_unix_ms, |current| {
                current.max(event.created_at_unix_ms)
            }),
        );
        *event_totals_by_account
            .entry(event.account.clone())
            .or_insert(0) += 1;
        last_event_by_account
            .entry(event.account)
            .and_modify(|current| *current = (*current).max(event.created_at_unix_ms))
            .or_insert(event.created_at_unix_ms);
    }

    let mut accounts = balances
        .accounts
        .into_iter()
        .filter(|account| account.bucket == internal_bucket)
        .map(|account| InternalBucketAccountSummary {
            account: account.account.clone(),
            balance: account.balance,
            available_balance: account.available_balance,
            active_hold_total: account.active_hold_total,
            event_count: event_totals_by_account
                .get(&account.account)
                .copied()
                .unwrap_or(account.event_count),
            last_event_at_unix_ms: last_event_by_account.get(&account.account).copied(),
        })
        .collect::<Vec<_>>();

    if accounts.is_empty() && total_event_count == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "bucket not found" })),
        )
            .into_response();
    }

    accounts.sort_by(|left, right| {
        right
            .balance
            .cmp(&left.balance)
            .then_with(|| left.account.cmp(&right.account))
    });

    let summary = InternalBucketSummary {
        bucket,
        total_balance: accounts.iter().map(|account| account.balance).sum(),
        available_balance: accounts
            .iter()
            .map(|account| account.available_balance)
            .sum(),
        active_hold_total: accounts
            .iter()
            .map(|account| account.active_hold_total)
            .sum(),
        account_count: accounts.len(),
        event_count: total_event_count
            .max(accounts.iter().map(|account| account.event_count).sum()),
        last_event_at_unix_ms,
    };

    json_response(
        StatusCode::OK,
        &node,
        &InternalBucketDetailResponse { summary, accounts },
    )
}

async fn internal_bucket_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((user_id, bucket)): Path<(Uuid, String)>,
    Query(query): Query<BucketEventsQuery>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    let (node, events) =
        match request_best_typed(&state, NodeRpcRequest::Events, expect_events).await {
            Ok(values) => values,
            Err(response) => return response,
        };

    let internal_bucket = internal_bucket_for_user(user_id, &bucket);
    let q = normalized_query(query.q.as_deref());
    let account = normalized_query(query.account.as_deref());
    let (page, limit, offset) = pagination(query.page, query.limit, 25, 200);
    let mut filtered = events
        .events
        .into_iter()
        .filter(|event| event.bucket == internal_bucket)
        .filter(|event| {
            account
                .as_deref()
                .is_none_or(|needle| contains_case_insensitive(&event.account, needle))
        })
        .filter(|event| {
            q.as_deref()
                .is_none_or(|needle| event_matches_query(event, needle))
        })
        .map(|mut event| {
            event.bucket = bucket.clone();
            event
        })
        .collect::<Vec<_>>();

    filtered.sort_by(|left, right| {
        right
            .created_at_unix_ms
            .cmp(&left.created_at_unix_ms)
            .then_with(|| right.origin_seq.cmp(&left.origin_seq))
            .then_with(|| right.event_id.cmp(&left.event_id))
    });

    let total = filtered.len();
    let events = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();

    json_response(
        StatusCode::OK,
        &node,
        &InternalBucketEventsResponse {
            events,
            total,
            page,
            limit,
        },
    )
}

async fn internal_create_bucket_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((user_id, bucket)): Path<(Uuid, String)>,
    Json(request): Json<GatewayBucketEventRequest>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    let internal_bucket = internal_bucket_for_user(user_id, &bucket);
    submit_create_event(&state, &bucket, internal_bucket, request, true, false).await
}

async fn submit_create_event(
    state: &AppState,
    external_bucket: &str,
    internal_bucket: String,
    request: GatewayBucketEventRequest,
    rewrite_bucket: bool,
    allow_reserved_bucket: bool,
) -> Response {
    if let Err(message) = shardd_types::validate_event_note(request.note.as_deref()) {
        return bad_request(&message);
    }
    let Some(nonce) = request
        .idempotency_nonce
        .as_ref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
    else {
        return bad_request(
            "idempotency_nonce is required — generate a UUID v4 per logical operation and reuse it on retries",
        );
    };
    let mut create_request = GatewayBucketEventRequest {
        idempotency_nonce: Some(nonce),
        ..request
    }
    .into_create_request(internal_bucket);
    create_request.allow_reserved_bucket = allow_reserved_bucket;

    match state
        .mesh
        .request_best_with_node(NodeRpcRequest::CreateEvent(create_request))
        .await
    {
        Ok((node, Ok(NodeRpcResponse::CreateEvent(mut body)))) => {
            if rewrite_bucket {
                body.event.bucket = external_bucket.to_string();
            }
            // Dedup always in play now that every event carries a nonce.
            // 200 signals "already happened" on retries; 201 on first write.
            let status = if body.deduplicated {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            json_response(status, &node, &body)
        }
        Ok((node, Ok(other))) => gateway_internal_response(
            Some(&node),
            format!("unexpected response for create_event: {other:?}"),
        ),
        Ok((node, Err(error))) => rpc_error_response(&node, error),
        Err(error) => gateway_unavailable_response(error.to_string()),
    }
}

async fn proxy_read<T, F>(
    State(state): State<AppState>,
    request: NodeRpcRequest,
    map: F,
) -> Response
where
    T: Serialize,
    F: FnOnce(NodeRpcResponse) -> Result<T>,
{
    match request_best_typed(&state, request, map).await {
        Ok((node, body)) => json_response(StatusCode::OK, &node, &body),
        Err(response) => response,
    }
}

async fn request_best_typed<T, F>(
    state: &AppState,
    request: NodeRpcRequest,
    map: F,
) -> Result<(MeshNode, T), Response>
where
    F: FnOnce(NodeRpcResponse) -> Result<T>,
{
    match state.mesh.request_best_with_node(request).await {
        Ok((node, Ok(response))) => match map(response) {
            Ok(body) => Ok((node, body)),
            Err(error) => Err(gateway_internal_response(Some(&node), error.to_string())),
        },
        Ok((node, Err(error))) => Err(rpc_error_response(&node, error)),
        Err(error) => Err(gateway_unavailable_response(error.to_string())),
    }
}

#[derive(Debug, Clone)]
struct AuthorizedBucket {
    user_id: Uuid,
}

impl GatewayBucketEventRequest {
    fn into_create_request(self, bucket: String) -> CreateEventRequest {
        CreateEventRequest {
            bucket,
            account: self.account,
            amount: self.amount,
            note: self.note,
            // Unwrap: `submit_create_event` validates presence before
            // calling this; internal synthesized calls always populate.
            idempotency_nonce: self.idempotency_nonce.expect("idempotency_nonce required"),
            max_overdraft: self.max_overdraft,
            min_acks: self.min_acks,
            ack_timeout_ms: self.ack_timeout_ms,
            hold_amount: self.hold_amount,
            hold_expires_at_unix_ms: self.hold_expires_at_unix_ms,
            settle_reservation: self.settle_reservation,
            release_reservation: self.release_reservation,
            // Wire payload doesn't expose this, so external clients
            // never set it. `submit_create_event` overwrites the value
            // based on the route after this conversion.
            allow_reserved_bucket: false,
        }
    }
}

// --------------- internal billing endpoints ---------------

/// §3.5: hard-delete a bucket cluster-wide by emitting a `BucketDelete`
/// meta event on the node we're locally attached to. The meta event
/// replicates to every peer via normal gossipsub; each peer applies the
/// cascade on receipt. Machine-auth only — the dashboard's admin and
/// developer delete handlers are the only legitimate callers.
#[derive(Debug, serde::Deserialize)]
struct InternalMetaBucketDeleteRequest {
    bucket: String,
    #[serde(default)]
    reason: Option<String>,
}

async fn internal_meta_bucket_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InternalMetaBucketDeleteRequest>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }
    if request.bucket.is_empty() {
        return bad_request("bucket is required");
    }

    match state
        .mesh
        .request_best_with_node(NodeRpcRequest::DeleteBucket {
            bucket: request.bucket.clone(),
            reason: request.reason.clone(),
        })
        .await
    {
        Ok((node, Ok(NodeRpcResponse::DeleteBucket(event)))) => json_response(
            StatusCode::OK,
            &node,
            &serde_json::json!({
                "event_id": event.event_id,
                "bucket": request.bucket,
            }),
        ),
        Ok((node, Ok(other))) => gateway_internal_response(
            Some(&node),
            format!("unexpected node response for DeleteBucket: {:?}", other),
        ),
        Ok((node, Err(error))) => rpc_error_response(&node, error),
        Err(error) => gateway_unavailable_response(error.to_string()),
    }
}

/// Admin events viewer backend. Machine-auth only; the dashboard's
/// `AdminUser`-gated `/api/admin/events` route is the only legit caller.
/// Serves the single-node filtered view + per-edge heads snapshot
/// (`replication`) so the UI can annotate each row with its cluster-wide
/// replication state in one round-trip.
#[derive(Debug, serde::Deserialize)]
struct InternalAdminEventsQuery {
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
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
    /// When set, fan-out to every known edge's heads/max_known and bundle
    /// them in the response. Expensive; only used when the dashboard UI
    /// is showing the replication matrix drawer for a specific event.
    #[serde(default)]
    replication: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
struct InternalAdminEventsReplication {
    /// node_label → { heads, max_known_seqs } snapshot.
    per_node: BTreeMap<String, InternalAdminEventsReplicationEntry>,
}

#[derive(Debug, serde::Serialize)]
struct InternalAdminEventsReplicationEntry {
    heads: BTreeMap<String, u64>,
    max_known_seqs: BTreeMap<String, u64>,
    error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct InternalAdminEventsResponseBody {
    #[serde(flatten)]
    page: EventsFilterResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    replication: Option<InternalAdminEventsReplication>,
}

async fn internal_admin_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InternalAdminEventsQuery>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0);
    let request = EventsFilterRequest {
        bucket: query.bucket,
        bucket_prefix: None,
        account: query.account,
        origin: query.origin,
        event_type: query.event_type,
        since_unix_ms: query.since_ms,
        until_unix_ms: query.until_ms,
        search: query.search,
        limit,
        offset,
    };

    let page = match request_best_typed::<EventsFilterResponse, _>(
        &state,
        NodeRpcRequest::EventsFilter(request),
        expect_events_filter,
    )
    .await
    {
        Ok((_, body)) => body,
        Err(response) => return response,
    };

    let replication = if query.replication.unwrap_or(false) {
        Some(collect_replication_snapshot(&state).await)
    } else {
        None
    };

    let body = InternalAdminEventsResponseBody { page, replication };
    match serde_json::to_value(&body) {
        Ok(value) => Json(value).into_response(),
        Err(error) => gateway_internal_response(None, error.to_string()),
    }
}

/// Per-user events viewer backend. Same shape as `internal_admin_events`
/// but scoped to one user's bucket namespace. The dashboard's
/// `CurrentUser`-gated `/api/developer/events` route is the only legit
/// caller. Rewrites every internal bucket name back to the user-facing
/// form before returning — any row whose bucket doesn't decode for this
/// user is dropped as defense-in-depth against cross-user leaks.
async fn internal_user_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Query(query): Query<InternalAdminEventsQuery>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0);
    // A specific user-facing bucket filter translates to the exact
    // internal name; otherwise scope to the user's namespace via prefix.
    let (bucket_filter, prefix_filter) = match query.bucket.as_deref() {
        Some(b) if !b.is_empty() => (Some(internal_bucket_for_user(user_id, b)), None),
        _ => (None, Some(internal_bucket_prefix(user_id))),
    };
    let request = EventsFilterRequest {
        bucket: bucket_filter,
        bucket_prefix: prefix_filter,
        account: query.account,
        origin: query.origin,
        event_type: query.event_type,
        since_unix_ms: query.since_ms,
        until_unix_ms: query.until_ms,
        search: query.search,
        limit,
        offset,
    };

    let mut page = match request_best_typed::<EventsFilterResponse, _>(
        &state,
        NodeRpcRequest::EventsFilter(request),
        expect_events_filter,
    )
    .await
    {
        Ok((_, body)) => body,
        Err(response) => return response,
    };

    rewrite_events_to_external(user_id, &mut page);

    let replication = if query.replication.unwrap_or(false) {
        let mut snap = collect_replication_snapshot(&state).await;
        for entry in snap.per_node.values_mut() {
            entry.heads = rewrite_bucket_keys(user_id, std::mem::take(&mut entry.heads));
            entry.max_known_seqs =
                rewrite_bucket_keys(user_id, std::mem::take(&mut entry.max_known_seqs));
        }
        Some(snap)
    } else {
        None
    };

    let body = InternalAdminEventsResponseBody { page, replication };
    match serde_json::to_value(&body) {
        Ok(value) => Json(value).into_response(),
        Err(error) => gateway_internal_response(None, error.to_string()),
    }
}

/// Strip events whose bucket is outside `user_id`'s namespace (defense in
/// depth against a storage row leaking past the prefix filter), and
/// rewrite every remaining `event.bucket` plus the page's `heads` /
/// `max_known_seqs` keys from the internal `user_X__bucket_Y` form back
/// to the user-facing bucket name.
fn rewrite_events_to_external(user_id: Uuid, page: &mut EventsFilterResponse) {
    let events = std::mem::take(&mut page.events);
    let mut kept = Vec::with_capacity(events.len());
    let mut dropped: u64 = 0;
    for mut event in events {
        match external_bucket_from_internal_user(user_id, &event.bucket) {
            Some(external) => {
                event.bucket = external;
                kept.push(event);
            }
            None => dropped += 1,
        }
    }
    page.events = kept;
    // Keep `total` consistent with the filtered rows so the UI paginator
    // stays honest. Any drops here mean the storage-level filter
    // returned a cross-namespace row, which is already a bug — just
    // don't surface those counts.
    page.total = page.total.saturating_sub(dropped);
    page.heads = rewrite_bucket_keys(user_id, std::mem::take(&mut page.heads));
    page.max_known_seqs = rewrite_bucket_keys(user_id, std::mem::take(&mut page.max_known_seqs));
}

/// Rewrite the bucket segment of `"{bucket}\t{origin}:{epoch}"` keys
/// from internal to external form. Entries whose bucket doesn't decode
/// for this user are dropped.
fn rewrite_bucket_keys(user_id: Uuid, map: BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for (key, value) in map {
        let Some((internal_bucket, rest)) = key.split_once('\t') else {
            continue;
        };
        let Some(external) = external_bucket_from_internal_user(user_id, internal_bucket) else {
            continue;
        };
        out.insert(format!("{external}\t{rest}"), value);
    }
    out
}

/// Snapshot of the node's `deleted_buckets` projection — every
/// hard-purged bucket name with its `BucketDelete` timestamp. The
/// dashboard's `/api/developer/buckets` handler joins this set against
/// the per-user registry to classify each row as active / archived /
/// nuked. Machine-auth only.
async fn internal_deleted_buckets(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }

    match request_best_typed::<Vec<DeletedBucketEntry>, _>(
        &state,
        NodeRpcRequest::DeletedBuckets,
        expect_deleted_buckets,
    )
    .await
    {
        Ok((node, entries)) => json_response(
            StatusCode::OK,
            &node,
            &serde_json::json!({ "buckets": entries }),
        ),
        Err(response) => response,
    }
}

fn expect_deleted_buckets(response: NodeRpcResponse) -> Result<Vec<DeletedBucketEntry>> {
    match response {
        NodeRpcResponse::DeletedBuckets(entries) => Ok(entries),
        other => anyhow::bail!("unexpected node response for deleted-buckets: {:?}", other),
    }
}

fn expect_events_filter(response: NodeRpcResponse) -> Result<EventsFilterResponse> {
    match response {
        NodeRpcResponse::EventsFilter(body) => Ok(body),
        other => anyhow::bail!("unexpected node response for events-filter: {:?}", other),
    }
}

/// Fan out to every known peer's State RPC to gather their heads +
/// max_known snapshots. The UI overlays these with the primary-query
/// heads to show per-edge replication status per event.
async fn collect_replication_snapshot(state: &AppState) -> InternalAdminEventsReplication {
    let nodes = state.mesh.all_nodes();
    let mut per_node: BTreeMap<String, InternalAdminEventsReplicationEntry> = BTreeMap::new();

    // Label lookup: use the configured `node_label` when present, else
    // fall back to the raw node_id. Matches how `prometheus_metrics`
    // surfaces node labels.
    let node_labels: std::collections::HashMap<String, String> = state
        .public_edges
        .as_ref()
        .map(|dir| {
            let mut map = std::collections::HashMap::new();
            for edge in std::iter::once(&dir.self_edge).chain(dir.known_edges.iter()) {
                if let Some(nid) = &edge.node_id {
                    let label = edge
                        .node_label
                        .clone()
                        .unwrap_or_else(|| edge.edge_id.to_uppercase());
                    map.insert(nid.clone(), label);
                }
            }
            map
        })
        .unwrap_or_default();

    for node in nodes {
        let label = node_labels.get(&node.node_id).cloned().unwrap_or_else(|| {
            // Shortened node_id as a stable fallback label.
            node.node_id[..8.min(node.node_id.len())].to_string()
        });
        let peer_id = match node
            .peer_id
            .parse::<shardd_broadcast::libp2p_crate::PeerId>()
        {
            Ok(p) => p,
            Err(error) => {
                per_node.insert(
                    label,
                    InternalAdminEventsReplicationEntry {
                        heads: BTreeMap::new(),
                        max_known_seqs: BTreeMap::new(),
                        error: Some(format!("parse peer_id: {error}")),
                    },
                );
                continue;
            }
        };
        let resp = state.mesh.request_to(peer_id, NodeRpcRequest::State).await;
        let entry = match resp {
            Ok(Ok(NodeRpcResponse::State(state_resp))) => InternalAdminEventsReplicationEntry {
                heads: state_resp.contiguous_heads,
                // max_known isn't in StateResponse — fall back to per-bucket
                // sync_gap via HealthResponse's per-bucket map. But since
                // the UI only uses heads for the "is this at this node"
                // gate, that's enough for v1.
                max_known_seqs: BTreeMap::new(),
                error: None,
            },
            Ok(Ok(other)) => InternalAdminEventsReplicationEntry {
                heads: BTreeMap::new(),
                max_known_seqs: BTreeMap::new(),
                error: Some(format!("unexpected response: {:?}", other)),
            },
            Ok(Err(err)) => InternalAdminEventsReplicationEntry {
                heads: BTreeMap::new(),
                max_known_seqs: BTreeMap::new(),
                error: Some(err.message),
            },
            Err(err) => InternalAdminEventsReplicationEntry {
                heads: BTreeMap::new(),
                max_known_seqs: BTreeMap::new(),
                error: Some(err.to_string()),
            },
        };
        per_node.insert(label, entry);
    }

    InternalAdminEventsReplication { per_node }
}

/// Create an event in a raw bucket (no user-scoping rewrite). Machine auth only.
async fn internal_billing_create_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<GatewayCreateEventRequest>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }
    // No bucket rewriting — use the bucket name as-is. The internal
    // billing route is the only path that may target reserved buckets
    // (e.g. `__billing__<user_id>`), so opt in here.
    submit_create_event(
        &state,
        &request.bucket,
        request.bucket.clone(),
        request.event,
        false,
        true,
    )
    .await
}

/// Read balance for a raw bucket (no user-scoping rewrite). Machine auth only.
async fn internal_billing_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Query<OwnerBucketQuery>,
) -> Response {
    if let Err(response) = authorize_internal_machine(&state, &headers) {
        return *response;
    }
    let bucket = match &query.0.bucket {
        Some(b) if !b.is_empty() => b.clone(),
        _ => return bad_request("bucket query parameter is required"),
    };
    match state
        .mesh
        .request_best_with_node(NodeRpcRequest::Balances)
        .await
    {
        Ok((node, Ok(response))) => match expect_balances(response) {
            Ok(body) => {
                let filtered = BalancesResponse {
                    accounts: body
                        .accounts
                        .into_iter()
                        .filter(|a| a.bucket == bucket)
                        .collect(),
                    total_balance: 0,
                };
                json_response(StatusCode::OK, &node, &filtered)
            }
            Err(error) => gateway_internal_response(Some(&node), error.to_string()),
        },
        Ok((node, Err(error))) => rpc_error_response(&node, error),
        Err(error) => gateway_unavailable_response(error.to_string()),
    }
}

// --------------- billing ---------------

fn billing_bucket_for_user(user_id: Uuid) -> String {
    format!("__billing__{user_id}")
}

/// Check user's credit balance and deduct `cost` credits.
/// Returns Ok(()) if sufficient credits, or an error Response (402) if not.
async fn billing_check_and_deduct(
    state: &AppState,
    user_id: Uuid,
    cost: i64,
    action_label: &str,
) -> Result<(), Response> {
    let billing_bucket = billing_bucket_for_user(user_id);

    // 1. Read balance
    let balance = match state
        .mesh
        .request_best_with_node(NodeRpcRequest::Balances)
        .await
    {
        Ok((_node, Ok(NodeRpcResponse::Balances(body)))) => body
            .accounts
            .iter()
            .find(|a| a.bucket == billing_bucket && a.account == "credits")
            .map(|a| a.balance)
            .unwrap_or(0),
        _ => 0, // if mesh is unavailable, allow (fail open for now)
    };

    // 2. Check
    if balance <= 0 {
        return Err(payment_required());
    }

    // 3. Deduct
    let deduct_request = CreateEventRequest {
        bucket: billing_bucket,
        account: "credits".to_string(),
        amount: -cost,
        note: Some(format!("api:{action_label}")),
        // Fresh UUID per API call — no stable upstream key here; the
        // spawn below retries on transport errors within a single write
        // so a nonce is still useful as dedup insurance.
        idempotency_nonce: uuid::Uuid::new_v4().to_string(),
        max_overdraft: Some(u64::MAX), // billing deductions always go through
        min_acks: None,
        ack_timeout_ms: None,
        hold_amount: None,
        hold_expires_at_unix_ms: None,
        settle_reservation: None,
        release_reservation: None,
        // Internal billing path writes into `__billing__<user_id>`.
        allow_reserved_bucket: true,
    };
    // Fire-and-forget: don't block the user's request on the deduction write
    let mesh = state.mesh.clone();
    tokio::spawn(async move {
        if let Err(e) = mesh
            .request_best_with_node(NodeRpcRequest::CreateEvent(deduct_request))
            .await
        {
            warn!(error = %e, "billing deduction event failed");
        }
    });

    Ok(())
}

fn payment_required() -> Response {
    (
        StatusCode::PAYMENT_REQUIRED,
        Json(serde_json::json!({
            "error": "insufficient_credits",
            "code": "PAYMENT_REQUIRED"
        })),
    )
        .into_response()
}

// --------------- auth ---------------

async fn authorize_request(
    auth: &GatewayAuthClient,
    headers: &HeaderMap,
    action: GatewayMachineAction,
    bucket: &str,
) -> Result<AuthorizedBucket, Response> {
    let api_key = bearer_token(headers).ok_or_else(|| unauthorized("missing bearer api key"))?;
    let decision = auth
        .authorize(api_key, action, bucket)
        .await
        .map_err(|error| gateway_unavailable_response(error.to_string()))?;

    if decision.allowed {
        let Some(user_id) = decision.user_id else {
            return Err(gateway_unavailable_response(
                "dashboard introspection returned no user id".to_string(),
            ));
        };
        return Ok(AuthorizedBucket { user_id });
    }

    let reason = decision
        .denial_reason
        .unwrap_or_else(|| "access_denied".to_string());
    if decision.valid {
        Err(forbidden(&reason))
    } else {
        Err(unauthorized(&reason))
    }
}

fn authorize_internal_machine(state: &AppState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(auth) = &state.auth else {
        return Err(Box::new(gateway_unavailable_response(
            "dashboard machine auth is not configured".to_string(),
        )));
    };

    let provided = headers
        .get("x-machine-auth-secret")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Box::new(forbidden("missing machine auth secret")))?;
    if provided != auth.shared_secret {
        return Err(Box::new(forbidden("invalid machine auth secret")));
    }
    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .filter(|value| !value.trim().is_empty())
}

fn require_bucket_query(query: &OwnerBucketQuery) -> Result<String, &'static str> {
    let bucket = query
        .bucket
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or("bucket query parameter is required")?;
    Ok(bucket)
}

fn reject_legacy_owner(owner: Option<&str>, message: &'static str) -> Result<(), &'static str> {
    if owner.is_some_and(|value| !value.trim().is_empty()) {
        return Err(message);
    }
    Ok(())
}

fn filter_balances(
    body: BalancesResponse,
    internal_bucket: &str,
    external_bucket: &str,
) -> BalancesResponse {
    let accounts = body
        .accounts
        .into_iter()
        .filter(|account| account.bucket == internal_bucket)
        .map(|mut account| {
            account.bucket = external_bucket.to_string();
            account
        })
        .collect::<Vec<_>>();
    let total_balance = accounts.iter().map(|account| account.balance).sum();
    BalancesResponse {
        accounts,
        total_balance,
    }
}

fn filter_collapsed(
    body: BTreeMap<String, CollapsedBalance>,
    internal_bucket: &str,
    external_bucket: &str,
) -> BTreeMap<String, CollapsedBalance> {
    let prefix = format!("{internal_bucket}:");
    body.into_iter()
        .filter_map(|(key, value)| {
            key.strip_prefix(&prefix)
                .map(|account| (format!("{external_bucket}:{account}"), value))
        })
        .collect()
}

fn pagination(
    page: Option<usize>,
    limit: Option<usize>,
    default_limit: usize,
    max_limit: usize,
) -> (usize, usize, usize) {
    let page = page.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(default_limit).clamp(1, max_limit);
    let offset = (page - 1).saturating_mul(limit);
    (page, limit, offset)
}

fn normalized_query(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn event_matches_query(event: &Event, needle: &str) -> bool {
    contains_case_insensitive(&event.event_id, needle)
        || contains_case_insensitive(&event.account, needle)
        || contains_case_insensitive(&event.r#type.to_string(), needle)
        || contains_case_insensitive(&event.amount.to_string(), needle)
        || event
            .note
            .as_deref()
            .is_some_and(|value| contains_case_insensitive(value, needle))
        || contains_case_insensitive(&event.idempotency_nonce, needle)
}

fn summarize_user_buckets(
    user_id: Uuid,
    balances: BalancesResponse,
    events: EventsResponse,
) -> Vec<InternalBucketSummary> {
    let mut aggregates = BTreeMap::<String, BucketAggregate>::new();

    for account in balances.accounts {
        let Some(bucket) = external_bucket_from_internal_user(user_id, &account.bucket) else {
            continue;
        };
        let entry = aggregates.entry(bucket).or_default();
        entry.total_balance += account.balance;
        entry.available_balance += account.available_balance;
        entry.active_hold_total += account.active_hold_total;
        entry.account_count += 1;
        entry.balance_event_count += account.event_count;
    }

    for event in events.events {
        let Some(bucket) = external_bucket_from_internal_user(user_id, &event.bucket) else {
            continue;
        };
        let entry = aggregates.entry(bucket).or_default();
        entry.observed_event_count += 1;
        entry.last_event_at_unix_ms = Some(
            entry
                .last_event_at_unix_ms
                .map_or(event.created_at_unix_ms, |current| {
                    current.max(event.created_at_unix_ms)
                }),
        );
    }

    aggregates
        .into_iter()
        .map(|(bucket, aggregate)| InternalBucketSummary {
            bucket,
            total_balance: aggregate.total_balance,
            available_balance: aggregate.available_balance,
            active_hold_total: aggregate.active_hold_total,
            account_count: aggregate.account_count,
            event_count: if aggregate.observed_event_count > 0 {
                aggregate.observed_event_count
            } else {
                aggregate.balance_event_count
            },
            last_event_at_unix_ms: aggregate.last_event_at_unix_ms,
        })
        .collect()
}

async fn prometheus_metrics(State(state): State<AppState>) -> Response {
    let health = build_gateway_health(&state);
    let nodes = state.mesh.all_nodes();
    let edge_label = state
        .public_edges
        .as_ref()
        .and_then(|d| d.self_edge.label.clone())
        .unwrap_or_else(|| {
            health
                .edge_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string())
        });
    let edge_id = health.edge_id.as_deref().unwrap_or("unknown");
    let region = health.region.as_deref().unwrap_or("unknown");

    let mut out = String::with_capacity(2048);
    out.push_str("# HELP shardd_edge_ready Whether this edge is ready to serve requests.\n");
    out.push_str("# TYPE shardd_edge_ready gauge\n");
    out.push_str(&format!(
        "shardd_edge_ready{{edge=\"{edge_label}\",edge_id=\"{edge_id}\",region=\"{region}\"}} {}\n",
        if health.ready { 1 } else { 0 }
    ));

    out.push_str("# HELP shardd_edge_discovered_nodes Number of mesh nodes discovered.\n");
    out.push_str("# TYPE shardd_edge_discovered_nodes gauge\n");
    out.push_str(&format!("shardd_edge_discovered_nodes{{edge=\"{edge_label}\",edge_id=\"{edge_id}\",region=\"{region}\"}} {}\n", health.discovered_nodes));

    out.push_str("# HELP shardd_edge_healthy_nodes Number of healthy mesh nodes.\n");
    out.push_str("# TYPE shardd_edge_healthy_nodes gauge\n");
    out.push_str(&format!("shardd_edge_healthy_nodes{{edge=\"{edge_label}\",edge_id=\"{edge_id}\",region=\"{region}\"}} {}\n", health.healthy_nodes));

    out.push_str(
        "# HELP shardd_edge_best_node_rtt_ms RTT to the best mesh node in milliseconds.\n",
    );
    out.push_str("# TYPE shardd_edge_best_node_rtt_ms gauge\n");
    out.push_str(&format!("shardd_edge_best_node_rtt_ms{{edge=\"{edge_label}\",edge_id=\"{edge_id}\",region=\"{region}\"}} {}\n", health.best_node_rtt_ms.unwrap_or(0)));

    out.push_str("# HELP shardd_edge_sync_gap Sync gap of the best mesh node.\n");
    out.push_str("# TYPE shardd_edge_sync_gap gauge\n");
    out.push_str(&format!("shardd_edge_sync_gap{{edge=\"{edge_label}\",edge_id=\"{edge_id}\",region=\"{region}\"}} {}\n", health.sync_gap.unwrap_or(0)));

    out.push_str("# HELP shardd_edge_overloaded Whether the best mesh node is overloaded.\n");
    out.push_str("# TYPE shardd_edge_overloaded gauge\n");
    out.push_str(&format!("shardd_edge_overloaded{{edge=\"{edge_label}\",edge_id=\"{edge_id}\",region=\"{region}\"}} {}\n", if health.overloaded == Some(true) { 1 } else { 0 }));

    // Build node_id → label lookup from public edges config
    let node_labels: std::collections::HashMap<String, String> = state
        .public_edges
        .as_ref()
        .map(|dir| {
            let mut map = std::collections::HashMap::new();
            for edge in std::iter::once(&dir.self_edge).chain(dir.known_edges.iter()) {
                if let Some(nid) = &edge.node_id {
                    let label = edge
                        .node_label
                        .clone()
                        .unwrap_or_else(|| edge.edge_id.to_uppercase());
                    map.insert(nid.clone(), label);
                }
            }
            map
        })
        .unwrap_or_default();

    out.push_str(
        "# HELP shardd_node_ping_rtt_ms Ping RTT to each discovered mesh node in milliseconds.\n",
    );
    out.push_str("# TYPE shardd_node_ping_rtt_ms gauge\n");
    out.push_str("# HELP shardd_node_sync_gap_per_bucket Per-bucket sync gap on each mesh node.\n");
    out.push_str("# TYPE shardd_node_sync_gap_per_bucket gauge\n");
    for node in &nodes {
        let rtt = node.ping_rtt.map(|d| d.as_millis() as u64).unwrap_or(0);
        let node_ready = node.health.as_ref().map(|h| h.ready).unwrap_or(false);
        let node_gap = node.health.as_ref().map(|h| h.sync_gap).unwrap_or(0);
        let label = node_labels
            .get(&node.node_id)
            .cloned()
            .unwrap_or_else(|| node.node_id[..8.min(node.node_id.len())].to_string());
        out.push_str(&format!("shardd_node_ping_rtt_ms{{edge=\"{edge_label}\",node=\"{label}\",ready=\"{node_ready}\"}} {rtt}\n"));
        out.push_str(&format!(
            "shardd_node_sync_gap{{edge=\"{edge_label}\",node=\"{label}\"}} {node_gap}\n"
        ));
        // One time-series per bucket. Escape quotes and backslashes in
        // bucket names per Prometheus text-format rules.
        if let Some(per_bucket) = node.health.as_ref().map(|h| &h.sync_gap_per_bucket) {
            for (bucket, gap) in per_bucket {
                let escaped = bucket.replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!(
                    "shardd_node_sync_gap_per_bucket{{edge=\"{edge_label}\",node=\"{label}\",bucket=\"{escaped}\"}} {gap}\n"
                ));
            }
        }
    }

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/plain; version=0.0.4; charset=utf-8".parse().unwrap(),
    );
    (headers, out).into_response()
}

fn build_gateway_health(state: &AppState) -> PublicEdgeHealthResponse {
    let nodes = state.mesh.all_nodes();
    let healthy_nodes = nodes.iter().filter(|node| node_is_healthy(node)).count();
    let best_node = state.mesh.best_node();
    PublicEdgeHealthResponse {
        observed_at_unix_ms: now_ms(),
        edge_id: state
            .public_edges
            .as_ref()
            .map(|directory| directory.self_edge.edge_id.clone()),
        region: state
            .public_edges
            .as_ref()
            .map(|directory| directory.self_edge.region.clone()),
        base_url: state
            .public_edges
            .as_ref()
            .map(|directory| directory.self_edge.base_url.clone()),
        ready: healthy_nodes > 0,
        discovered_nodes: nodes.len(),
        healthy_nodes,
        best_node_rtt_ms: best_node
            .as_ref()
            .and_then(|node| node.ping_rtt.map(|rtt| rtt.as_millis() as u64)),
        sync_gap: best_node
            .as_ref()
            .and_then(|node| node.health.as_ref().map(|health| health.sync_gap)),
        overloaded: best_node
            .as_ref()
            .and_then(|node| node.health.as_ref().map(|health| health.overloaded)),
        auth_enabled: state.auth.is_some(),
    }
}

async fn build_public_edge_directory_response(
    state: &AppState,
    directory: &PublicEdgeDirectory,
) -> PublicEdgeDirectoryResponse {
    let self_health = build_gateway_health(state);
    let mut edges = Vec::with_capacity(directory.known_edges.len());

    for edge in &directory.known_edges {
        if edge.edge_id == directory.self_edge.edge_id
            && edge.base_url == directory.self_edge.base_url
        {
            edges.push(summary_from_health(edge, &self_health));
            continue;
        }
        edges.push(fetch_public_edge_summary(directory, edge).await);
    }

    PublicEdgeDirectoryResponse {
        observed_at_unix_ms: now_ms(),
        edges,
    }
}

async fn fetch_public_edge_summary(
    directory: &PublicEdgeDirectory,
    edge: &PublicEdgeConfig,
) -> PublicEdgeSummary {
    let response = directory.http.get(edge.health_url()).send().await;
    let Ok(response) = response else {
        return unreachable_public_edge_summary(edge);
    };
    if !response.status().is_success() {
        return unreachable_public_edge_summary(edge);
    }
    let Ok(health) = response.json::<PublicEdgeHealthResponse>().await else {
        return unreachable_public_edge_summary(edge);
    };
    summary_from_health(edge, &health)
}

fn summary_from_health(
    edge: &PublicEdgeConfig,
    health: &PublicEdgeHealthResponse,
) -> PublicEdgeSummary {
    PublicEdgeSummary {
        edge_id: edge.edge_id.clone(),
        region: edge.region.clone(),
        base_url: edge.base_url.clone(),
        health_url: edge.health_url(),
        reachable: true,
        ready: health.ready,
        observed_at_unix_ms: Some(health.observed_at_unix_ms),
        discovered_nodes: Some(health.discovered_nodes),
        healthy_nodes: Some(health.healthy_nodes),
        best_node_rtt_ms: health.best_node_rtt_ms,
        sync_gap: health.sync_gap,
        overloaded: health.overloaded,
    }
}

fn unreachable_public_edge_summary(edge: &PublicEdgeConfig) -> PublicEdgeSummary {
    PublicEdgeSummary {
        edge_id: edge.edge_id.clone(),
        region: edge.region.clone(),
        base_url: edge.base_url.clone(),
        health_url: edge.health_url(),
        reachable: false,
        ready: false,
        observed_at_unix_ms: None,
        discovered_nodes: None,
        healthy_nodes: None,
        best_node_rtt_ms: None,
        sync_gap: None,
        overloaded: None,
    }
}

fn internal_bucket_for_user(user_id: Uuid, bucket: &str) -> String {
    format!(
        "user_{}__bucket_{}",
        sanitize_namespace_value(&user_id.to_string()),
        hex::encode(bucket.as_bytes())
    )
}

fn external_bucket_from_internal_user(user_id: Uuid, internal_bucket: &str) -> Option<String> {
    let encoded = internal_bucket.strip_prefix(&internal_bucket_prefix(user_id))?;
    let bytes = hex::decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

fn internal_bucket_prefix(user_id: Uuid) -> String {
    format!(
        "user_{}__bucket_",
        sanitize_namespace_value(&user_id.to_string())
    )
}

fn sanitize_namespace_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn auth_cache_key(api_key: &str, action: &GatewayMachineAction, bucket: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hasher.update(b"\0");
    match action {
        GatewayMachineAction::Read => hasher.update(b"read"),
        GatewayMachineAction::Write => hasher.update(b"write"),
    }
    hasher.update(b"\0");
    hasher.update(bucket.as_bytes());
    hex::encode(hasher.finalize())
}

fn now_ms() -> u64 {
    Event::now_ms()
}

fn expect_health(response: NodeRpcResponse) -> Result<HealthResponse> {
    match response {
        NodeRpcResponse::Health(body) => Ok(body),
        other => Err(anyhow!("unexpected response for health: {other:?}")),
    }
}

fn expect_state(response: NodeRpcResponse) -> Result<StateResponse> {
    match response {
        NodeRpcResponse::State(body) => Ok(body),
        other => Err(anyhow!("unexpected response for state: {other:?}")),
    }
}

fn expect_events(response: NodeRpcResponse) -> Result<EventsResponse> {
    match response {
        NodeRpcResponse::Events(body) => Ok(body),
        other => Err(anyhow!("unexpected response for events: {other:?}")),
    }
}

fn expect_heads(response: NodeRpcResponse) -> Result<BTreeMap<String, u64>> {
    match response {
        NodeRpcResponse::Heads(body) => Ok(body),
        other => Err(anyhow!("unexpected response for heads: {other:?}")),
    }
}

fn expect_balances(response: NodeRpcResponse) -> Result<BalancesResponse> {
    match response {
        NodeRpcResponse::Balances(body) => Ok(body),
        other => Err(anyhow!("unexpected response for balances: {other:?}")),
    }
}

fn expect_collapsed(response: NodeRpcResponse) -> Result<BTreeMap<String, CollapsedBalance>> {
    match response {
        NodeRpcResponse::Collapsed(body) => Ok(body),
        other => Err(anyhow!("unexpected response for collapsed: {other:?}")),
    }
}

fn expect_collapsed_account(response: NodeRpcResponse) -> Result<CollapsedBalance> {
    match response {
        NodeRpcResponse::CollapsedAccount(body) => Ok(body),
        other => Err(anyhow!(
            "unexpected response for collapsed account: {other:?}"
        )),
    }
}

fn expect_persistence(response: NodeRpcResponse) -> Result<PersistenceStats> {
    match response {
        NodeRpcResponse::Persistence(body) => Ok(body),
        other => Err(anyhow!("unexpected response for persistence: {other:?}")),
    }
}

fn expect_digests(response: NodeRpcResponse) -> Result<BTreeMap<String, DigestInfo>> {
    match response {
        NodeRpcResponse::Digests(body) => Ok(body),
        other => Err(anyhow!("unexpected response for digests: {other:?}")),
    }
}

fn expect_debug_origin(response: NodeRpcResponse) -> Result<DebugOriginResponse> {
    match response {
        NodeRpcResponse::DebugOrigin(body) => Ok(body),
        other => Err(anyhow!("unexpected response for debug origin: {other:?}")),
    }
}

fn expect_registry(response: NodeRpcResponse) -> Result<Vec<NodeRegistryEntry>> {
    match response {
        NodeRpcResponse::Registry(body) => Ok(body),
        other => Err(anyhow!("unexpected response for registry: {other:?}")),
    }
}

fn rpc_error_response(node: &MeshNode, error: NodeRpcError) -> Response {
    match error.code {
        NodeRpcErrorCode::ServiceUnavailable => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            node,
            &serde_json::json!({
                "error": error.message
            }),
        ),
        NodeRpcErrorCode::InsufficientFunds => json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            node,
            &error
                .insufficient_funds
                .expect("insufficient funds payload must be present"),
        ),
        NodeRpcErrorCode::InvalidInput => json_response(
            StatusCode::BAD_REQUEST,
            node,
            &serde_json::json!({
                "error": error.message
            }),
        ),
        NodeRpcErrorCode::NotFound => json_response(
            StatusCode::NOT_FOUND,
            node,
            &serde_json::json!({
                "error": error.message
            }),
        ),
        NodeRpcErrorCode::Internal => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            node,
            &serde_json::json!({
                "error": error.message
            }),
        ),
    }
}

fn gateway_unavailable_response(message: String) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn gateway_internal_response(node: Option<&MeshNode>, message: String) -> Response {
    let mut response = (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response();
    if let Some(node) = node {
        attach_node_headers(response.headers_mut(), node);
    }
    response
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn not_exposed_response(feature: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("{feature} is not exposed on the authenticated edge gateway")
        })),
    )
        .into_response()
}

fn json_response<T: Serialize>(status: StatusCode, node: &MeshNode, body: &T) -> Response {
    let mut response = (status, Json(body)).into_response();
    attach_node_headers(response.headers_mut(), node);
    response
}

fn attach_node_headers(headers: &mut HeaderMap, node: &MeshNode) {
    insert_header(headers, "x-shardd-target-node-id", &node.node_id);
    insert_header(headers, "x-shardd-target-peer-id", &node.peer_id);
    if let Some(addr) = &node.advertise_addr {
        insert_header(headers, "x-shardd-target-advertise-addr", addr);
    }
    if let Some(rtt) = node.ping_rtt {
        insert_header(
            headers,
            "x-shardd-target-rtt-ms",
            &rtt.as_millis().to_string(),
        );
    }
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    let header_name = HeaderName::from_static(name);
    let Ok(header_value) = HeaderValue::from_str(value) else {
        return;
    };
    headers.insert(header_name, header_value);
}

fn summarize_node(node: MeshNode) -> GatewayNodeSummary {
    GatewayNodeSummary {
        node_id: node.node_id,
        peer_id: node.peer_id,
        advertise_addr: node.advertise_addr,
        listen_addrs: node.listen_addrs.iter().map(ToString::to_string).collect(),
        ping_rtt_ms: node.ping_rtt.map(|rtt| rtt.as_millis() as u64),
        ready: node.health.as_ref().map(|health| health.ready),
        sync_gap: node.health.as_ref().map(|health| health.sync_gap),
        overloaded: node.health.as_ref().map(|health| health.overloaded),
        inflight_requests: node.health.as_ref().map(|health| health.inflight_requests),
        failure_count: node.failure_count,
        is_best: false,
    }
}

fn summarize_nodes_with_best(state: &AppState) -> Vec<GatewayNodeSummary> {
    let best_peer_id = state.mesh.best_node().map(|n| n.peer_id);
    state
        .mesh
        .all_nodes()
        .into_iter()
        .map(|node| {
            let mut summary = summarize_node(node);
            if let Some(best) = best_peer_id.as_ref() {
                summary.is_best = &summary.peer_id == best;
            }
            summary
        })
        .collect()
}

/// How long after its last observation we still trust a node's
/// cached health status. Past this, we treat the node as unknown
/// (i.e. unhealthy for routing) even if the snapshot said ready.
///
/// Kept comfortably larger than the mesh client's `health_interval`
/// (currently 1s) so a single dropped probe never flaps this.
const HEALTH_OBSERVATION_MAX_AGE_MS: u64 = 15_000;

fn node_is_healthy(node: &MeshNode) -> bool {
    let Some(health) = node.health.as_ref() else {
        return false;
    };
    if !health.ready || health.overloaded {
        return false;
    }
    // Reject a stale snapshot: if we haven't heard a fresh health
    // response within the window, treat the node as unknown.
    match node.last_health_at_unix_ms {
        Some(last) => now_ms().saturating_sub(last) <= HEALTH_OBSERVATION_MAX_AGE_MS,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use shardd_types::{AccountBalance, CollapsedBalance};
    use tower::ServiceExt;

    fn sample_user_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000123").unwrap()
    }

    fn empty_state() -> AppState {
        // MeshClient now hard-requires PSK and identity_seed — feed it a
        // deterministic test configuration so the test harness builds a
        // valid mesh client without touching the network.
        let mut config = MeshClientConfig::new(Vec::new());
        config.psk = Some([0xAB; 32]);
        config.identity_seed = "test-gateway".to_string();
        let mesh = Arc::new(MeshClient::start(config).expect("mesh client starts"));
        AppState {
            mesh,
            auth: None,
            public_edges: None,
        }
    }

    fn public_edge_state() -> AppState {
        let mut state = empty_state();
        state.public_edges = Some(Arc::new(
            PublicEdgeDirectory::new(
                PublicEdgeConfig::new(
                    "use1".into(),
                    "us-east-1".into(),
                    "https://use1.api.dev.example.com".into(),
                )
                .expect("public edge config"),
                Vec::new(),
            )
            .expect("public edge directory"),
        ));
        state
    }

    fn authenticated_state() -> AppState {
        let mut state = public_edge_state();
        state.auth = Some(Arc::new(
            GatewayAuthClient::new("https://app.dev.example.com".into(), "secret".into())
                .expect("auth client"),
        ));
        state
    }

    #[tokio::test]
    async fn collapsed_account_route_matches_before_summary_route() {
        let app = build_app(empty_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/collapsed/smoke/alice")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn collapsed_summary_route_still_matches() {
        let app = build_app(empty_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/collapsed")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(json.contains("no libp2p nodes discovered"));
    }

    #[tokio::test]
    async fn debug_origin_route_matches() {
        let app = build_app(empty_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/debug/origin/test-origin")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn gateway_health_exposes_public_edge_metadata() {
        let app = build_app(public_edge_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/gateway/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let health: PublicEdgeHealthResponse =
            serde_json::from_slice(&body).expect("health response");
        assert_eq!(health.edge_id.as_deref(), Some("use1"));
        assert_eq!(health.region.as_deref(), Some("us-east-1"));
        assert_eq!(
            health.base_url.as_deref(),
            Some("https://use1.api.dev.example.com")
        );
        assert!(!health.ready);
        assert_eq!(health.discovered_nodes, 0);
        assert_eq!(health.healthy_nodes, 0);
    }

    #[tokio::test]
    async fn gateway_edges_returns_public_edge_directory() {
        let app = build_app(public_edge_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/gateway/edges")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let directory: PublicEdgeDirectoryResponse =
            serde_json::from_slice(&body).expect("directory response");
        assert_eq!(directory.edges.len(), 1);
        assert_eq!(directory.edges[0].edge_id, "use1");
        assert_eq!(directory.edges[0].region, "us-east-1");
        assert_eq!(
            directory.edges[0].base_url,
            "https://use1.api.dev.example.com"
        );
        assert_eq!(
            directory.edges[0].health_url,
            "https://use1.api.dev.example.com/gateway/health"
        );
        assert!(directory.edges[0].reachable);
        assert!(!directory.edges[0].ready);
    }

    #[tokio::test]
    async fn gateway_nodes_is_hidden_on_authenticated_gateway() {
        let app = build_app(authenticated_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/gateway/nodes")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(json.contains("gateway nodes is not exposed"));
    }

    #[test]
    fn internal_bucket_encoding_avoids_colons() {
        let bucket = internal_bucket_for_user(sample_user_id(), "orders/eu");
        assert!(!bucket.contains(':'));
        assert!(bucket.starts_with("user_00000000-0000-0000-0000-000000000123__bucket_"));
    }

    #[test]
    fn internal_bucket_round_trips_external_name() {
        let internal = internal_bucket_for_user(sample_user_id(), "orders/eu");
        assert_eq!(
            external_bucket_from_internal_user(sample_user_id(), &internal).as_deref(),
            Some("orders/eu")
        );
    }

    #[test]
    fn balance_filter_rewrites_bucket_and_total() {
        let filtered = filter_balances(
            BalancesResponse {
                accounts: vec![
                    AccountBalance {
                        bucket: internal_bucket_for_user(sample_user_id(), "orders"),
                        account: "alice".into(),
                        balance: 10,
                        available_balance: 7,
                        active_hold_total: 3,
                        reserved_by_origin: BTreeMap::new(),
                        event_count: 1,
                    },
                    AccountBalance {
                        bucket: "other".into(),
                        account: "bob".into(),
                        balance: 20,
                        available_balance: 20,
                        active_hold_total: 0,
                        reserved_by_origin: BTreeMap::new(),
                        event_count: 1,
                    },
                ],
                total_balance: 30,
            },
            &internal_bucket_for_user(sample_user_id(), "orders"),
            "orders",
        );
        assert_eq!(filtered.total_balance, 10);
        assert_eq!(filtered.accounts.len(), 1);
        assert_eq!(filtered.accounts[0].bucket, "orders");
    }

    #[test]
    fn collapsed_filter_uses_external_bucket_key() {
        let internal_bucket = internal_bucket_for_user(sample_user_id(), "orders");
        let filtered = filter_collapsed(
            BTreeMap::from([
                (
                    format!("{internal_bucket}:alice"),
                    CollapsedBalance {
                        balance: 10,
                        available_balance: 9,
                        status: "ok".into(),
                        reserved_by_origin: BTreeMap::new(),
                        contributing_origins: BTreeMap::new(),
                    },
                ),
                (
                    "other:bob".into(),
                    CollapsedBalance {
                        balance: 2,
                        available_balance: 2,
                        status: "ok".into(),
                        reserved_by_origin: BTreeMap::new(),
                        contributing_origins: BTreeMap::new(),
                    },
                ),
            ]),
            &internal_bucket,
            "orders",
        );
        assert_eq!(filtered.len(), 1);
        assert!(filtered.contains_key("orders:alice"));
    }

    #[tokio::test]
    async fn internal_bucket_routes_require_machine_secret() {
        let app = build_app(authenticated_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/internal/users/00000000-0000-0000-0000-000000000123/buckets")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(json.contains("machine auth secret"));
    }

    #[tokio::test]
    async fn owner_query_is_rejected_before_auth() {
        let app = build_app(authenticated_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/balances?owner=dev_123&bucket=orders")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(json.contains("owner query parameter is no longer supported"));
    }

    #[tokio::test]
    async fn owner_body_field_is_rejected_before_auth() {
        let app = build_app(authenticated_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/events")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"owner":"dev_123","bucket":"orders","account":"alice","amount":10}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(json.contains("owner is no longer supported"));
    }
}
