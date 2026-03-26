use shardd_types::*;

pub async fn fetch_events(base: &str) -> Result<Vec<Event>, reqwest::Error> {
    reqwest::Client::new()
        .get(format!("{base}/events"))
        .send()
        .await?
        .json()
        .await
}

pub async fn create_event(
    base: &str,
    amount: i64,
    note: Option<String>,
) -> Result<CreateEventResponse, reqwest::Error> {
    reqwest::Client::new()
        .post(format!("{base}/events"))
        .json(&CreateEventRequest { amount, note })
        .send()
        .await?
        .json()
        .await
}

pub async fn trigger_sync(base: &str) -> Result<SyncTriggerResponse, reqwest::Error> {
    reqwest::Client::new()
        .post(format!("{base}/sync"))
        .send()
        .await?
        .json()
        .await
}

/// Discover all nodes by fetching /peers from the bootstrap node.
/// Maps internal peer addresses to public URLs using the bootstrap hostname.
pub async fn discover_nodes(bootstrap_url: &str) -> Result<Vec<String>, reqwest::Error> {
    // Parse hostname from bootstrap URL (e.g. "http://16.162.34.54:3001" -> "16.162.34.54")
    let hostname = bootstrap_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(':')
        .next()
        .unwrap_or("127.0.0.1")
        .to_string();

    // Get the state to find what port this node is on
    let state: StateResponse = reqwest::Client::new()
        .get(format!("{bootstrap_url}/state"))
        .send()
        .await?
        .json()
        .await?;

    // Get peers (these are internal addresses like "node2:3002")
    let peers: Vec<String> = reqwest::Client::new()
        .get(format!("{bootstrap_url}/peers"))
        .send()
        .await?
        .json()
        .await?;

    let mut urls = vec![bootstrap_url.to_string()];

    for peer in &peers {
        // Extract port from peer address (e.g. "node2:3002" -> "3002")
        let port = peer.split(':').last().unwrap_or("3001");
        let url = format!("http://{hostname}:{port}");
        if !urls.contains(&url) {
            urls.push(url);
        }
    }

    // Also include bootstrap node's own port in case it's not in the peer list
    let self_port = state.addr.split(':').last().unwrap_or("3001");
    let self_url = format!("http://{hostname}:{self_port}");
    if !urls.contains(&self_url) {
        urls.push(self_url);
    }

    urls.sort();
    urls.dedup();
    Ok(urls)
}

/// Fetch state from all known node URLs. Returns map of node_id -> (url, state).
pub async fn fetch_all_states(urls: &[String]) -> Vec<(String, StateResponse)> {
    let mut results = Vec::new();
    let client = reqwest::Client::new();
    for url in urls {
        if let Ok(resp) = client.get(format!("{url}/state")).send().await {
            if let Ok(state) = resp.json::<StateResponse>().await {
                results.push((url.clone(), state));
            }
        }
    }
    results.sort_by(|a, b| a.1.addr.cmp(&b.1.addr));
    results
}
