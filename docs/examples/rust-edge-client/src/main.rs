use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::env;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
struct PublicEdgeDirectoryResponse {
    edges: Vec<PublicEdgeSummary>,
}

#[derive(Debug, Clone, Deserialize)]
struct PublicEdgeSummary {
    edge_id: String,
    region: String,
    base_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PublicEdgeHealthResponse {
    edge_id: Option<String>,
    region: Option<String>,
    ready: bool,
    healthy_nodes: usize,
    overloaded: Option<bool>,
}

#[derive(Debug, Clone)]
struct Candidate {
    edge_id: String,
    region: String,
    base_url: String,
    latency_ms: u128,
    ready: bool,
    healthy_nodes: usize,
    overloaded: bool,
}

fn normalize_base_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn fetch_json<T: for<'de> Deserialize<'de>>(
    client: &Client,
    url: &str,
    api_key: Option<&str>,
) -> anyhow::Result<(T, u128)> {
    let mut request = client.get(url).header("accept", "application/json");
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let started = Instant::now();
    let response = request.send()?;
    let latency_ms = started.elapsed().as_millis();
    let response = response.error_for_status()?;
    Ok((response.json::<T>()?, latency_ms))
}

fn discover_edges(client: &Client, bootstraps: &[String]) -> BTreeMap<String, PublicEdgeSummary> {
    let mut edges = BTreeMap::new();
    for bootstrap in bootstraps {
        let base_url = normalize_base_url(bootstrap);
        edges.entry(base_url.clone()).or_insert(PublicEdgeSummary {
            edge_id: base_url.clone(),
            region: "unknown".into(),
            base_url: base_url.clone(),
        });
        let url = format!("{base_url}/gateway/edges");
        let Ok((directory, _)) = fetch_json::<PublicEdgeDirectoryResponse>(client, &url, None)
        else {
            continue;
        };
        for edge in directory.edges {
            edges.insert(normalize_base_url(&edge.base_url), edge);
        }
    }
    edges
}

fn probe_edge(client: &Client, edge: &PublicEdgeSummary) -> Option<Candidate> {
    let base_url = normalize_base_url(&edge.base_url);
    let url = format!("{base_url}/gateway/health");
    let Ok((health, latency_ms)) = fetch_json::<PublicEdgeHealthResponse>(client, &url, None)
    else {
        return None;
    };
    Some(Candidate {
        edge_id: health.edge_id.unwrap_or_else(|| edge.edge_id.clone()),
        region: health.region.unwrap_or_else(|| edge.region.clone()),
        base_url,
        latency_ms,
        ready: health.ready,
        healthy_nodes: health.healthy_nodes,
        overloaded: health.overloaded.unwrap_or(false),
    })
}

fn main() -> anyhow::Result<()> {
    let api_key = env::var("SHARDD_API_KEY").expect("set SHARDD_API_KEY");
    let bucket = env::var("SHARDD_BUCKET").unwrap_or_else(|_| "orders".into());
    let bootstraps = env::args().skip(1).collect::<Vec<_>>();
    if bootstraps.is_empty() {
        eprintln!("usage: cargo run -- <bootstrap-url> [more-bootstrap-urls]");
        std::process::exit(2);
    }

    let client = Client::builder().timeout(Duration::from_secs(3)).build()?;
    let discovered = discover_edges(&client, &bootstraps);
    let mut candidates = discovered
        .values()
        .filter_map(|edge| probe_edge(&client, edge))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        anyhow::bail!("no reachable public edges");
    }
    candidates.sort_by(|a, b| {
        (!a.ready)
            .cmp(&(!b.ready))
            .then_with(|| a.overloaded.cmp(&b.overloaded))
            .then_with(|| b.healthy_nodes.cmp(&a.healthy_nodes))
            .then_with(|| a.latency_ms.cmp(&b.latency_ms))
    });
    let selected = &candidates[0];

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "selected": {
                "edge_id": selected.edge_id,
                "region": selected.region,
                "base_url": selected.base_url,
                "latency_ms": selected.latency_ms,
                "ready": selected.ready,
                "healthy_nodes": selected.healthy_nodes,
                "overloaded": selected.overloaded
            }
        }))?
    );

    let response = client
        .get(format!("{}/balances?bucket={}", selected.base_url, bucket))
        .bearer_auth(api_key)
        .header("accept", "application/json")
        .send()?
        .error_for_status()?;

    println!("{}", response.text()?);
    Ok(())
}
