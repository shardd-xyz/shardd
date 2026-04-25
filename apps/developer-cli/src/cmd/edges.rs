//! `shardd edges` — current regional edge directory (via dashboard).

use anyhow::Result;
use reqwest::Method;
use serde_json::Value;

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

pub async fn run(dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = credentials::load()?;
    let dashboard_url = crate::cmd::resolve_dashboard_url(dashboard_url_override, &creds);
    let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;
    let v: Value = client
        .request_value(Method::GET, "/api/developer/edges", None)
        .await?;
    print_json(&v)
}
