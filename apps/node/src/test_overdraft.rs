use shardd_storage::memory::InMemoryStorage;
use shardd_storage::StorageBackend;
use shardd_types::NodeMeta;

use crate::peer::PeerSet;
use crate::state::SharedState;

/// Create a node with InMemoryStorage and no pre-existing events.
async fn make_node(node_id: &str, addr: &str, max_peers: usize) -> SharedState<InMemoryStorage> {
    let storage = InMemoryStorage::new();
    // Pre-create node meta so allocate_seq works
    storage
        .save_node_meta(&NodeMeta {
            node_id: node_id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 0,
            next_seq: 1,
        })
        .await
        .unwrap();

    let peers = PeerSet::new(max_peers, addr.to_string());
    SharedState::new(node_id.to_string(), addr.to_string(), 1, peers, storage).await
}

/// Simulate sync: pull all events from `src` that `dst` is missing.
async fn sync_one_direction<S: shardd_storage::StorageBackend>(
    src: &SharedState<S>,
    dst: &SharedState<S>,
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
    if events.is_empty() {
        0
    } else {
        dst.insert_events_batch(events).await
    }
}

/// Full-mesh sync: every node pulls from every other, repeat until converged.
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
        if total == 0 {
            break;
        }
    }
    rounds
}

#[tokio::test]
async fn four_nodes_400rps_overdraft_guard_bypass() {
    const NUM_NODES: usize = 4;
    const EVENTS_PER_NODE: usize = 400;
    const MAX_PEERS: usize = 4;
    const DEBIT_AMOUNT: i64 = -10;
    const STARTING_CREDIT: i64 = 10_000;

    let bucket = "default";
    let account = "shared-account";

    let mut nodes = Vec::new();
    for i in 0..NUM_NODES {
        let node = make_node(
            &format!("node-{i}"),
            &format!("127.0.0.1:{}", 9000 + i),
            MAX_PEERS,
        )
        .await;
        node.create_local_event(
            bucket.to_string(),
            account.to_string(),
            STARTING_CREDIT,
            Some("initial credit".to_string()),
            None,
        )
        .await
        .unwrap();
        nodes.push(node);
    }

    let mut accepted_per_node = vec![0usize; NUM_NODES];
    let mut denied_per_node = vec![0usize; NUM_NODES];

    for (i, node) in nodes.iter().enumerate() {
        for _ in 0..EVENTS_PER_NODE {
            match node
                .create_local_event(
                    bucket.to_string(),
                    account.to_string(),
                    DEBIT_AMOUNT,
                    None,
                    None,
                )
                .await
            {
                Ok(_) => accepted_per_node[i] += 1,
                Err(_) => denied_per_node[i] += 1,
            }
        }
    }

    for i in 0..NUM_NODES {
        assert_eq!(accepted_per_node[i], EVENTS_PER_NODE);
        assert_eq!(denied_per_node[i], 0);
    }

    let total_accepted: usize = accepted_per_node.iter().sum();
    let total_debits = total_accepted as i64 * DEBIT_AMOUNT;
    let total_credit = STARTING_CREDIT * NUM_NODES as i64;

    let sync_rounds = full_mesh_sync(&nodes).await;
    println!("Converged in {sync_rounds} round(s)");

    let global_balance = total_credit + total_debits;
    for (i, node) in nodes.iter().enumerate() {
        let balance = node.account_balance(bucket, account);
        assert_eq!(balance, global_balance, "node-{i} balance mismatch");
    }

    let mut checksums = Vec::new();
    for node in &nodes {
        checksums.push(node.checksum().await);
    }
    for i in 1..checksums.len() {
        assert_eq!(checksums[0], checksums[i], "checksum mismatch node-0 vs node-{i}");
    }
}

#[tokio::test]
async fn four_nodes_400rps_single_credit_overdraft_breach() {
    const NUM_NODES: usize = 4;
    const EVENTS_PER_NODE: usize = 400;
    const MAX_PEERS: usize = 4;
    const DEBIT_AMOUNT: i64 = -10;
    const STARTING_CREDIT: i64 = 10_000;
    const MAX_OVERDRAFT: u64 = 5_000;

    let bucket = "default";
    let account = "shared-account";

    let mut nodes = Vec::new();
    for i in 0..NUM_NODES {
        nodes.push(
            make_node(
                &format!("node-{i}"),
                &format!("127.0.0.1:{}", 9000 + i),
                MAX_PEERS,
            )
            .await,
        );
    }

    nodes[0]
        .create_local_event(
            bucket.to_string(),
            account.to_string(),
            STARTING_CREDIT,
            Some("initial credit".to_string()),
            None,
        )
        .await
        .unwrap();

    let mut accepted = vec![0usize; NUM_NODES];

    for (i, node) in nodes.iter().enumerate() {
        for _ in 0..EVENTS_PER_NODE {
            match node
                .create_local_event(
                    bucket.to_string(),
                    account.to_string(),
                    DEBIT_AMOUNT,
                    None,
                    Some(MAX_OVERDRAFT),
                )
                .await
            {
                Ok(_) => accepted[i] += 1,
                Err(_) => {}
            }
        }
    }

    assert_eq!(accepted[0], EVENTS_PER_NODE);
    for i in 1..NUM_NODES {
        assert_eq!(accepted[i], EVENTS_PER_NODE);
    }

    let total_accepted: usize = accepted.iter().sum();
    let total_debit_value = total_accepted as i64 * DEBIT_AMOUNT;
    let true_balance = STARTING_CREDIT + total_debit_value;
    let intended_floor = -(MAX_OVERDRAFT as i64);
    assert!(true_balance < intended_floor, "should breach the overdraft limit");

    let sync_rounds = full_mesh_sync(&nodes).await;
    println!("Converged in {sync_rounds} round(s)");

    for (i, node) in nodes.iter().enumerate() {
        let balance = node.account_balance(bucket, account);
        assert_eq!(balance, true_balance, "node-{i} balance mismatch after sync");
    }

    let mut checksums = Vec::new();
    for node in &nodes {
        checksums.push(node.checksum().await);
    }
    for i in 1..checksums.len() {
        assert_eq!(checksums[0], checksums[i], "checksum mismatch node-0 vs node-{i}");
    }
}

