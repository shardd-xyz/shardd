use crate::api::{ApiError, api_get};
use crate::types::*;

/// Per-user events listing. Server scopes the response to the caller's
/// bucket namespace; callers don't need to pass a user id.
pub async fn list_events(
    filter: &AdminEventsFilter,
    page: usize,
    limit: usize,
    replication: bool,
) -> Result<AdminEventListResponse, ApiError> {
    let offset = (page - 1) * limit;
    let mut params = vec![format!("limit={limit}"), format!("offset={offset}")];
    if !filter.bucket.is_empty() {
        params.push(format!("bucket={}", urlencoding::encode(&filter.bucket)));
    }
    if !filter.account.is_empty() {
        params.push(format!("account={}", urlencoding::encode(&filter.account)));
    }
    if !filter.origin.is_empty() {
        params.push(format!("origin={}", urlencoding::encode(&filter.origin)));
    }
    if !filter.event_type.is_empty() {
        params.push(format!(
            "event_type={}",
            urlencoding::encode(&filter.event_type)
        ));
    }
    if let Some(since) = filter.since_ms {
        params.push(format!("since_ms={since}"));
    }
    if let Some(until) = filter.until_ms {
        params.push(format!("until_ms={until}"));
    }
    if !filter.search.is_empty() {
        params.push(format!("search={}", urlencoding::encode(&filter.search)));
    }
    if replication {
        params.push("replication=true".to_string());
    }
    api_get(&format!("/api/developer/events?{}", params.join("&"))).await
}
