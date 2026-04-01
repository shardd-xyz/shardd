use std::sync::Arc;

use shardd_broadcast::memory::InMemoryBus;
use shardd_broadcast::Broadcaster;
use shardd_storage::memory::InMemoryStorage;
use shardd_storage::StorageBackend;
use shardd_types::NodeMeta;

use crate::peer::PeerSet;
use crate::state::SharedState;

async fn make_node(node_id: &str, addr: &str, max_peers: usize) -> SharedState<InMemoryStorage> {
    let storage = InMemoryStorage::new();
    storage.save_node_meta(&NodeMeta {
        node_id: node_id.to_string(),
        host: "127.0.0.1".to_string(),
        port: 0,
        next_seq: 1,
    }).await.unwrap();

    let peers = PeerSet::new(max_peers, addr.to_string());
    let bus = InMemoryBus::new(10000);
    let broadcaster: Arc<dyn Broadcaster> = Arc::new(bus.broadcaster());
    let (batch_tx, _batch_rx) = tokio::sync::mpsc::unbounded_channel();

    SharedState::new(
        node_id.to_string(), addr.to_string(), 1, peers,
        storage, batch_tx, broadcaster,
    ).await
}

async fn sync_one_direction<S: StorageBackend>(
    src: &SharedState<S>, dst: &SharedState<S>,
) -> usize {
    let src_heads = src.get_heads();
    let dst_heads = dst.get_heads();
    let mut events = Vec::new();
    for (origin, &src_head) in &src_heads {
        let dst_head = dst_heads.get(origin).copied().unwrap_or(0);
        if src_head > dst_head {
            events.extend(src.get_events_range(origin, dst_head + 1, src_head).await);
        }
    }
    if events.is_empty() { 0 } else { dst.insert_events_batch(events).await }
}

async fn full_mesh_sync(nodes: &[SharedState<InMemoryStorage>]) -> usize {
    let mut rounds = 0;
    loop {
        rounds += 1;
        let mut total = 0usize;
        for i in 0..nodes.len() {
            for j in 0..nodes.len() {
                if i != j {
                    total += sync_one_direction(&nodes[j], &nodes[i]).await;
                }
            }
        }
        if total == 0 { break; }
    }
    rounds
}

// ── Multi-node sync tests ────────────────────────────────────────────

#[tokio::test]
async fn four_nodes_400rps_overdraft_guard_bypass() {
    const N: usize = 4;
    const EPN: usize = 400;
    let mut nodes = Vec::new();
    for i in 0..N {
        let node = make_node(&format!("node-{i}"), &format!("127.0.0.1:{}", 9000+i), N).await;
        node.create_local_event("default".into(), "shared".into(), 10_000, Some("credit".into()), None, 0, 0).await.unwrap();
        nodes.push(node);
    }

    let mut accepted = vec![0usize; N];
    for (i, node) in nodes.iter().enumerate() {
        for _ in 0..EPN {
            if node.create_local_event("default".into(), "shared".into(), -10, None, None, 0, 0).await.is_ok() {
                accepted[i] += 1;
            }
        }
    }
    for i in 0..N { assert_eq!(accepted[i], EPN); }

    full_mesh_sync(&nodes).await;
    let global = 10_000i64 * N as i64 + (N * EPN) as i64 * -10;
    for (i, node) in nodes.iter().enumerate() {
        assert_eq!(node.account_balance("default", "shared"), global, "node-{i} balance mismatch");
    }
    let mut checksums = Vec::new();
    for node in &nodes { checksums.push(node.checksum().await); }
    for i in 1..checksums.len() {
        assert_eq!(checksums[0], checksums[i], "checksum mismatch node-0 vs node-{i}");
    }
}