#[tokio::test]
async fn debit_denied_when_balance_insufficient() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .await
        .unwrap();
    assert_eq!(node.account_balance("default", "main"), 100);

    let result = node
        .create_local_event("default".into(), "main".into(), -200, None, None)
        .await;
    assert!(result.is_err());
    let (balance, projected) = result.unwrap_err();
    assert_eq!(balance, 100);
    assert_eq!(projected, -100);
    assert_eq!(node.account_balance("default", "main"), 100);
}

#[tokio::test]
async fn debit_allowed_within_overdraft_limit() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .await
        .unwrap();

    let result = node
        .create_local_event("default".into(), "main".into(), -200, None, Some(200))
        .await;
    assert!(result.is_ok());
    assert_eq!(node.account_balance("default", "main"), -100);
}

#[tokio::test]
async fn debit_denied_when_exceeding_overdraft_limit() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .await
        .unwrap();

    let result = node
        .create_local_event("default".into(), "main".into(), -400, None, Some(200))
        .await;
    assert!(result.is_err());
    let (balance, projected) = result.unwrap_err();
    assert_eq!(balance, 100);
    assert_eq!(projected, -300);
    assert_eq!(node.account_balance("default", "main"), 100);
}

#[tokio::test]
async fn credits_always_succeed_regardless_of_overdraft() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), -500, None, Some(1000))
        .await
        .unwrap();
    assert_eq!(node.account_balance("default", "main"), -500);

    let result = node
        .create_local_event("default".into(), "main".into(), 100, None, None)
        .await;
    assert!(result.is_ok());
    assert_eq!(node.account_balance("default", "main"), -400);
}

#[tokio::test]
async fn replicated_events_bypass_overdraft_check() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let event = shardd_types::Event {
        event_id: "replicated-1".into(),
        origin_node_id: "node-1".into(),
        origin_seq: 1,
        created_at_unix_ms: 0,
        bucket: "default".into(),
        account: "main".into(),
        amount: -999,
        note: None,
    };
    let inserted = node.insert_event(event).await;
    assert!(inserted);
    assert_eq!(node.account_balance("default", "main"), -999);
}

#[tokio::test]
async fn exact_balance_debit_succeeds() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .await
        .unwrap();
    let result = node
        .create_local_event("default".into(), "main".into(), -100, None, None)
        .await;
    assert!(result.is_ok());
    assert_eq!(node.account_balance("default", "main"), 0);
}

// ── Deduplication tests (parity with elixir_ledger) ──────────────────

#[tokio::test]
async fn deduplicates_by_origin_and_seq() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let event = shardd_types::Event {
        event_id: "dup-1".into(),
        origin_node_id: "remote".into(),
        origin_seq: 1,
        created_at_unix_ms: 1000,
        bucket: "default".into(),
        account: "alice".into(),
        amount: 100,
        note: None,
    };
    assert!(node.insert_event(event.clone()).await);
    assert!(!node.insert_event(event).await); // duplicate
    assert_eq!(node.account_balance("default", "alice"), 100); // not 200
}

#[tokio::test]
async fn batch_insert_deduplicates() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    let events: Vec<shardd_types::Event> = (1..=5)
        .map(|i| shardd_types::Event {
            event_id: format!("batch-{i}"),
            origin_node_id: "remote".into(),
            origin_seq: i,
            created_at_unix_ms: 1000 + i as u64,
            bucket: "default".into(),
            account: "alice".into(),
            amount: 10,
            note: None,
        })
        .collect();
    assert_eq!(node.insert_events_batch(events.clone()).await, 5);
    assert_eq!(node.insert_events_batch(events).await, 0); // all duplicates
}

// ── Head tracking tests ──────────────────────────────────────────────

