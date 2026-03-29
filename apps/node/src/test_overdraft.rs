use std::collections::BTreeMap;

use shardd_storage::NullStorage;

use crate::peer::PeerSet;
use crate::state::SharedState;

/// Create a node with NullStorage and no pre-existing events.
fn make_node(node_id: &str, addr: &str, max_peers: usize) -> SharedState {
    let peers = PeerSet::new(max_peers, addr.to_string());
    SharedState::new(
        node_id.to_string(),
        addr.to_string(),
        1,
        peers,
        BTreeMap::new(),
        NullStorage,
    )
}

/// Simulate sync: pull all events from `src` that `dst` is missing.
fn sync_one_direction(src: &SharedState, dst: &SharedState) -> usize {
    let src_heads = src.get_heads();
    let dst_heads = dst.get_heads();
    let mut events = Vec::new();
    for (origin, &src_head) in &src_heads {
        let dst_head = dst_heads.get(origin).copied().unwrap_or(0);
        if src_head > dst_head {
            events.extend(src.get_events_range(origin, dst_head + 1, src_head));
        }
    }
    if events.is_empty() {
        0
    } else {
        dst.insert_events_batch(events)
    }
}

/// Full-mesh sync: every node pulls from every other, repeat until converged.
fn full_mesh_sync(nodes: &[SharedState]) -> usize {
    let mut rounds = 0;
    loop {
        rounds += 1;
        let mut total = 0usize;
        for i in 0..nodes.len() {
            for j in 0..nodes.len() {
                if i != j {
                    total += sync_one_direction(&nodes[j], &nodes[i]);
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
    // Scenario: account starts with +10,000 credit. Each of 4 nodes processes
    // 400 debit requests of -10 with max_overdraft=0 (no overdraft allowed).
    // Locally each node's guard sees 10,000 - 4,000 = 6,000 → all debits pass.
    // But the TRUE global balance is 10,000 - 16,000 = -6,000 (overdraft!).
    // The per-node guard is blind to other nodes' debits.

    const NUM_NODES: usize = 4;
    const EVENTS_PER_NODE: usize = 400;
    const MAX_PEERS: usize = 4;
    const DEBIT_AMOUNT: i64 = -10;
    const STARTING_CREDIT: i64 = 10_000;

    let bucket = "default";
    let account = "shared-account";

    // ── Phase 1: Create 4 isolated nodes, each with the starting credit ──
    let nodes: Vec<SharedState> = (0..NUM_NODES)
        .map(|i| {
            let node = make_node(
                &format!("node-{i}"),
                &format!("127.0.0.1:{}", 9000 + i),
                MAX_PEERS,
            );
            // Seed with starting credit (credits always succeed, no overdraft check)
            node.create_local_event(
                bucket.to_string(),
                account.to_string(),
                STARTING_CREDIT,
                Some("initial credit".to_string()),
                None,
            )
            .unwrap();
            node
        })
        .collect();

    // ── Phase 2: Each node fires 400 debits with max_overdraft=0 (guard active) ──
    // The guard checks LOCAL balance only. Each node sees 10,000 and debits -10
    // per request. All 400 pass locally because 10,000 - 4,000 = 6,000 >= 0.
    let mut accepted_per_node = vec![0usize; NUM_NODES];
    let mut denied_per_node = vec![0usize; NUM_NODES];

    for (i, node) in nodes.iter().enumerate() {
        for _ in 0..EVENTS_PER_NODE {
            match node.create_local_event(
                bucket.to_string(),
                account.to_string(),
                DEBIT_AMOUNT,
                None,
                None, // max_overdraft=None → floor=0, no overdraft allowed
            ) {
                Ok(_) => accepted_per_node[i] += 1,
                Err(_) => denied_per_node[i] += 1,
            }
        }
    }

    // ── Phase 3: Pre-sync — each node thinks it's fine ──
    println!("\n=== PRE-SYNC: OVERDRAFT GUARD BYPASS ===");
    println!("  Starting credit per node: {STARTING_CREDIT}");
    println!(
        "  Debits attempted: {} per node ({} total)",
        EVENTS_PER_NODE,
        EVENTS_PER_NODE * NUM_NODES
    );
    for i in 0..NUM_NODES {
        let balance = nodes[i].account_balance(bucket, account);
        println!(
            "  node-{i}: accepted={}, denied={}, local_balance={balance}",
            accepted_per_node[i], denied_per_node[i],
        );
        // Guard should have let all 400 through: 10,000 - 4,000 = 6,000 >= 0
        assert_eq!(accepted_per_node[i], EVENTS_PER_NODE);
        assert_eq!(denied_per_node[i], 0);
    }

    let total_accepted: usize = accepted_per_node.iter().sum();
    let total_debits = total_accepted as i64 * DEBIT_AMOUNT;
    // 4 credit events (one per node) + all accepted debits
    let total_credit = STARTING_CREDIT * NUM_NODES as i64;

    println!("\n=== OVERDRAFT ANALYSIS ===");
    println!("  Total credits across nodes: {total_credit} ({NUM_NODES} × {STARTING_CREDIT})");
    println!("  Total debits accepted:      {total_accepted} × {DEBIT_AMOUNT} = {total_debits}");

    // After sync, credits also replicate — every node will see ALL 4 credit events.
    // True global balance = (4 × 10,000) + (1,600 × -10) = 40,000 - 16,000 = 24,000.
    // BUT: each node's local view pre-sync is: 10,000 (own credit) + own debits.
    // They don't see the other 3 credits either. The point is that in a real system
    // there's ONE credit (not one per node). Let's also test that scenario.

    // ── Phase 4: Sync and verify consistency ──
    let sync_rounds = full_mesh_sync(&nodes);
    println!("\n=== SYNC ===");
    println!("  Converged in {sync_rounds} round(s)");

    // After sync: each node sees all 4 credits + all 1,600 debits
    let global_balance = total_credit + total_debits;
    println!("\n=== POST-SYNC STATE ===");
    for (i, node) in nodes.iter().enumerate() {
        let balance = node.account_balance(bucket, account);
        let count = node.event_count();
        println!("  node-{i}: events={count}, balance={balance}");
        assert_eq!(balance, global_balance);
    }

    let checksums: Vec<String> = nodes.iter().map(|n| n.checksum()).collect();
    for i in 1..checksums.len() {
        assert_eq!(checksums[0], checksums[i], "checksum mismatch node-0 vs node-{i}");
    }
    println!("\n  All checksums match: {}…", &checksums[0][..16]);
    println!(
        "  Global balance: {global_balance} (credits {total_credit} + debits {total_debits})"
    );
}

#[tokio::test]
async fn four_nodes_400rps_single_credit_overdraft_breach() {
    // The real-world scenario: ONE credit of 10,000 exists on node-0.
    // All 4 nodes debit against it. Each node's guard only sees local balance.
    // Node-0 sees balance drop from 10,000. Nodes 1-3 see balance 0 (no credit yet)
    // and should deny all debits with max_overdraft=0.
    //
    // With max_overdraft=5000, nodes 1-3 can each push to -5,000 locally.
    // After sync the true balance = 10,000 - (all accepted debits × 10).

    const NUM_NODES: usize = 4;
    const EVENTS_PER_NODE: usize = 400;
    const MAX_PEERS: usize = 4;
    const DEBIT_AMOUNT: i64 = -10;
    const STARTING_CREDIT: i64 = 10_000;
    const MAX_OVERDRAFT: u64 = 5_000;

    let bucket = "default";
    let account = "shared-account";

    // ── Phase 1: Create nodes — only node-0 has the credit ──
    let nodes: Vec<SharedState> = (0..NUM_NODES)
        .map(|i| {
            make_node(
                &format!("node-{i}"),
                &format!("127.0.0.1:{}", 9000 + i),
                MAX_PEERS,
            )
        })
        .collect();

    // Only node-0 gets the initial credit
    nodes[0]
        .create_local_event(
            bucket.to_string(),
            account.to_string(),
            STARTING_CREDIT,
            Some("initial credit".to_string()),
            None,
        )
        .unwrap();

    // ── Phase 2: All nodes fire 400 debits with max_overdraft=5000 ──
    let mut accepted = vec![0usize; NUM_NODES];
    let mut denied = vec![0usize; NUM_NODES];

    for (i, node) in nodes.iter().enumerate() {
        for _ in 0..EVENTS_PER_NODE {
            match node.create_local_event(
                bucket.to_string(),
                account.to_string(),
                DEBIT_AMOUNT,
                None,
                Some(MAX_OVERDRAFT),
            ) {
                Ok(_) => accepted[i] += 1,
                Err(_) => denied[i] += 1,
            }
        }
    }

    // ── Phase 3: Pre-sync analysis ──
    println!("\n=== SINGLE-CREDIT OVERDRAFT BREACH ===");
    println!("  Credit: {STARTING_CREDIT} on node-0 only");
    println!("  max_overdraft: {MAX_OVERDRAFT} (floor = -{})", MAX_OVERDRAFT);
    println!("  Debits: {EVENTS_PER_NODE} × {DEBIT_AMOUNT} attempted per node\n");

    for i in 0..NUM_NODES {
        let balance = nodes[i].account_balance(bucket, account);
        println!(
            "  node-{i}: accepted={:>3}, denied={:>3}, local_balance={balance}",
            accepted[i], denied[i]
        );
    }

    // node-0: starts at 10,000, can go to -5,000 → 15,000 headroom → 1,500 debits possible
    //         but only 400 attempted → all 400 accepted, balance = 10,000 - 4,000 = 6,000
    assert_eq!(accepted[0], EVENTS_PER_NODE);
    assert_eq!(nodes[0].account_balance(bucket, account),
        STARTING_CREDIT + (EVENTS_PER_NODE as i64 * DEBIT_AMOUNT));

    // nodes 1-3: start at 0, can go to -5,000 → 500 debits possible, but only 400 attempted
    //            all 400 accepted, balance = -4,000
    for i in 1..NUM_NODES {
        assert_eq!(accepted[i], EVENTS_PER_NODE);
        assert_eq!(
            nodes[i].account_balance(bucket, account),
            EVENTS_PER_NODE as i64 * DEBIT_AMOUNT
        );
    }

    let total_accepted: usize = accepted.iter().sum();
    let total_debit_value = total_accepted as i64 * DEBIT_AMOUNT;

    println!("\n=== OVERDRAFT BREACH ===");
    println!("  Total debits accepted: {total_accepted} (all nodes thought they were within limits)");
    println!("  Total debit value:     {total_debit_value}");
    println!("  Starting credit:       {STARTING_CREDIT}");
    let true_balance = STARTING_CREDIT + total_debit_value;
    println!("  True global balance:   {true_balance}");
    let overdraft_amount = if true_balance < 0 { -true_balance } else { 0 };
    let intended_floor = -(MAX_OVERDRAFT as i64);
    let breach = if true_balance < intended_floor {
        true_balance - intended_floor
    } else {
        0
    };
    println!("  Intended floor:        {intended_floor}");
    println!("  Overdraft amount:      {overdraft_amount}");
    println!(
        "  Breach past limit:     {} ({}x the allowed overdraft)",
        breach.abs(),
        if MAX_OVERDRAFT > 0 {
            breach.abs() as f64 / MAX_OVERDRAFT as f64
        } else {
            0.0
        }
    );

    // The guard allowed it because each node only saw its own balance.
    // 4 nodes × 400 debits × -10 = -16,000 total debits
    // true balance = 10,000 - 16,000 = -6,000
    // intended floor = -5,000, so we breached by 1,000
    assert_eq!(true_balance, STARTING_CREDIT + (NUM_NODES as i64 * EVENTS_PER_NODE as i64 * DEBIT_AMOUNT));
    assert!(true_balance < intended_floor, "should breach the overdraft limit");

    // ── Phase 4: Sync and verify convergence ──
    let sync_rounds = full_mesh_sync(&nodes);
    println!("\n=== POST-SYNC ===");
    println!("  Converged in {sync_rounds} round(s)");

    for (i, node) in nodes.iter().enumerate() {
        let balance = node.account_balance(bucket, account);
        let count = node.event_count();
        println!("  node-{i}: events={count}, balance={balance}");
        assert_eq!(balance, true_balance, "node-{i} balance mismatch after sync");
    }

    let checksums: Vec<String> = nodes.iter().map(|n| n.checksum()).collect();
    for i in 1..checksums.len() {
        assert_eq!(checksums[0], checksums[i], "checksum mismatch node-0 vs node-{i}");
    }
    println!("  All checksums match: {}…", &checksums[0][..16]);
    println!("\n=== RESULT ===");
    println!("  Overdraft guard was active (max_overdraft={MAX_OVERDRAFT}) but guards are LOCAL-only.");
    println!("  {NUM_NODES} nodes accepted {total_accepted} debits totalling {total_debit_value}.");
    println!("  True balance {true_balance} breached floor {intended_floor} by {}.\n", breach.abs());
}

#[tokio::test]
async fn debit_denied_when_balance_insufficient() {
    let node = make_node("node-0", "127.0.0.1:9000", 4);

    // Credit 100
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .unwrap();
    assert_eq!(node.account_balance("default", "main"), 100);

    // Debit -200 with no overdraft allowed → should fail
    let result = node.create_local_event("default".into(), "main".into(), -200, None, None);
    assert!(result.is_err());
    let (balance, projected) = result.unwrap_err();
    assert_eq!(balance, 100);
    assert_eq!(projected, -100);

    // Balance unchanged
    assert_eq!(node.account_balance("default", "main"), 100);
}

#[tokio::test]
async fn debit_allowed_within_overdraft_limit() {
    let node = make_node("node-0", "127.0.0.1:9000", 4);

    // Credit 100
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .unwrap();

    // Debit -200 with max_overdraft=200 → balance goes to -100, floor is -200 → allowed
    let result =
        node.create_local_event("default".into(), "main".into(), -200, None, Some(200));
    assert!(result.is_ok());
    assert_eq!(node.account_balance("default", "main"), -100);
}

#[tokio::test]
async fn debit_denied_when_exceeding_overdraft_limit() {
    let node = make_node("node-0", "127.0.0.1:9000", 4);

    // Credit 100
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .unwrap();

    // Debit -400 with max_overdraft=200 → projected -300 < floor -200 → denied
    let result =
        node.create_local_event("default".into(), "main".into(), -400, None, Some(200));
    assert!(result.is_err());
    let (balance, projected) = result.unwrap_err();
    assert_eq!(balance, 100);
    assert_eq!(projected, -300);

    // Balance unchanged
    assert_eq!(node.account_balance("default", "main"), 100);
}

#[tokio::test]
async fn credits_always_succeed_regardless_of_overdraft() {
    let node = make_node("node-0", "127.0.0.1:9000", 4);

    // Drive balance negative via unlimited overdraft
    node.create_local_event("default".into(), "main".into(), -500, None, Some(1000))
        .unwrap();
    assert_eq!(node.account_balance("default", "main"), -500);

    // Credit +100 with max_overdraft=None → should always succeed (credits bypass check)
    let result = node.create_local_event("default".into(), "main".into(), 100, None, None);
    assert!(result.is_ok());
    assert_eq!(node.account_balance("default", "main"), -400);
}

#[tokio::test]
async fn replicated_events_bypass_overdraft_check() {
    let node = make_node("node-0", "127.0.0.1:9000", 4);

    // Directly insert a replicated debit event that would violate overdraft
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
    let inserted = node.insert_event(event);
    assert!(inserted);
    assert_eq!(node.account_balance("default", "main"), -999);
}

#[tokio::test]
async fn exact_balance_debit_succeeds() {
    let node = make_node("node-0", "127.0.0.1:9000", 4);

    // Credit 100, then debit exactly -100 with no overdraft → balance hits 0 exactly
    node.create_local_event("default".into(), "main".into(), 100, None, None)
        .unwrap();
    let result = node.create_local_event("default".into(), "main".into(), -100, None, None);
    assert!(result.is_ok());
    assert_eq!(node.account_balance("default", "main"), 0);
}