#[tokio::test]
async fn four_nodes_400rps_single_credit_overdraft_breach() {
    const N: usize = 4;
    const EPN: usize = 400;
    let mut nodes = Vec::new();
    for i in 0..N {
        nodes.push(make_node(&format!("node-{i}"), &format!("127.0.0.1:{}", 9000+i), N).await);
    }
    nodes[0].create_local_event("default".into(), "shared".into(), 10_000, Some("credit".into()), None, 0, 0).await.unwrap();

    let mut accepted = vec![0usize; N];
    for (i, node) in nodes.iter().enumerate() {
        for _ in 0..EPN {
            if node.create_local_event("default".into(), "shared".into(), -10, None, Some(5000), 0, 0).await.is_ok() {
                accepted[i] += 1;
            }
        }
    }
    for i in 0..N { assert_eq!(accepted[i], EPN); }

    let true_balance = 10_000i64 + (N * EPN) as i64 * -10;
    assert!(true_balance < -5000);

    full_mesh_sync(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        assert_eq!(node.account_balance("default", "shared"), true_balance, "node-{i}");
    }
}

// ── Single-node overdraft tests ──────────────────────────────────────

#[tokio::test]
async fn debit_denied_when_balance_insufficient() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None, 0, 0).await.unwrap();
    let result = node.create_local_event("default".into(), "main".into(), -200, None, None, 0, 0).await;
    assert!(result.is_err());
    let (balance, projected) = result.unwrap_err();
    assert_eq!(balance, 100);
    assert_eq!(projected, -100);
    assert_eq!(node.account_balance("default", "main"), 100);
}

#[tokio::test]
async fn debit_allowed_within_overdraft_limit() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None, 0, 0).await.unwrap();
    assert!(node.create_local_event("default".into(), "main".into(), -200, None, Some(200), 0, 0).await.is_ok());
    assert_eq!(node.account_balance("default", "main"), -100);
}

#[tokio::test]
async fn debit_denied_when_exceeding_overdraft_limit() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None, 0, 0).await.unwrap();
    let result = node.create_local_event("default".into(), "main".into(), -400, None, Some(200), 0, 0).await;
    assert!(result.is_err());
    assert_eq!(node.account_balance("default", "main"), 100);
}

#[tokio::test]
async fn credits_always_succeed_regardless_of_overdraft() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), -500, None, Some(1000), 0, 0).await.unwrap();
    assert!(node.create_local_event("default".into(), "main".into(), 100, None, None, 0, 0).await.is_ok());
    assert_eq!(node.account_balance("default", "main"), -400);
}

#[tokio::test]
async fn replicated_events_bypass_overdraft_check() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let event = shardd_types::Event {
        event_id: "replicated-1".into(), origin_node_id: "node-1".into(),
        origin_seq: 1, created_at_unix_ms: 0,
        bucket: "default".into(), account: "main".into(), amount: -999, note: None,
    };
    assert!(node.insert_event(event));
    assert_eq!(node.account_balance("default", "main"), -999);
}

#[tokio::test]
async fn exact_balance_debit_succeeds() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None, 0, 0).await.unwrap();
    assert!(node.create_local_event("default".into(), "main".into(), -100, None, None, 0, 0).await.is_ok());
    assert_eq!(node.account_balance("default", "main"), 0);
}

// ── Dedup + head + read + collapsed tests ────────────────────────────

#[tokio::test]
async fn deduplicates_by_origin_and_seq() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let event = shardd_types::Event {
        event_id: "dup-1".into(), origin_node_id: "remote".into(), origin_seq: 1,
        created_at_unix_ms: 1000, bucket: "default".into(), account: "alice".into(), amount: 100, note: None,
    };
    assert!(node.insert_event(event.clone()));
    assert!(!node.insert_event(event));
    assert_eq!(node.account_balance("default", "alice"), 100);
}

#[tokio::test]
async fn batch_insert_deduplicates() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let events: Vec<shardd_types::Event> = (1..=5).map(|i| shardd_types::Event {
        event_id: format!("batch-{i}"), origin_node_id: "remote".into(), origin_seq: i,
        created_at_unix_ms: 1000 + i as u64, bucket: "default".into(), account: "alice".into(), amount: 10, note: None,
    }).collect();
    assert_eq!(node.insert_events_batch(events.clone()).await, 5);
    assert_eq!(node.insert_events_batch(events).await, 0);
}