#[tokio::test]
async fn tracks_contiguous_head_per_origin() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    for i in 1..=3 {
        let event = shardd_types::Event {
            event_id: format!("h-{i}"),
            origin_node_id: "origin-a".into(),
            origin_seq: i,
            created_at_unix_ms: i as u64 * 1000,
            bucket: "b".into(),
            account: "a".into(),
            amount: 1,
            note: None,
        };
        node.insert_event(event).await;
    }
    assert_eq!(*node.get_heads().get("origin-a").unwrap(), 3);
}

#[tokio::test]
async fn head_stops_at_gap_and_advances_on_fill() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    // Insert seq 1 and 3, skip 2
    for i in [1, 3] {
        let event = shardd_types::Event {
            event_id: format!("gap-{i}"),
            origin_node_id: "origin-b".into(),
            origin_seq: i,
            created_at_unix_ms: i as u64 * 1000,
            bucket: "b".into(),
            account: "a".into(),
            amount: 1,
            note: None,
        };
        node.insert_event(event).await;
    }
    assert_eq!(*node.get_heads().get("origin-b").unwrap(), 1);

    // Fill the gap
    let event2 = shardd_types::Event {
        event_id: "gap-2".into(),
        origin_node_id: "origin-b".into(),
        origin_seq: 2,
        created_at_unix_ms: 2000,
        bucket: "b".into(),
        account: "a".into(),
        amount: 1,
        note: None,
    };
    node.insert_event(event2).await;
    assert_eq!(*node.get_heads().get("origin-b").unwrap(), 3);
}

// ── State read tests ─────────────────────────────────────────────────

#[tokio::test]
async fn get_events_returns_sorted_by_time() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("b".into(), "a".into(), 10, None, None).await.unwrap();
    node.create_local_event("b".into(), "a".into(), 20, None, None).await.unwrap();

    let events = node.all_events_sorted().await;
    assert_eq!(events.len(), 2);
    assert!(events[0].created_at_unix_ms <= events[1].created_at_unix_ms);
}

#[tokio::test]
async fn get_events_range_returns_correct_range() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("b".into(), "a".into(), 1, None, None).await.unwrap();
    node.create_local_event("b".into(), "a".into(), 2, None, None).await.unwrap();
    node.create_local_event("b".into(), "a".into(), 3, None, None).await.unwrap();

    let events = node.get_events_range("node-0", 2, 3).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].origin_seq, 2);
    assert_eq!(events[1].origin_seq, 3);
}

#[tokio::test]
async fn all_balances_returns_all_accounts() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("bucket1".into(), "alice".into(), 100, None, None).await.unwrap();
    node.create_local_event("bucket1".into(), "bob".into(), 50, None, None).await.unwrap();

    let balances = node.all_balances();
    assert_eq!(balances.len(), 2);
    assert!(balances.iter().any(|b| b.bucket == "bucket1" && b.account == "alice" && b.balance == 100));
    assert!(balances.iter().any(|b| b.bucket == "bucket1" && b.account == "bob" && b.balance == 50));
}

#[tokio::test]
async fn event_count_tracks_total() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    assert_eq!(node.event_count(), 0);
    node.create_local_event("b".into(), "a".into(), 1, None, None).await.unwrap();
    assert_eq!(node.event_count(), 1);
}

#[tokio::test]
async fn checksum_is_deterministic() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("b".into(), "a".into(), 100, None, None).await.unwrap();
    let c1 = node.checksum().await;
    let c2 = node.checksum().await;
    assert_eq!(c1, c2);
    assert_eq!(c1.len(), 64); // SHA-256 hex
}

// ── Collapsed state tests ────────────────────────────────────────────

#[tokio::test]
async fn collapsed_locally_confirmed_when_no_gaps() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "alice".into(), 100, None, None).await.unwrap();
    node.create_local_event("default".into(), "alice".into(), 50, None, None).await.unwrap();

    let collapsed = node.collapsed_state();
    let entry = collapsed.get("default:alice").unwrap();
    assert_eq!(entry.status, "locally_confirmed");
    assert_eq!(entry.balance, 150);
}

#[tokio::test]
async fn collapsed_provisional_when_gaps_exist() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    // Insert remote events with gap (seq 1 and 3, missing 2)
    for i in [1u64, 3] {
        let event = shardd_types::Event {
            event_id: format!("prov-{i}"),
            origin_node_id: "remote-gapped".into(),
            origin_seq: i,
            created_at_unix_ms: i * 1000,
            bucket: "default".into(),
            account: "bob".into(),
            amount: 10,
            note: None,
        };
        node.insert_event(event).await;
    }

    let collapsed = node.collapsed_state();
    let entry = collapsed.get("default:bob").unwrap();
    assert_eq!(entry.status, "provisional");
    assert_eq!(entry.balance, 20);
}

#[tokio::test]
async fn collapsed_single_account() {
    let node = make_node("node-0", "127.0.0.1:9000", 4).await;
    node.create_local_event("default".into(), "alice".into(), 100, None, None).await.unwrap();

    let entry = node.collapsed_balance("default", "alice");
    assert_eq!(entry.balance, 100);
    assert_eq!(entry.status, "locally_confirmed");
}
