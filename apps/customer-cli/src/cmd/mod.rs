pub mod accounts;
pub mod auth;
pub mod balances;
pub mod billing;
pub mod buckets;
pub mod edges;
pub mod events;
pub mod health;
pub mod keys;
pub mod profile;

use crate::credentials::{self, Credentials};

/// Resolve the dashboard URL with the same precedence rules as
/// credentials::dashboard_url, but starting from already-loaded
/// credentials so each subcommand doesn't re-read the file.
pub fn resolve_dashboard_url(cli_override: Option<&str>, creds: &Credentials) -> String {
    if let Some(u) = cli_override {
        return u.trim_end_matches('/').to_string();
    }
    if let Ok(env) = std::env::var("SHARDD_DASHBOARD_URL") {
        return env.trim_end_matches('/').to_string();
    }
    if !creds.dashboard_url.is_empty() {
        return creds.dashboard_url.trim_end_matches('/').to_string();
    }
    credentials::DEFAULT_DASHBOARD_URL.to_string()
}

/// Minimal RFC 3986 percent-encoding for path segments and query values.
/// Sufficient for bucket/account names (lowercase, digits, -, _) and
/// short status / search strings.
pub fn urlencode(s: &str) -> String {
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
