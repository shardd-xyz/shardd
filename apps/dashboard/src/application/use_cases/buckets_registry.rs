use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::app_error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
pub struct OwnedBucket {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub evm_enabled: bool,
    pub evm_paused: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvmBucketStatus {
    pub enabled: bool,
    pub paused: bool,
}

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
    async fn exists(&self, user_id: Uuid, name: &str) -> AppResult<bool>;
    async fn list(&self, user_id: Uuid, filter: BucketStatusFilter) -> AppResult<Vec<OwnedBucket>>;
    async fn archive(&self, user_id: Uuid, name: &str) -> AppResult<bool>;
    async fn count(&self, user_id: Uuid) -> AppResult<i64>;

    async fn get_evm_status(&self, user_id: Uuid, name: &str)
    -> AppResult<Option<EvmBucketStatus>>;
    async fn get_evm_status_by_name(&self, name: &str) -> AppResult<Option<EvmBucketStatus>>;
    async fn set_evm_enabled(&self, user_id: Uuid, name: &str, enabled: bool) -> AppResult<bool>;
    async fn set_evm_paused(&self, user_id: Uuid, name: &str, paused: bool) -> AppResult<bool>;
}

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
