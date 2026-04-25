//! `shardd events create | list` — calls the dashboard's wrapper
//! endpoints (which proxy to the gateway with proper edge selection).

use anyhow::Result;
use clap::Subcommand;
use reqwest::Method;
use serde_json::{Value, json};

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum EventsCmd {
    /// Credit, debit, or hold a balance. Positive = credit, negative = debit.
    Create {
        #[arg(long)]
        bucket: String,
        #[arg(long)]
        account: String,
        #[arg(long, allow_hyphen_values = true)]
        amount: i64,
        #[arg(long)]
        note: Option<String>,
        /// Pass an explicit nonce to make retries collapse onto one
        /// logical write. Generated automatically (UUID v4) by the
        /// dashboard if omitted.
        #[arg(long)]
        idempotency_nonce: Option<String>,
        #[arg(long)]
        max_overdraft: Option<u64>,
        #[arg(long)]
        min_acks: Option<u32>,
        #[arg(long)]
        ack_timeout_ms: Option<u64>,
    },
    /// List events in a bucket.
    List {
        #[arg(long)]
        bucket: String,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        page: Option<usize>,
        #[arg(long)]
        limit: Option<usize>,
    },
}

pub async fn run(cmd: EventsCmd, dashboard_url_override: Option<&str>) -> Result<()> {
    let creds = credentials::load()?;
    let dashboard_url = crate::cmd::resolve_dashboard_url(dashboard_url_override, &creds);
    let client = DashboardClient::new(dashboard_url, Some(creds.api_key.clone()))?;

    match cmd {
        EventsCmd::Create {
            bucket,
            account,
            amount,
            note,
            idempotency_nonce,
            max_overdraft,
            min_acks,
            ack_timeout_ms,
        } => {
            let mut body = json!({
                "account": account,
                "amount": amount,
            });
            if let Some(n) = note {
                body["note"] = json!(n);
            }
            if let Some(n) = idempotency_nonce {
                body["idempotency_nonce"] = json!(n);
            }
            if let Some(v) = max_overdraft {
                body["max_overdraft"] = json!(v);
            }
            if let Some(v) = min_acks {
                body["min_acks"] = json!(v);
            }
            if let Some(v) = ack_timeout_ms {
                body["ack_timeout_ms"] = json!(v);
            }
            let path = format!("/api/developer/buckets/{}/events", urlencode(&bucket));
            let v: Value = client
                .request_value(Method::POST, &path, Some(&body))
                .await?;
            print_json(&v)
        }
        EventsCmd::List {
            bucket,
            account,
            page,
            limit,
        } => {
            let mut path = format!("/api/developer/buckets/{}/events", urlencode(&bucket));
            let mut sep = "?";
            if let Some(a) = account.as_ref() {
                path.push_str(&format!("{sep}account={}", urlencode(a)));
                sep = "&";
            }
            if let Some(p) = page {
                path.push_str(&format!("{sep}page={p}"));
                sep = "&";
            }
            if let Some(l) = limit {
                path.push_str(&format!("{sep}limit={l}"));
                let _ = sep;
            }
            let v: Value = client.request_value(Method::GET, &path, None).await?;
            print_json(&v)
        }
    }
}

fn urlencode(s: &str) -> String {
    crate::cmd::urlencode(s)
}
