//! `shardd keys list | create | rotate | revoke | scopes …` — control plane.

use anyhow::Result;
use clap::Subcommand;
use reqwest::Method;
use serde_json::{Value, json};

use crate::credentials;
use crate::http::DashboardClient;
use crate::output::print_json;

#[derive(Subcommand)]
pub enum KeysCmd {
    List,
    /// Create a new API key. Returns the raw key once — capture it.
    Create {
        #[arg(long)]
        name: String,
        /// RFC 3339 timestamp; key never expires if omitted.
        #[arg(long)]
        expires_at: Option<String>,
        /// Grant scope(s). Use the format
        ///   all:rw  | exact:<bucket>:rw | prefix:<bucket>:r
        /// Repeatable. Default: a single "all:rw" scope when omitted.
        #[arg(long = "scope")]
        scopes: Vec<String>,
    },
    Rotate {
        key_id: String,
    },
    Revoke {
        key_id: String,
    },
    Scopes {
        #[command(subcommand)]
        cmd: ScopesCmd,
    },
}

#[derive(Subcommand)]
pub enum ScopesCmd {
    List {
        key_id: String,
    },
    Add {
        key_id: String,
        /// `all`, `exact`, or `prefix`.
        #[arg(long)]
        match_type: String,
        #[arg(long)]
        bucket: Option<String>,
        #[arg(long)]
        read: bool,
        #[arg(long)]
        write: bool,
    },
    Remove {
        scope_id: String,
    },
}

pub async fn run(cmd: KeysCmd, dashboard_url_override: Option<&str>) -> Result<()> {
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
        KeysCmd::List => {
            let v: Value = client
                .request_value(Method::GET, "/api/developer/keys", None)
                .await?;
            print_json(&v)
        }
        KeysCmd::Create {
            name,
            expires_at,
            scopes,
        } => {
            let scopes_json = build_scopes(&scopes)?;
            let mut body = json!({
                "name": name,
                "scopes": scopes_json,
            });
            if let Some(e) = expires_at {
                body["expires_at"] = json!(e);
            }
            let v: Value = client
                .request_value(Method::POST, "/api/developer/keys", Some(&body))
                .await?;
            print_json(&v)
        }
        KeysCmd::Rotate { key_id } => {
            let path = format!("/api/developer/keys/{key_id}/rotate");
            let v: Value = client.request_value(Method::POST, &path, None).await?;
            print_json(&v)
        }
        KeysCmd::Revoke { key_id } => {
            let path = format!("/api/developer/keys/{key_id}/revoke");
            client.request_no_content(Method::POST, &path, None).await?;
            print_json(&json!({"revoked": key_id}))
        }
        KeysCmd::Scopes { cmd } => match cmd {
            ScopesCmd::List { key_id } => {
                let path = format!("/api/developer/keys/{key_id}/scopes");
                let v: Value = client.request_value(Method::GET, &path, None).await?;
                print_json(&v)
            }
            ScopesCmd::Add {
                key_id,
                match_type,
                bucket,
                read,
                write,
            } => {
                let body = json!({
                    "match_type": match_type,
                    "bucket": bucket,
                    "can_read": read,
                    "can_write": write,
                });
                let path = format!("/api/developer/keys/{key_id}/scopes");
                let v: Value = client
                    .request_value(Method::POST, &path, Some(&body))
                    .await?;
                print_json(&v)
            }
            ScopesCmd::Remove { scope_id } => {
                let path = format!("/api/developer/scopes/{scope_id}");
                client
                    .request_no_content(Method::DELETE, &path, None)
                    .await?;
                print_json(&json!({"removed": scope_id}))
            }
        },
    }
}

/// Parse a `--scope` flag value into the JSON shape the dashboard expects.
/// Accepts:
///   "all:rw" | "all:r" | "all:w"
///   "exact:<bucket>:rw"
///   "prefix:<bucket>:rw"
fn build_scopes(raw: &[String]) -> Result<Vec<Value>> {
    if raw.is_empty() {
        // Default for `keys create` with no --scope: all + read+write,
        // mirroring the dashboard's quick-create path.
        return Ok(vec![json!({
            "match_type": "all",
            "bucket": null,
            "can_read": true,
            "can_write": true,
        })]);
    }
    raw.iter().map(parse_one_scope).collect()
}

fn parse_one_scope(s: &String) -> Result<Value> {
    let parts: Vec<&str> = s.split(':').collect();
    let (match_type, bucket, perms) = match parts.as_slice() {
        ["all", perms] => ("all", None, *perms),
        ["exact", bucket, perms] => ("exact", Some(bucket.to_string()), *perms),
        ["prefix", bucket, perms] => ("prefix", Some(bucket.to_string()), *perms),
        _ => {
            return Err(anyhow::anyhow!(
                "could not parse --scope {s} (expected all:<perms>, exact:<bucket>:<perms>, or prefix:<bucket>:<perms>)"
            ));
        }
    };
    let can_read = perms.contains('r');
    let can_write = perms.contains('w');
    if !can_read && !can_write {
        return Err(anyhow::anyhow!("--scope {s} grants neither r nor w"));
    }
    Ok(json!({
        "match_type": match_type,
        "bucket": bucket,
        "can_read": can_read,
        "can_write": can_write,
    }))
}
