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
async fn four_nodes_400rps_consistency_and_overdraft() {
    const NUM_NODES: usize = 4;
    const EVENTS_PER_NODE: usize = 400;
    const MAX_PEERS: usize = 4;
    const DEBIT_AMOUNT: i64 = -10;

    // ── Phase 1: Create 4 isolated nodes ──
    let nodes: Vec<SharedState> = (0..NUM_NODES)
        .map(|i| {
            make_node(
                &format!("node-{i}"),
                &format!("127.0.0.1:{}", 9000 + i),
                MAX_PEERS,
            )
        })
        .collect();

    // ── Phase 2: Each node creates 400 events debiting the same account ──
    // Simulates 400 req/s per node for 1 second (1,600 total requests).
    // All hit the same (bucket, account), each debiting -10.
    let bucket = "default";
    let account = "shared-account";

    for node in &nodes {
        for _ in 0..EVENTS_PER_NODE {
            node.create_local_event(
                bucket.to_string(),
                account.to_string(),
                DEBIT_AMOUNT,
                None,
            );
        }
    }

    // ── Phase 3: Pre-sync assertions — overdraft is invisible ──
    let per_node_expected = EVENTS_PER_NODE as i64 * DEBIT_AMOUNT; // -4,000
    let global_expected = per_node_expected * NUM_NODES as i64; // -16,000

    println!("\n=== PRE-SYNC STATE ===");
    for (i, node) in nodes.iter().enumerate() {
        let balance = node.account_balance(bucket, account);
        let count = node.event_count();
        println!(
            "  node-{i}: events={count}, balance={balance} (sees only own debits)"
        );
        assert_eq!(count, EVENTS_PER_NODE, "node-{i} event count before sync");
        assert_eq!(balance, per_node_expected, "node-{i} balance before sync");
    }

    // Each node thinks the balance is -4,000 but reality is -16,000.
    // The invisible overdraft per node = what they don't see = 3 * 4,000 = 12,000.
    let max_invisible_overdraft = per_node_expected - global_expected; // 12,000
    println!("\n=== OVERDRAFT ANALYSIS ===");
    println!(
        "  Per-node visible balance:  {per_node_expected} (each sees only own {EVENTS_PER_NODE} debits)"
    );
    println!("  True global balance:       {global_expected} (across all {NUM_NODES} nodes)");
    println!(
        "  Max invisible overdraft:   {max_invisible_overdraft} per node (unseen debits from {} other nodes)",
        NUM_NODES - 1
    );
    println!(
        "  Overdraft ratio:           {:.0}% of true balance is hidden",
        (max_invisible_overdraft as f64 / global_expected.abs() as f64) * 100.0
    );

    // ── Phase 4: Full-mesh sync until convergence ──
    let sync_rounds = full_mesh_sync(&nodes);
    println!("\n=== SYNC ===");
    println!("  Converged in {sync_rounds} full-mesh round(s)");

    // ── Phase 5: Post-sync consistency ──
    let total_events = NUM_NODES * EVENTS_PER_NODE; // 1,600

    println!("\n=== POST-SYNC STATE ===");
    for (i, node) in nodes.iter().enumerate() {
        let balance = node.account_balance(bucket, account);
        let count = node.event_count();
        println!("  node-{i}: events={count}, balance={balance}");
        assert_eq!(
            count, total_events,
            "node-{i} should have {total_events} events after sync"
        );
        assert_eq!(
            balance, global_expected,
            "node-{i} should show global balance {global_expected} after sync"
        );
        assert_eq!(
            node.total_balance(),
            global_expected,
            "node-{i} total_balance mismatch"
        );
    }

    // ── Phase 6: Checksum consistency ──
    let checksums: Vec<String> = nodes.iter().map(|n| n.checksum()).collect();
    for i in 1..checksums.len() {
        assert_eq!(
            checksums[0], checksums[i],
            "checksum mismatch: node-0 vs node-{i}"
        );
    }
    println!("\n=== CONSISTENCY ===");
    println!("  All {} nodes share checksum: {}…", NUM_NODES, &checksums[0][..16]);

    // ── Phase 7: Heads consistency ──
    let all_heads: Vec<BTreeMap<String, u64>> = nodes.iter().map(|n| n.get_heads()).collect();
    for i in 1..all_heads.len() {
        assert_eq!(
            all_heads[0], all_heads[i],
            "heads mismatch: node-0 vs node-{i}"
        );
    }
    assert_eq!(
        all_heads[0].len(),
        NUM_NODES,
        "should have heads for all {NUM_NODES} origins"
    );
    for (origin, &head) in &all_heads[0] {
        assert_eq!(
            head, EVENTS_PER_NODE as u64,
            "origin {origin} contiguous head should be {EVENTS_PER_NODE}"
        );
    }
    println!(
        "  All heads match: {} origins, each at seq {}",
        all_heads[0].len(),
        EVENTS_PER_NODE
    );

    println!("\n=== RESULT: PASSED ===");
    println!(
        "  {} nodes × {} events × {} amount = {} true balance",
        NUM_NODES, EVENTS_PER_NODE, DEBIT_AMOUNT, global_expected
    );
    println!("  Pre-sync: each node was blind to {:.0}% of debits (overdraft of {max_invisible_overdraft})",
        ((NUM_NODES - 1) as f64 / NUM_NODES as f64) * 100.0
    );
    println!("  Post-sync: full consistency achieved across all nodes\n");
}
