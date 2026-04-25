//! `shardd buckets list | create | get | archive | purge` — control plane.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use reqwest::Method;
use serde_json::{Value, json};

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum BucketsCmd {
    /// List your buckets (paginated).
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long, alias = "search")]
        q: Option<String>,
        #[arg(long, default_value = "1")]
        page: usize,
        #[arg(long, default_value = "25")]
        limit: usize,
    },
    /// Create a new bucket. Names must be lowercase letters/digits/-/_.
    Create {
        #[arg(long)]
        name: String,
    },
    /// Detail view: accounts + recent events for a bucket.
    Get {
        #[arg(long)]
        name: String,
    },
    /// Soft-archive a bucket. Reversible (mesh state stays intact).
    Archive {
        #[arg(long)]
        name: String,
    },
    /// Permanently delete a bucket cluster-wide. NO UNDO.
    /// `--confirm` must equal the bucket name.
    Purge {
        #[arg(long)]
        name: String,
        #[arg(long)]
        confirm: String,
    },
}

pub async fn run(cmd: BucketsCmd, dashboard_url_override: Option<&str>) -> Result<()> {
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
        BucketsCmd::List {
            status,
            q,
            page,
            limit,
        } => {
            let mut path = format!("/api/developer/buckets?page={page}&limit={limit}");
            if let Some(s) = status {
                path.push_str(&format!("&status={}", urlencode(&s)));
            }
            if let Some(s) = q {
                path.push_str(&format!("&q={}", urlencode(&s)));
            }
            let v: Value = client.request_value(Method::GET, &path, None).await?;
            print_json(&v)
        }
        BucketsCmd::Create { name } => {
            let body = json!({ "name": name });
            let v: Value = client
                .request_value(Method::POST, "/api/developer/buckets", Some(&body))
                .await?;
            print_json(&v)
        }
        BucketsCmd::Get { name } => {
            let path = format!("/api/developer/buckets/{}", urlencode(&name));
            let v: Value = client.request_value(Method::GET, &path, None).await?;
            print_json(&v)
        }
        BucketsCmd::Archive { name } => {
            let path = format!("/api/developer/buckets/{}", urlencode(&name));
            client
                .request_no_content(Method::DELETE, &path, None)
                .await?;
            print_json(&json!({"archived": name}))
        }
        BucketsCmd::Purge { name, confirm } => {
            if confirm != name {
                return Err(anyhow!(
                    "--confirm must equal the bucket name; refusing to purge"
                ));
            }
            let path = format!(
                "/api/developer/buckets/{}/purge?confirm={}",
                urlencode(&name),
                urlencode(&confirm)
            );
            client
                .request_no_content(Method::DELETE, &path, None)
                .await?;
            print_json(&json!({"purged": name}))
        }
    }
}

fn urlencode(s: &str) -> String {
    // Minimal — replace anything outside the unreserved-char set per
    // RFC 3986. Adequate for bucket names (lowercase, digits, -, _)
    // and short status strings.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
