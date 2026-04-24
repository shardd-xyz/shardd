use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::app_error::AppResult;

#[async_trait]
pub trait AuditLogRepo: Send + Sync {
    async fn log(&self, entry: NewAuditEntry) -> AppResult<()>;
    async fn list(&self, limit: i64, offset: i64) -> AppResult<Vec<AuditEntry>>;
    async fn count(&self) -> AppResult<i64>;
}

#[derive(Debug, Clone)]
pub struct NewAuditEntry {
    pub admin_id: Uuid,
    pub admin_email: String,
    pub action: String,
    pub target_user_id: Option<Uuid>,
    pub target_email: Option<String>,
    pub metadata: JsonValue,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub admin_id: Option<Uuid>,
    pub admin_email: String,
    pub action: String,
    pub target_user_id: Option<Uuid>,
    pub target_email: Option<String>,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
}
