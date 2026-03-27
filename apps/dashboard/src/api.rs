use shardd_types::*;
use std::collections::HashSet;

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
    bucket: &str,
    account: &str,
    amount: i64,
    note: Option<String>,
) -> Result<CreateEventResponse, reqwest::Error> {
    reqwest::Client::new()
        .post(format!("{base}/events"))
        .json(&CreateEventRequest {
            bucket: bucket.to_string(),
            account: account.to_string(),
            amount,
            note,
        })
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

/// Discover all nodes by crawling /peers from the bootstrap node and then
/// from discovered nodes until no new nodes are found.
pub async fn discover_nodes(bootstrap_url: &str) -> Result<Vec<String>, reqwest::Error> {
    let hostname = bootstrap_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(':')
        .next()
        .unwrap_or("127.0.0.1")
        .to_string();

    let client = reqwest::Client::new();
    let mut known_urls: HashSet<String> = HashSet::new();
    known_urls.insert(bootstrap_url.to_string());

    // Crawl: fetch peers from known nodes, add new ones, repeat
    let mut to_crawl = vec![bootstrap_url.to_string()];
    for _ in 0..5 {
        // max 5 rounds of crawling
        let mut next_crawl = Vec::new();
        for url in &to_crawl {
            // Get this node's peers
            let peers = match client.get(format!("{url}/peers")).send().await {
                Ok(resp) => resp.json::<Vec<String>>().await.unwrap_or_default(),
                Err(_) => continue,
            };
            // Also get this node's own address from /state
            let self_addr = match client.get(format!("{url}/state")).send().await {
                Ok(resp) => resp
                    .json::<StateResponse>()
                    .await
                    .ok()
                    .map(|s| s.addr),
                Err(_) => None,
            };

            // Collect all addresses
            let mut addrs: Vec<String> = peers;
            if let Some(addr) = self_addr {
                addrs.push(addr);
            }

            for addr in addrs {
                let port = addr.split(':').last().unwrap_or("3001");
                let url = format!("http://{hostname}:{port}");
                if known_urls.insert(url.clone()) {
                    next_crawl.push(url);
                }
            }
        }
        if next_crawl.is_empty() {
            break;
        }
        to_crawl = next_crawl;
    }

    let mut urls: Vec<String> = known_urls.into_iter().collect();
    urls.sort();
    Ok(urls)
}

/// Fetch state from all known node URLs concurrently.
pub async fn fetch_all_states(urls: &[String]) -> Vec<(String, StateResponse)> {
    let client = reqwest::Client::new();
    let futs: Vec<_> = urls
        .iter()
        .map(|url| {
            let client = client.clone();
            let url = url.clone();
            async move {
                let result = client
                    .get(format!("{url}/state"))
                    .send()
                    .await
                    .ok()?
                    .json::<StateResponse>()
                    .await
                    .ok()?;
                Some((url, result))
            }
        })
        .collect();
    let results = futures::future::join_all(futs).await;
    let mut out: Vec<(String, StateResponse)> =
        results.into_iter().flatten().collect();
    out.sort_by(|a, b| a.1.addr.cmp(&b.1.addr));
    out
}
