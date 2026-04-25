//! `shardd health [--edge URL]` — probe `/gateway/health` directly on
//! a public edge. Authentication-free per the public edge contract;
//! we hit the URL with raw reqwest rather than via the dashboard.
//!
//! Without --edge, picks the first ready edge from the dashboard's
//! /api/developer/edges directory.

use anyhow::{Context, Result, anyhow};
use reqwest::Method;
use serde_json::Value;

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

pub async fn run(edge_arg: Option<String>, dashboard_url_override: Option<&str>) -> Result<()> {
    let target = match edge_arg {
        Some(url) => url,
        None => {
            // Pick the first edge from the directory. No edges
            // configured → bail with a clear message.
            let creds = credentials::load()?;
            let dashboard_url = crate::cmd::resolve_dashboard_url(dashboard_url_override, &creds);
            let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;
            let v: Value = client
                .request_value(Method::GET, "/api/developer/edges", None)
                .await?;
            v.as_array()
                .and_then(|arr| arr.first())
                .and_then(|edge| edge.get("base_url"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("no edges configured — pass --edge <URL> explicitly"))?
        }
    };

    let url = format!("{}/gateway/health", target.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.context("parse health response")?;
    if !status.is_success() {
        return Err(anyhow!("edge returned {} for {}", status.as_u16(), url));
    }
    print_json(&body)
}
