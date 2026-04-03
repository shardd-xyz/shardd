//! Catch-up sync + trustless bootstrap per protocol.md §4.2, §4.3.
//!
//! Safety net: runs every 30s to fetch missed events from peers.
//! Bootstrap: pulls ALL events from ALL origins for new/restarting nodes.

use std::collections::BTreeMap;
use std::time::Duration;
use tracing::{debug, info, warn};

use shardd_storage::StorageBackend;
use shardd_types::Event;

use crate::state::SharedState;

/// Slow catch-up sync loop (§4.2). Safety net, not primary sync.
pub async fn catchup_loop<S: StorageBackend>(state: SharedState<S>, interval_ms: u64) {
    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
    interval.tick().await;

    loop {
        interval.tick().await;

        // Get peers from registry or bootstrap list
        // For now, use a simple peer list (will be enhanced with registry in production)
        let peers: Vec<String> = vec![]; // TODO: get from registry/SWIM

        for peer in &peers {
            match catchup_from_peer(&state, peer).await {
                Ok(n) if n > 0 => info!(peer, events = n, "catch-up sync applied events"),
                Ok(_) => {}
                Err(e) => debug!(peer, error = %e, "catch-up sync failed"),
            }
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

    // Fetch peer's heads (epoch-aware)
    let resp = client.get(format!("{base}/heads"))
        .timeout(Duration::from_secs(5)).send().await?;
    let peer_heads: BTreeMap<String, u64> = resp.json().await?;

    let local_heads = state.get_heads();
    let mut all_events = Vec::new();

    for (key, &peer_head) in &peer_heads {
        let my_head = local_heads.get(key).copied().unwrap_or(0);
        if peer_head <= my_head { continue; }

        // Parse epoch-aware key: "origin:epoch"
        let parts: Vec<&str> = key.rsplitn(2, ':').collect();
        if parts.len() != 2 { continue; }
        let epoch: u32 = match parts[0].parse() {
            Ok(e) => e,
            Err(_) => continue,
        };
        let origin = parts[1];

        let from_seq = my_head + 1;
        let to_seq = peer_head;

        let resp = client.post(format!("{base}/events/range"))
            .json(&serde_json::json!({
                "origin_node_id": origin,
                "origin_epoch": epoch,
                "from_seq": from_seq,
                "to_seq": to_seq,
            }))
            .timeout(Duration::from_secs(10))
            .send().await?;
        let events: Vec<Event> = resp.json().await?;
        all_events.extend(events);
    }

    if all_events.is_empty() { return Ok(0); }
    Ok(state.insert_events_batch(&all_events).await)
}

/// Trustless bootstrap (§4.3): pull ALL events from ALL origins.
pub async fn bootstrap_from_peers<S: StorageBackend>(
    state: &SharedState<S>,
    peers: &[String],
) {
    if peers.is_empty() {
        debug!("no peers for bootstrap");
        return;
    }

    info!(peers = peers.len(), "starting trustless bootstrap");
    let client = reqwest::Client::new();
    let mut total = 0usize;

    for peer in peers {
        let base = format!("http://{peer}");

        let resp = match client.get(format!("{base}/heads"))
            .timeout(Duration::from_secs(10)).send().await {
            Ok(r) => r,
            Err(e) => { warn!(peer, error = %e, "bootstrap: heads fetch failed"); continue; }
        };

        let peer_heads: BTreeMap<String, u64> = match resp.json().await {
            Ok(h) => h,
            Err(e) => { warn!(peer, error = %e, "bootstrap: heads parse failed"); continue; }
        };

        let local_heads = state.get_heads();

        for (key, &peer_head) in &peer_heads {
            let my_head = local_heads.get(key).copied().unwrap_or(0);
            if peer_head <= my_head { continue; }

            let parts: Vec<&str> = key.rsplitn(2, ':').collect();
            if parts.len() != 2 { continue; }
            let epoch: u32 = match parts[0].parse() { Ok(e) => e, Err(_) => continue };
            let origin = parts[1];

            // Fetch in chunks of 10000
            let mut from = my_head + 1;
            while from <= peer_head {
                let to = (from + 9999).min(peer_head);
                if let Ok(resp) = client.post(format!("{base}/events/range"))
                    .json(&serde_json::json!({
                        "origin_node_id": origin,
                        "origin_epoch": epoch,
                        "from_seq": from,
                        "to_seq": to,
                    }))
                    .timeout(Duration::from_secs(30))
                    .send().await
                {
                    if let Ok(events) = resp.json::<Vec<Event>>().await {
                        total += state.insert_events_batch(&events).await;
                    }
                }
                from = to + 1;
            }
        }
    }

    info!(events = total, "bootstrap complete");
}
