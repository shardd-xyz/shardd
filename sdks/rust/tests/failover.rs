//! Failover integration test.
//!
//! Runs only when `SHARDD_FAILOVER_GATEWAYS` is set — a comma-separated
//! list of local gateway URLs, e.g.
//!   SHARDD_FAILOVER_GATEWAYS=http://127.0.0.1:8081,http://127.0.0.1:8082,http://127.0.0.1:8083
//!
//! `./run sdk:test:failover` spins up the 3-gateway docker harness,
//! sets this var, and runs the test.
//!
//! Exercises:
//!   * all-healthy edge selection + write + idempotent replay
//!   * failover when the first URL is a closed port
//!   * single-survivor failover
//!   * mid-test mesh outage when `SHARDD_FAILOVER_KILLED_GATEWAY` is set

use std::env;

use shardd::{Client, CreateEventOptions};

fn bucket() -> String {
    env::var("SHARDD_FAILOVER_BUCKET").unwrap_or_else(|_| "failover-test".into())
}

fn gateways() -> Option<Vec<String>> {
    env::var("SHARDD_FAILOVER_GATEWAYS").ok().and_then(|v| {
        let parts: Vec<String> = v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() { None } else { Some(parts) }
    })
}

#[tokio::test]
async fn all_healthy_probe_picks_one_and_writes_succeed() {
    let Some(edges) = gateways() else {
        eprintln!("skipping: set SHARDD_FAILOVER_GATEWAYS to run");
        return;
    };
    let client = Client::builder()
        .api_key("local-dev".into())
        .edges(edges)
        .build()
        .unwrap();

    let first = client
        .create_event(
            &bucket(),
            "alice",
            10,
            CreateEventOptions {
                note: Some("failover test: phase A".into()),
                ..Default::default()
            },
        )
        .await
        .expect("create_event should succeed with all edges healthy");
    assert_eq!(first.deduplicated, false, "first write is not a retry");

    // Replay with the same nonce — must dedupe even though the SDK
    // may pin a different edge on this call.
    let replay = client
        .create_event(
            &bucket(),
            "alice",
            10,
            CreateEventOptions {
                idempotency_nonce: Some(first.event.idempotency_nonce.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("replay should succeed");
    assert_eq!(first.event.event_id, replay.event.event_id);
    assert!(replay.deduplicated, "replay must dedupe");
}

#[tokio::test]
async fn closed_port_mixed_in_is_skipped_by_probe() {
    let Some(mut edges) = gateways() else {
        return;
    };
    // Port 1 on localhost is guaranteed unreachable — simulates a
    // gateway that vanished between probe cycles.
    edges.insert(0, "http://127.0.0.1:1".into());
    let client = Client::builder()
        .api_key("local-dev".into())
        .edges(edges)
        .build()
        .unwrap();
    let result = client
        .create_event(
            &bucket(),
            "bob",
            5,
            CreateEventOptions {
                note: Some("failover test: phase B".into()),
                ..Default::default()
            },
        )
        .await
        .expect("SDK must skip the dead URL and pick a healthy edge");
    assert_eq!(result.deduplicated, false);
}

#[tokio::test]
async fn single_survivor_still_succeeds() {
    let Some(gateways) = gateways() else {
        return;
    };
    let survivor = gateways
        .first()
        .cloned()
        .expect("need at least one gateway URL");
    let edges = vec![
        "http://127.0.0.1:1".into(),
        "http://127.0.0.1:2".into(),
        survivor,
    ];
    let client = Client::builder()
        .api_key("local-dev".into())
        .edges(edges)
        .build()
        .unwrap();
    let result = client
        .create_event(
            &bucket(),
            "carol",
            7,
            CreateEventOptions {
                note: Some("failover test: phase C".into()),
                ..Default::default()
            },
        )
        .await
        .expect("write must route to the only healthy edge");
    assert_eq!(result.deduplicated, false);
}

/// Executed only when the harness stopped one of the three gateways
/// mid-run (via `docker compose stop gateway2`). `SHARDD_FAILOVER_GATEWAYS`
/// still lists all 3 URLs; the SDK must mark the dead one cool and
/// succeed on another.
#[tokio::test]
async fn mid_test_outage_does_not_break_writes() {
    if env::var("SHARDD_FAILOVER_KILLED_GATEWAY").is_err() {
        return;
    }
    let Some(edges) = gateways() else {
        return;
    };
    let client = Client::builder()
        .api_key("local-dev".into())
        .edges(edges)
        .build()
        .unwrap();
    let result = client
        .create_event(
            &bucket(),
            "dan",
            3,
            CreateEventOptions {
                note: Some("failover test: phase D — mid-outage".into()),
                ..Default::default()
            },
        )
        .await
        .expect("write must fail over from the killed edge");
    assert_eq!(result.deduplicated, false);
}
