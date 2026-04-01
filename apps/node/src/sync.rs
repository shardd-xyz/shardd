//! Catch-up sync — safety net for missed broadcasts.
//! Runs every 30s (not every 3s). Primary sync is via Broadcaster.

use std::collections::BTreeMap;
use std::time::Duration;
use tracing::{debug, info, warn};

use shardd_storage::StorageBackend;
use shardd_types::Event;

use crate::state::SharedState;

/// Slow catch-up sync loop. Safety net for:
/// - New node bootstrap (pull history from peers)
/// - Recovering events missed during network partitions
/// - Filling gaps from crashed nodes
pub async fn catchup_loop<S: StorageBackend>(state: SharedState<S>, interval_ms: u64) {
    let interval = Duration::from_millis(interval_ms);
    loop {
        tokio::time::sleep(interval).await;
        let peers = state.peers.lock().await.to_vec();
        if peers.is_empty() {
            continue;
        }

        let mut total_applied = 0usize;
        for peer in &peers {
            match catchup_from_peer(&state, peer).await {
                Ok(n) => total_applied += n,
                Err(e) => debug!(peer, error = %e, "catchup failed"),
            }
        }

        if total_applied > 0 {
            info!(events_applied = total_applied, "catchup sync complete");
        }
    }
}

/// Pull missing events from a single peer via HTTP.
async fn catchup_from_peer<S: StorageBackend>(
    state: &SharedState<S>,
    peer: &str,
) -> anyhow::Result<usize> {
    let client = reqwest::Client::new();
    let base = format!("http://{peer}");

    // Fetch peer's heads
    let resp = client
        .get(format!("{base}/heads"))
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    let peer_heads: BTreeMap<String, u64> = resp.json().await?;

    let local_heads = state.get_heads();

    let mut all_events = Vec::new();
    for (origin, &peer_head) in &peer_heads {
        let my_head = local_heads.get(origin).copied().unwrap_or(0);
        if peer_head <= my_head {
            continue;
        }
        let from_seq = my_head + 1;
        let to_seq = peer_head;
        debug!(origin, from_seq, to_seq, "catchup: fetching range");

        let resp = client
            .post(format!("{base}/events/range"))
            .json(&serde_json::json!({
                "origin_node_id": origin,
                "from_seq": from_seq,
                "to_seq": to_seq,
            }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        let events: Vec<Event> = resp.json().await?;
        all_events.extend(events);
    }

    if all_events.is_empty() {
        return Ok(0);
    }
    let applied = state.insert_events_batch(all_events).await;
    Ok(applied)
}

/// Trustless bootstrap: pull ALL events from peers, recompute state.
pub async fn bootstrap_from_peers<S: StorageBackend>(
    state: &SharedState<S>,
) {
    let peers = state.peers.lock().await.to_vec();
    if peers.is_empty() {
        debug!("no peers for bootstrap");
        return;
    }

    info!(peers = peers.len(), "starting trustless bootstrap from peers");

    let client = reqwest::Client::new();
    let mut total = 0usize;

    for peer in &peers {
        let base = format!("http://{peer}");

        let resp = match client
            .get(format!("{base}/heads"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => { warn!(peer, error = %e, "bootstrap: failed to get heads"); continue; }
        };

        let peer_heads: BTreeMap<String, u64> = match resp.json().await {
            Ok(h) => h,
            Err(e) => { warn!(peer, error = %e, "bootstrap: failed to parse heads"); continue; }
        };

        let local_heads = state.get_heads();

        for (origin, &peer_head) in &peer_heads {
            let my_head = local_heads.get(origin).copied().unwrap_or(0);
            if peer_head <= my_head { continue; }

            // Fetch in chunks of 10000
            let mut from = my_head + 1;
            while from <= peer_head {
                let to = (from + 9999).min(peer_head);
                let resp = client
                    .post(format!("{base}/events/range"))
                    .json(&serde_json::json!({
                        "origin_node_id": origin,
                        "from_seq": from,
                        "to_seq": to,
                    }))
                    .timeout(Duration::from_secs(30))
                    .send()
                    .await;

                if let Ok(resp) = resp {
                    if let Ok(events) = resp.json::<Vec<Event>>().await {
                        let n = state.insert_events_batch(events).await;
                        total += n;
                    }
                }
                from = to + 1;
            }
        }
    }

    info!(events = total, "bootstrap complete");
}
