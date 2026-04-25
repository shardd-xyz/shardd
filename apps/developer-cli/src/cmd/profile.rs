//! `shardd profile get | update | export` — control plane on /api/user/*.

use anyhow::Result;
use clap::Subcommand;
use reqwest::Method;
use serde_json::{Value, json};

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum ProfileCmd {
    /// Print profile (calls /api/developer/me).
    Get,
    /// Update display name and/or language.
    Update {
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        language: Option<String>,
    },
    /// Stream the full account export (profile, keys, scopes, buckets)
    /// as JSON. Pipe to a file: `shardd profile export > export.json`.
    Export,
}

pub async fn run(cmd: ProfileCmd, dashboard_url_override: Option<&str>) -> Result<()> {
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
        ProfileCmd::Get => {
            let v: Value = client
                .request_value(Method::GET, "/api/developer/me", None)
                .await?;
            print_json(&v)
        }
        ProfileCmd::Update {
            display_name,
            language,
        } => {
            let mut body = json!({});
            if let Some(n) = display_name {
                body["display_name"] = json!(n);
            }
            if let Some(l) = language {
                body["language"] = json!(l);
            }
            let v: Value = client
                .request_value(Method::PATCH, "/api/user", Some(&body))
                .await?;
            print_json(&v)
        }
        ProfileCmd::Export => {
            let v: Value = client
                .request_value(Method::GET, "/api/user/export", None)
                .await?;
            print_json(&v)
        }
    }
}
