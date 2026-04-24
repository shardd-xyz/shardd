use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::app_error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
pub struct OwnedBucket {
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Soft-delete timestamp. `None` for active buckets. A row with
    /// `archived_at.is_some()` may still be a *nuked* bucket — the purge
    /// handler archives the row too. The authoritative nuked signal
    /// lives in the mesh meta log (`deleted_buckets`).
    pub archived_at: Option<DateTime<Utc>>,
}

/// Scope for `BucketRegistry::list`. `Active` returns only un-archived
/// rows (the historical default). The other variants return rows whose
/// archive state matches; the final archived-vs-nuked split is done by
/// the caller using the mesh `deleted_buckets` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketStatusFilter {
    All,
    Active,
    Archived,
    Nuked,
}

#[async_trait]
pub trait BucketRegistry: Send + Sync {
    async fn create(&self, user_id: Uuid, name: &str) -> AppResult<OwnedBucket>;
    /// True iff the bucket exists for this user AND is not archived.
    /// Write-path checks should use this, not the raw `delete`-only logic.
    async fn exists(&self, user_id: Uuid, name: &str) -> AppResult<bool>;
    /// Filtered listing, newest first. `Active` returns only un-archived
    /// rows; `Archived`/`Nuked`/`All` return archived rows (the caller
    /// must cross-reference the mesh deleted set to split them).
    async fn list(&self, user_id: Uuid, filter: BucketStatusFilter) -> AppResult<Vec<OwnedBucket>>;
    /// Mark as archived: writes are rejected and the UI hides it. Event
    /// history on the mesh is untouched. Returns true if a row was updated.
    async fn archive(&self, user_id: Uuid, name: &str) -> AppResult<bool>;
    /// Count of non-archived buckets. Used for admin read-outs only; no
    /// longer gates account deletion (soft-delete replaces the old guard).
    async fn count(&self, user_id: Uuid) -> AppResult<i64>;
}

/// Bucket names are a subset of what the mesh accepts so we can safely pass
/// them through the URL path, the bucket-hashing helper in the gateway, and
/// storage indexes. Enforced on every create.
pub fn validate_bucket_name(name: &str) -> AppResult<()> {
    if name.is_empty() {
        return Err(AppError::InvalidInput("bucket name is required".into()));
    }
    if name.len() > 63 {
        return Err(AppError::InvalidInput(
            "bucket name must be 63 characters or fewer".into(),
        ));
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(AppError::InvalidInput(
            "bucket name must start with a letter or digit".into(),
        ));
    }
    for c in name.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
        if !ok {
            return Err(AppError::InvalidInput(
                "bucket name may only contain lowercase letters, digits, '-' or '_'".into(),
            ));
        }
    }
    Ok(())
}
