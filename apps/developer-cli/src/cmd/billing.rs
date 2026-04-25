//! `shardd billing status | plans | portal` — control plane on
//! /api/billing/*. Read-only from the CLI; checkout + plan changes
//! are deliberately funneled through the hosted Stripe portal.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum BillingCmd {
    /// Plan, credits remaining, monthly allowance, subscription status.
    Status,
    /// List available plans.
    Plans,
    /// Open the hosted Stripe portal in the browser. Returns the URL
    /// either way (so you can copy/paste if auto-open fails).
    Portal,
}

pub async fn run(cmd: BillingCmd, dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = credentials::load()?;
    let dashboard_url = dashboard_url_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if creds.dashboard_url.is_empty() {
                credentials::DEFAULT_DASHBOARD_URL.to_string()
            } else {
                creds.dashboard_url.clone()
            }
        });
    let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;

    match cmd {
        BillingCmd::Status => {
            let v: Value = client
                .request_value(Method::GET, "/api/billing/status", None)
                .await?;
            print_json(&v)
        }
        BillingCmd::Plans => {
            let v: Value = client
                .request_value(Method::GET, "/api/billing/plans", None)
                .await?;
            print_json(&v)
        }
        BillingCmd::Portal => {
            #[derive(Deserialize)]
            struct PortalResp {
                #[serde(default)]
                url: Option<String>,
            }
            let resp: PortalResp = client
                .request_json::<PortalResp, ()>(Method::POST, "/api/billing/portal", None)
                .await?;
            let url = resp
                .url
                .ok_or_else(|| anyhow!("portal endpoint returned no URL"))?;
            eprintln!("    Opening Stripe portal: {url}");
            if let Err(err) = webbrowser::open(&url) {
                eprintln!("    (couldn't auto-open: {err})");
            }
            print_json(&serde_json::json!({"url": url}))
        }
    }
}
