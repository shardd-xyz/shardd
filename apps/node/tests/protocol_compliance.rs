//! Protocol compliance integration tests.
//! Each test starts real node(s) against a test Postgres instance.

use std::time::Duration;

use serde::de::DeserializeOwned;
use shardd_broadcast::discovery::{derive_psk_from_cluster_key, parse_bootstrap_peers};
use shardd_broadcast::mesh_client::{MeshClient, MeshClientConfig};
use shardd_node::{NodeConfig, server};
use shardd_types::*;
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;

static TEST_DATABASE_SERVER: OnceCell<TestDatabaseServer> = OnceCell::const_new();

// ── Test helpers ────────────────────────────────────────────────────

fn test_config(database_url: &str) -> NodeConfig {
    NodeConfig {
        host: "127.0.0.1".into(),
        advertise_addrs: Vec::new(),
        database_url: database_url.to_string(),
        bootstrap: vec![],
        batch_flush_interval_ms: 50,
        batch_flush_size: 100,
        matview_refresh_ms: 60_000,
        orphan_check_interval_ms: 100,
        orphan_age_ms: 100,
        hold_multiplier: 0,
        hold_duration_ms: 0,
        libp2p_port: reserve_port(),
        psk_file: None,
        // shardd-node hard-requires a cluster_key for private mesh + stable
        // libp2p identity. Use a deterministic test key so nodes in the
        // same test run share the same private mesh.
        cluster_key: Some("shardd-test-cluster-key-deterministic".to_string()),
        event_worker_count: 4,
    }
}

#[derive(Clone, Copy)]
struct TestStatus(u16);

impl TestStatus {
    fn as_u16(self) -> u16 {
        self.0
    }
}

struct TestResponse {
    status: TestStatus,
    body: serde_json::Value,
}

impl TestResponse {
    fn status(&self) -> TestStatus {
        self.status
    }

    async fn json<T: DeserializeOwned>(self) -> serde_json::Result<T> {
        serde_json::from_value(self.body)
    }
}

async fn create_event(node: &TestNode, req: &serde_json::Value) -> TestResponse {
    // Every event carries an idempotency_nonce since the "all events
    // deduped" invariant landed. Tests that want to assert dedup pass
    // their own nonce; otherwise auto-inject a fresh one so each test
    // write is independent.
    let mut req = req.clone();
    if let Some(obj) = req.as_object_mut()
        && !obj.contains_key("idempotency_nonce")
    {
        obj.insert(
            "idempotency_nonce".to_string(),
            serde_json::Value::String(uuid::Uuid::new_v4().to_string()),
        );
    }
    let request: CreateEventRequest =
        serde_json::from_value(req).expect("invalid create-event request json");
    rpc_to_test_response(node.request(NodeRpcRequest::CreateEvent(request)).await)
}

async fn get_json(node: &TestNode, path: &str) -> serde_json::Value {
    let request = match path {
        "/events" => NodeRpcRequest::Events,
        "/state" => NodeRpcRequest::State,
        "/registry" => NodeRpcRequest::Registry,
        "/health" => NodeRpcRequest::Health,
        other => panic!("unsupported test RPC path: {other}"),
    };
    rpc_to_body(node.request(request).await)
}

fn rpc_to_test_response(result: NodeRpcResult) -> TestResponse {
    let status = TestStatus(match &result {
        Ok(NodeRpcResponse::CreateEvent(body)) => {
            if body.deduplicated {
                200
            } else {
                201
            }
        }
        Ok(_) => 200,
        Err(error) => match error.code {
            NodeRpcErrorCode::InsufficientFunds => 422,
            NodeRpcErrorCode::InvalidInput => 400,
            NodeRpcErrorCode::ServiceUnavailable => 503,
            NodeRpcErrorCode::NotFound => 404,
            NodeRpcErrorCode::Internal => 500,
        },
    });
    TestResponse {
        status,
        body: rpc_to_body(result),
    }
}

fn rpc_to_body(result: NodeRpcResult) -> serde_json::Value {
    match result {
        Ok(response) => response_to_value(response),
        Err(error) => error_to_value(error),
    }
}

