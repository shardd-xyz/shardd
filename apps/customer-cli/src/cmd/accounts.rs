//! `shardd accounts get` — single account's balance + active holds.
//! Pulls the bucket-detail document from the dashboard and filters
//! client-side. Returns the matching account row or 404 if missing.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use reqwest::Method;
use serde_json::Value;

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum AccountsCmd {
    Get {
        #[arg(long)]
        bucket: String,
        #[arg(long)]
        account: String,
    },
}

pub async fn run(cmd: AccountsCmd, dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = credentials::load()?;
    let dashboard_url = crate::cmd::resolve_dashboard_url(dashboard_url_override, &creds);
    let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;

    match cmd {
        AccountsCmd::Get { bucket, account } => {
            let path = format!("/api/developer/buckets/{}", crate::cmd::urlencode(&bucket));
            let v: Value = client.request_value(Method::GET, &path, None).await?;
            let accounts = v
                .get("accounts")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            let needle = account.as_str();
            let matched = accounts
                .into_iter()
                .find(|row| row.get("account").and_then(|a| a.as_str()) == Some(needle))
                .ok_or_else(|| anyhow!("account {needle} not found in bucket {bucket}"))?;
            print_json(&matched)
        }
    }
}