#[tokio::test]
async fn tracks_contiguous_head_per_origin() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    for i in 1..=3 {
        node.insert_event(shardd_types::Event {
            event_id: format!("h-{i}"), origin_node_id: "origin-a".into(), origin_seq: i,
            created_at_unix_ms: i as u64 * 1000, bucket: "b".into(), account: "a".into(), amount: 1, note: None,
        });
    }
    assert_eq!(*node.get_heads().get("origin-a").unwrap(), 3);
}

#[tokio::test]
async fn head_stops_at_gap_and_advances_on_fill() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    for i in [1, 3] {
        node.insert_event(shardd_types::Event {
            event_id: format!("gap-{i}"), origin_node_id: "origin-b".into(), origin_seq: i,
            created_at_unix_ms: i as u64 * 1000, bucket: "b".into(), account: "a".into(), amount: 1, note: None,
        });
    }
    assert_eq!(*node.get_heads().get("origin-b").unwrap(), 1);
    node.insert_event(shardd_types::Event {
        event_id: "gap-2".into(), origin_node_id: "origin-b".into(), origin_seq: 2,
        created_at_unix_ms: 2000, bucket: "b".into(), account: "a".into(), amount: 1, note: None,
    });
    assert_eq!(*node.get_heads().get("origin-b").unwrap(), 3);
}

#[tokio::test]
async fn all_balances_returns_all_accounts() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("bucket1".into(), "alice".into(), 100, None, None, 0, 0).await.unwrap();
    node.create_local_event("bucket1".into(), "bob".into(), 50, None, None, 0, 0).await.unwrap();
    let balances = node.all_balances();
    assert!(balances.iter().any(|b| b.bucket == "bucket1" && b.account == "alice" && b.balance == 100));
    assert!(balances.iter().any(|b| b.bucket == "bucket1" && b.account == "bob" && b.balance == 50));
}

#[tokio::test]
async fn event_count_tracks_total() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    assert_eq!(node.event_count(), 0);
    node.create_local_event("b".into(), "a".into(), 1, None, None, 0, 0).await.unwrap();
    assert_eq!(node.event_count(), 1);
}

#[tokio::test]
async fn checksum_is_deterministic() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("b".into(), "a".into(), 100, None, None, 0, 0).await.unwrap();
    let c1 = node.checksum().await;
    let c2 = node.checksum().await;
    assert_eq!(c1, c2);
    assert_eq!(c1.len(), 64);
}

#[tokio::test]
async fn collapsed_locally_confirmed_when_no_gaps() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "alice".into(), 100, None, None, 0, 0).await.unwrap();
    node.create_local_event("default".into(), "alice".into(), 50, None, None, 0, 0).await.unwrap();
    let collapsed = node.collapsed_state();
    assert_eq!(collapsed.get("default:alice").unwrap().status, "locally_confirmed");
    assert_eq!(collapsed.get("default:alice").unwrap().balance, 150);
}

#[tokio::test]
async fn collapsed_provisional_when_gaps_exist() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    for i in [1u64, 3] {
        node.insert_event(shardd_types::Event {
            event_id: format!("prov-{i}"), origin_node_id: "remote-gapped".into(), origin_seq: i,
            created_at_unix_ms: i * 1000, bucket: "default".into(), account: "bob".into(), amount: 10, note: None,
        });
    }
    let collapsed = node.collapsed_state();
    assert_eq!(collapsed.get("default:bob").unwrap().status, "provisional");
    assert_eq!(collapsed.get("default:bob").unwrap().balance, 20);
}

#[tokio::test]
async fn collapsed_single_account() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "alice".into(), 100, None, None, 0, 0).await.unwrap();
    let entry = node.collapsed_balance("default", "alice");
    assert_eq!(entry.balance, 100);
    assert_eq!(entry.status, "locally_confirmed");
}

#[tokio::test]
async fn persistence_tracking() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let stats = node.persistence_stats();
    assert_eq!(stats.buffered, 0);
    assert_eq!(stats.unpersisted, 0);

    node.create_local_event("b".into(), "a".into(), 100, None, None, 0, 0).await.unwrap();
    let stats = node.persistence_stats();
    assert_eq!(stats.buffered, 1);
    assert_eq!(stats.unpersisted, 1);
}