fn response_to_value(response: NodeRpcResponse) -> serde_json::Value {
    match response {
        NodeRpcResponse::CreateEvent(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Health(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::State(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Events(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Heads(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Balances(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Collapsed(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::CollapsedAccount(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Persistence(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Digests(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::DebugOrigin(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::Registry(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::DeleteBucket(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::EventsFilter(body) => serde_json::to_value(body).unwrap(),
        NodeRpcResponse::DeletedBuckets(body) => serde_json::to_value(body).unwrap(),
    }
}

fn error_to_value(error: NodeRpcError) -> serde_json::Value {
    match error.code {
        NodeRpcErrorCode::InsufficientFunds => serde_json::to_value(
            error
                .insufficient_funds
                .expect("insufficient_funds payload must be present"),
        )
        .unwrap(),
        _ => serde_json::json!({ "error": error.message }),
    }
}

struct TestDatabaseServer {
    base_url: String,
    _container: Option<ContainerAsync<Postgres>>,
}

impl TestDatabaseServer {
    async fn resolve() -> &'static Self {
        TEST_DATABASE_SERVER
            .get_or_init(|| async {
                if let Ok(base_url) = std::env::var("TEST_DATABASE_URL") {
                    return Self {
                        base_url,
                        _container: None,
                    };
                }

                let container = Postgres::default()
                    .start()
                    .await
                    .expect("failed to start test postgres container");
                let host = container
                    .get_host()
                    .await
                    .expect("test postgres host should be available");
                let port = container
                    .get_host_port_ipv4(5432)
                    .await
                    .expect("test postgres port should be exposed");

                Self {
                    base_url: format!("postgres://postgres:postgres@{host}:{port}/postgres"),
                    _container: Some(container),
                }
            })
            .await
    }
}

async fn create_test_db(base_url: &str, db_name: &str) -> String {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(base_url)
        .await
        .expect("failed to connect to postgres");

    sqlx::query(&format!("DROP DATABASE IF EXISTS \"{db_name}\""))
        .execute(&pool)
        .await
        .ok();
    sqlx::query(&format!("CREATE DATABASE \"{db_name}\""))
        .execute(&pool)
        .await
        .expect("failed to create test database");

    let base = base_url.rsplit_once('/').unwrap().0;
    format!("{base}/{db_name}")
}

async fn drop_test_db(base_url: &str, db_name: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(base_url)
        .await
        .ok();
    if let Some(pool) = pool {
        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{db_name}\""))
            .execute(&pool)
            .await
            .ok();
    }
}

async fn pg_base_url() -> String {
    TestDatabaseServer::resolve().await.base_url.clone()
}

fn reserve_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("failed to reserve local port")
        .local_addr()
        .expect("reserved port missing local addr")
        .port()
}

async fn start_mesh_client(bootstrap_peers: &[String], min_candidates: usize) -> MeshClient {
    let mut config =
        MeshClientConfig::new(parse_bootstrap_peers(bootstrap_peers).expect("valid bootstrap"));
    config.request_timeout = Duration::from_secs(5);
    config.health_interval = Duration::from_millis(200);
    config.health_ttl = Duration::from_secs(2);
    config.peer_ttl = Duration::from_secs(10);
    // The test harness nodes are started with the same cluster_key, so
    // the mesh client MUST use a matching PSK to join their private mesh.
    // identity_seed is left empty because no cache_path is set → ephemeral
    // keypair, which is fine for a short-lived test.
    config.psk = Some(
        derive_psk_from_cluster_key("shardd-test-cluster-key-deterministic")
            .expect("derive test psk"),
    );
    let client = MeshClient::start(config).expect("mesh client should start");
    client
        .wait_for_min_candidates(min_candidates, Duration::from_secs(8))
        .await
        .expect("mesh client should discover expected nodes");
    client
}

// ── Smoke test ──────────────────────────────────────────────────────

#[tokio::test]
async fn smoke_test_create_and_list_events() {
    let node = TestNode::start().await;

    let resp = create_event(
        &node,
        &serde_json::json!({
            "bucket": "test",
            "account": "user1",
            "amount": 1000
        }),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 201, "expected 201 Created");
    let body: CreateEventResponse = resp.json().await.unwrap();
    assert_eq!(body.balance, 1000);
    assert_eq!(body.event.amount, 1000);
    assert!(!body.deduplicated);

    tokio::time::sleep(Duration::from_millis(200)).await;

    let events: serde_json::Value = get_json(&node, "/events").await;
    let events_arr = events["events"].as_array().unwrap();
    assert_eq!(events_arr.len(), 1);
    assert_eq!(events_arr[0]["amount"].as_i64().unwrap(), 1000);

    let state: serde_json::Value = get_json(&node, "/state").await;
    assert!(
        state["ready"].as_bool().unwrap_or(false),
        "node should be ready"
    );
    assert_eq!(state["event_count"].as_u64().unwrap_or(0), 1);

    node.shutdown().await;
}

// ── Helper: start a test node ───────────────────────────────────────

struct TestNode {
    client: MeshClient,
    libp2p_addr: String,
    handle: Option<shardd_node::NodeHandle>,
    db_name: String,
    base_url: String,
}

impl TestNode {
    async fn start() -> Self {
        Self::start_with(|c| c).await
    }

    async fn start_with(configure: impl FnOnce(NodeConfig) -> NodeConfig) -> Self {
        let base = pg_base_url().await;
        let db_name = format!(
            "shardd_test_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")
        );
        let db_url = create_test_db(&base, &db_name).await;
        let config = configure(test_config(&db_url));
        let handle = server::start_node(config)
            .await
            .expect("failed to start node");
        let libp2p_addr = handle.libp2p_addr().to_string();
        let client = start_mesh_client(std::slice::from_ref(&libp2p_addr), 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        Self {
            client,
            libp2p_addr,
            handle: Some(handle),
            db_name,
            base_url: base,
        }
    }

    async fn request(&self, request: NodeRpcRequest) -> NodeRpcResult {
        self.client
            .request_best(request)
            .await
            .expect("mesh request failed")
    }

    async fn shutdown(mut self) {
        if let Some(h) = self.handle.take() {
            let join = h.shutdown();
            let _ = join.await;
        }
        drop_test_db(&self.base_url, &self.db_name).await;
    }
}

// ── §3: Event Lifecycle ─────────────────────────────────────────────

#[tokio::test]
async fn event_has_all_14_fields() {
    let node = TestNode::start().await;
    let resp = create_event(
        &node,
        &serde_json::json!({
            "bucket": "b", "account": "a", "amount": 100
        }),
    )
    .await;
    let body: serde_json::Value = resp.json().await.unwrap();
    let e = &body["event"];
    assert!(e["event_id"].is_string());
    assert!(e["origin_node_id"].is_string());
    assert!(e["origin_epoch"].is_number());
    assert!(e["origin_seq"].is_number());
    assert!(e["created_at_unix_ms"].is_number());
    assert!(e["type"].is_string());
    assert!(e["bucket"].is_string());
    assert!(e["account"].is_string());
    assert!(e["amount"].is_number());
    assert!(e.get("note").is_some());
    assert!(e.get("idempotency_nonce").is_some());
    assert!(e.get("void_ref").is_some());
    assert!(e.get("hold_amount").is_some());
    assert!(e.get("hold_expires_at_unix_ms").is_some());
    node.shutdown().await;
}

// ── §10: Idempotency ────────────────────────────────────────────────

#[tokio::test]
async fn idempotency_same_nonce_deduplicates() {
    let node = TestNode::start().await;

    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":1000}),
    )
    .await;

    let resp1 = create_event(
        &node,
        &serde_json::json!({
            "bucket":"b","account":"a","amount":-50,"idempotency_nonce":"nonce1"
        }),
    )
    .await;
    assert_eq!(resp1.status().as_u16(), 201);
    let body1: CreateEventResponse = resp1.json().await.unwrap();

    let resp2 = create_event(
        &node,
        &serde_json::json!({
            "bucket":"b","account":"a","amount":-50,"idempotency_nonce":"nonce1"
        }),
    )
    .await;
    assert_eq!(resp2.status().as_u16(), 200, "dedup should return 200");
    let body2: CreateEventResponse = resp2.json().await.unwrap();
    assert!(body2.deduplicated);
    assert_eq!(
        body2.event.event_id, body1.event.event_id,
        "should return same event"
    );

    node.shutdown().await;
}

#[tokio::test]
async fn idempotency_different_amount_not_dedup() {
    let node = TestNode::start().await;
    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":1000}),
    )
    .await;

    let resp1 = create_event(
        &node,
        &serde_json::json!({
            "bucket":"b","account":"a","amount":-50,"idempotency_nonce":"n1"
        }),
    )
    .await;
    let body1: CreateEventResponse = resp1.json().await.unwrap();

    let resp2 = create_event(
        &node,
        &serde_json::json!({
            "bucket":"b","account":"a","amount":-100,"idempotency_nonce":"n1"
        }),
    )
    .await;
    assert_eq!(
        resp2.status().as_u16(),
        201,
        "different amount = different op"
    );
    let body2: CreateEventResponse = resp2.json().await.unwrap();
    assert_ne!(body2.event.event_id, body1.event.event_id);

    node.shutdown().await;
}

// ── §11: Balance Holds ──────────────────────────────────────────────

#[tokio::test]
async fn debit_with_hold_reduces_available_balance() {
    let node = TestNode::start_with(|mut c| {
        c.hold_multiplier = 5;
        c.hold_duration_ms = 600_000;
        c
    })
    .await;

    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":1000}),
    )
    .await;

    let resp = create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":-100}),
    )
    .await;
    let body: CreateEventResponse = resp.json().await.unwrap();
    assert_eq!(body.balance, 900);
    assert!(
        body.available_balance < 900,
        "available should be less than balance due to hold"
    );
    assert_eq!(body.available_balance, 400);
    assert_eq!(body.event.hold_amount, 0);
    assert_eq!(body.event.hold_expires_at_unix_ms, 0);

    node.shutdown().await;
}

// ── §9: Overdraft Guard ─────────────────────────────────────────────

#[tokio::test]
async fn overdraft_guard_rejects_exceeding_debit() {
    let node = TestNode::start().await;
    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":100}),
    )
    .await;

    let resp = create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":-200}),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 422, "should reject overdraft");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"].as_str().unwrap(), "insufficient_funds");

    node.shutdown().await;
}

#[tokio::test]
async fn overdraft_guard_allows_with_max_overdraft() {
    let node = TestNode::start().await;
    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":100}),
    )
    .await;

    let resp = create_event(
        &node,
        &serde_json::json!({
            "bucket":"b","account":"a","amount":-200,"max_overdraft":500
        }),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        201,
        "should allow with max_overdraft"
    );
    let body: CreateEventResponse = resp.json().await.unwrap();
    assert_eq!(body.balance, -100);

    node.shutdown().await;
}

// ── §16: Convergence (single node checksum) ─────────────────────────

#[tokio::test]
async fn checksum_deterministic_after_events() {
    let node = TestNode::start().await;

    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":100}),
    )
    .await;
    create_event(
        &node,
        &serde_json::json!({"bucket":"b","account":"a","amount":-30}),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let state1: serde_json::Value = get_json(&node, "/state").await;
    let state2: serde_json::Value = get_json(&node, "/state").await;

    assert_eq!(
        state1["checksum"], state2["checksum"],
        "checksum should be deterministic"
    );
    assert_eq!(state1["event_count"].as_u64().unwrap(), 2);
    assert_eq!(state1["total_balance"].as_i64().unwrap(), 70);

    node.shutdown().await;
}

// ── §14: Registry CRDT ──────────────────────────────────────────────

#[tokio::test]
async fn self_node_registered_in_registry() {
    let node = TestNode::start().await;

    let registry: Vec<serde_json::Value> = get_json(&node, "/registry")
        .await
        .as_array()
        .unwrap()
        .to_vec();
    assert!(
        !registry.is_empty(),
        "registry should contain at least self"
    );
    let active = registry
        .iter()
        .any(|entry| entry["status"].as_str() == Some("active"));
    assert!(active, "self should be registered as active");

    node.shutdown().await;
}

// ── libp2p smoke test ───────────────────────────────────────────────

#[tokio::test]
async fn libp2p_node_starts_and_serves_events() {
    let node = TestNode::start().await;

    let resp = create_event(
        &node,
        &serde_json::json!({
            "bucket": "libp2p-test",
            "account": "user1",
            "amount": 500
        }),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        201,
        "libp2p node should accept events"
    );
    let body: CreateEventResponse = resp.json().await.unwrap();
    assert_eq!(body.balance, 500);

    tokio::time::sleep(Duration::from_millis(200)).await;

    let events: serde_json::Value = get_json(&node, "/events").await;
    assert_eq!(events["events"].as_array().unwrap().len(), 1);

    let health: HealthResponse = serde_json::from_value(get_json(&node, "/health").await).unwrap();
    assert!(health.ready);

    node.shutdown().await;
}

#[tokio::test]
async fn libp2p_discovery_finds_mesh_nodes_and_orders_by_rtt() {
    let node1 = TestNode::start_with(|mut config| {
        config.libp2p_port = reserve_port();
        config
    })
    .await;

    let node2 = TestNode::start_with(|mut config| {
        config.libp2p_port = reserve_port();
        config.bootstrap = vec![node1.libp2p_addr.clone()];
        config
    })
    .await;

    let client = start_mesh_client(std::slice::from_ref(&node1.libp2p_addr), 2).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let nodes = client.all_nodes();
    let addrs: Vec<String> = nodes
        .iter()
        .filter_map(|node| node.advertise_addr.clone())
        .collect();

    assert!(
        addrs.contains(&node1.libp2p_addr),
        "discovery should include node1 libp2p address"
    );
    assert!(
        addrs.contains(&node2.libp2p_addr),
        "discovery should include node2 libp2p address"
    );
    assert!(
        nodes.iter().all(|node| node.ping_rtt.is_some()),
        "every discovered node should have a measured ping"
    );
    for pair in nodes.windows(2) {
        assert!(
            pair[0].ping_rtt <= pair[1].ping_rtt,
            "discovered nodes should be sorted by increasing RTT"
        );
    }

    drop(client);
    node2.shutdown().await;
    node1.shutdown().await;
}
