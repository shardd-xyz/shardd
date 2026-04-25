//! `shardd balances list` — every account's balance in a bucket.
//! Reads the dashboard's bucket-detail endpoint and surfaces just the
//! account-balance projection so the output is focused.

use anyhow::Result;
use clap::Subcommand;
use reqwest::Method;
use serde_json::Value;

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum BalancesCmd {
    List {
        #[arg(long)]
        bucket: String,
        /// Print the full bucket-detail document instead of just the
        /// account list (useful for debugging).
        #[arg(long)]
        raw: bool,
    },
}

pub async fn run(cmd: BalancesCmd, dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = credentials::load()?;
    let dashboard_url = crate::cmd::resolve_dashboard_url(dashboard_url_override, &creds);
    let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;

    match cmd {
        BalancesCmd::List { bucket, raw } => {
            let path = format!("/api/developer/buckets/{}", crate::cmd::urlencode(&bucket));
            let v: Value = client.request_value(Method::GET, &path, None).await?;
            if raw {
                return print_json(&v);
            }
            // Project to just the accounts list when present; if the
            // shape changes, fall back to printing the whole doc.
            if let Some(accounts) = v.get("accounts") {
                print_json(accounts)
            } else {
                print_json(&v)
            }
        }
    }
}
