use crate::api::{ApiError, api_delete, api_get, api_post, api_put, api_put_no_body};
use crate::types::*;
use serde::Serialize;

pub async fn list_buckets(
    query: &str,
    page: usize,
    limit: usize,
    status: &str,
) -> Result<BucketListResponse, ApiError> {
    let mut params = vec![format!("page={page}"), format!("limit={limit}")];
    if !query.is_empty() {
        params.push(format!("q={}", urlencoding::encode(query)));
    }
    if !status.is_empty() {
        params.push(format!("status={}", urlencoding::encode(status)));
    }
    api_get(&format!("/api/developer/buckets?{}", params.join("&"))).await
}

pub async fn get_bucket_detail(bucket: &str) -> Result<BucketDetailResponse, ApiError> {
    api_get(&format!(
        "/api/developer/buckets/{}",
        urlencoding::encode(bucket)
    ))
    .await
}

pub async fn list_bucket_events(
    bucket: &str,
    query: &str,
    account: &str,
    page: usize,
    limit: usize,
) -> Result<EventListResponse, ApiError> {
    let mut params = vec![format!("page={page}"), format!("limit={limit}")];
    if !query.is_empty() {
        params.push(format!("q={}", urlencoding::encode(query)));
    }
    if !account.is_empty() {
        params.push(format!("account={}", urlencoding::encode(account)));
    }
    api_get(&format!(
        "/api/developer/buckets/{}/events?{}",
        urlencoding::encode(bucket),
        params.join("&")
    ))
    .await
}

pub async fn create_event(
    bucket: &str,
    req: &CreateEventRequest,
) -> Result<serde_json::Value, ApiError> {
    api_post(
        &format!(
            "/api/developer/buckets/{}/events",
            urlencoding::encode(bucket)
        ),
        req,
    )
    .await
}

pub async fn list_edges() -> Result<Vec<EdgeInfo>, ApiError> {
    api_get("/api/developer/edges").await
}

#[derive(Serialize)]
struct CreateBucketBody<'a> {
    name: &'a str,
}

pub async fn create_bucket(name: &str) -> Result<serde_json::Value, ApiError> {
    api_post("/api/developer/buckets", &CreateBucketBody { name }).await
}

pub async fn archive_bucket(bucket: &str) -> Result<(), ApiError> {
    // Server keeps the path shape as DELETE for REST ergonomics, but the
    // handler archives instead of purging. See apps/dashboard-ui `DangerZone`.
    api_delete(&format!(
        "/api/developer/buckets/{}",
        urlencoding::encode(bucket)
    ))
    .await
}

/// §3.5: permanuke. Hard-deletes the bucket cluster-wide by emitting a
/// `BucketDelete` meta event. The `?confirm=<bucket>` guard is the
/// backend check against accidental click-through; the UI also enforces
/// a typed-confirmation modal before calling this. There is no undo.
pub async fn purge_bucket(bucket: &str) -> Result<(), ApiError> {
    api_delete(&format!(
        "/api/developer/buckets/{}/purge?confirm={}",
        urlencoding::encode(bucket),
        urlencoding::encode(bucket)
    ))
    .await
}

// ── EVM settings ────────────────────────────────────────────────

pub async fn get_evm_status(bucket: &str) -> Result<EvmBucketStatus, ApiError> {
    api_get(&format!(
        "/api/developer/buckets/{}/evm",
        urlencoding::encode(bucket)
    ))
    .await
}

pub async fn enable_evm(bucket: &str) -> Result<serde_json::Value, ApiError> {
    api_put(
        &format!(
            "/api/developer/buckets/{}/evm/enable",
            urlencoding::encode(bucket)
        ),
        &serde_json::json!({}),
    )
    .await
}

pub async fn disable_evm(bucket: &str) -> Result<serde_json::Value, ApiError> {
    api_put(
        &format!(
            "/api/developer/buckets/{}/evm/disable",
            urlencoding::encode(bucket)
        ),
        &serde_json::json!({}),
    )
    .await
}

pub async fn pause_evm(bucket: &str) -> Result<(), ApiError> {
    api_put_no_body(&format!(
        "/api/developer/buckets/{}/evm/pause",
        urlencoding::encode(bucket)
    ))
    .await
}

pub async fn resume_evm(bucket: &str) -> Result<(), ApiError> {
    api_put_no_body(&format!(
        "/api/developer/buckets/{}/evm/resume",
        urlencoding::encode(bucket)
    ))
    .await
}

pub async fn enable_evm_whitelist(bucket: &str) -> Result<(), ApiError> {
    api_put_no_body(&format!(
        "/api/developer/buckets/{}/evm/whitelist/enable",
        urlencoding::encode(bucket)
    ))
    .await
}

pub async fn disable_evm_whitelist(bucket: &str) -> Result<(), ApiError> {
    api_put_no_body(&format!(
        "/api/developer/buckets/{}/evm/whitelist/disable",
        urlencoding::encode(bucket)
    ))
    .await
}

pub async fn add_whitelist_address(
    bucket: &str,
    address: &str,
) -> Result<serde_json::Value, ApiError> {
    api_post(
        &format!(
            "/api/developer/buckets/{}/evm/whitelist/addresses",
            urlencoding::encode(bucket)
        ),
        &serde_json::json!({ "address": address }),
    )
    .await
}

pub async fn remove_whitelist_address(bucket: &str, address: &str) -> Result<(), ApiError> {
    api_delete(&format!(
        "/api/developer/buckets/{}/evm/whitelist/addresses/{}",
        urlencoding::encode(bucket),
        urlencoding::encode(address)
    ))
    .await
}
