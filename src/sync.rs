use std::collections::BTreeMap;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::model::Event;
use crate::state::SharedState;

/// Background sync loop: periodically pull missing events from random peers.
pub async fn sync_loop(state: SharedState, interval_ms: u64, fanout: usize) {
    let interval = Duration::from_millis(interval_ms);
    loop {
        tokio::time::sleep(interval).await;
        let peers = {
            let st = state.lock().await;
            st.peers.random_sample(fanout)
        };
        if peers.is_empty() {
            continue;
        }
        debug!(peer_count = peers.len(), "sync round starting");
        let mut total_applied = 0usize;
        for peer in &peers {
            match sync_with_peer(&state, peer).await {
                Ok(n) => total_applied += n,
                Err(e) => warn!(peer, error = %e, "sync failed"),
            }
        }
        if total_applied > 0 {
            info!(events_applied = total_applied, "sync round complete");
        } else {
            debug!("sync round complete, already converged");
        }
    }
}

/// Pull missing events from a single peer.
async fn sync_with_peer(state: &SharedState, peer: &str) -> anyhow::Result<usize> {
    let client = reqwest::Client::new();
    let base = format!("http://{peer}");

    // Fetch peer's heads.
    let resp = client
        .get(format!("{base}/heads"))
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    let peer_heads: BTreeMap<String, u64> = resp.json().await?;

    // Fetch peer's peers for discovery.
    if let Ok(resp) = client
        .get(format!("{base}/peers"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
    {
        if let Ok(remote_peers) = resp.json::<Vec<String>>().await {
            let mut st = state.lock().await;
            let added_any = remote_peers.iter().any(|p| st.peers.add(p));
            if added_any {
                let _ = st.persist_peers().await;
            }
        }
    }

    // Compare heads and pull missing ranges.
    let mut applied = 0usize;
    for (origin, &peer_head) in &peer_heads {
        let my_head = {
            let st = state.lock().await;
            st.contiguous_heads.get(origin).copied().unwrap_or(0)
        };
        if peer_head <= my_head {
            continue;
        }
        let from_seq = my_head + 1;
        let to_seq = peer_head;
        debug!(origin, from_seq, to_seq, "fetching range from peer");

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

        let mut st = state.lock().await;
        for event in events {
            match st.insert_event(event).await {
                Ok(true) => applied += 1,
                Ok(false) => {} // duplicate
                Err(e) => warn!(error = %e, "failed to insert replicated event"),
            }
        }
    }

    Ok(applied)
}

/// Eagerly push a single event to a few random peers.
pub async fn eager_push(state: &SharedState, event: &Event, fanout: usize) {
    let peers = {
        let st = state.lock().await;
        st.peers.random_sample(fanout)
    };
    let client = reqwest::Client::new();
    for peer in peers {
        let event = event.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let url = format!("http://{peer}/events/replicate");
            if let Err(e) = client
                .post(&url)
                .json(&event)
                .timeout(Duration::from_secs(3))
                .send()
                .await
            {
                debug!(peer, error = %e, "eager push failed");
            }
        });
    }
}
