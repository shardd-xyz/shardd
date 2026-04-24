use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::{
    adapters::persistence::PostgresPersistence,
    app_error::{AppError, AppResult},
    use_cases::audit::{AuditEntry, AuditLogRepo, NewAuditEntry},
};

#[derive(sqlx::FromRow)]
struct AuditRow {
    id: Uuid,
    admin_id: Option<Uuid>,
    admin_email: String,
    action: String,
    target_user_id: Option<Uuid>,
    target_email: Option<String>,
    metadata: JsonValue,
    created_at: DateTime<Utc>,
}

impl From<AuditRow> for AuditEntry {
    fn from(r: AuditRow) -> Self {
        AuditEntry {
            id: r.id,
            admin_id: r.admin_id,
            admin_email: r.admin_email,
            action: r.action,
            target_user_id: r.target_user_id,
            target_email: r.target_email,
            metadata: r.metadata,
            created_at: r.created_at,
        }
    }
}

#[async_trait]
impl AuditLogRepo for PostgresPersistence {
    async fn log(&self, entry: NewAuditEntry) -> AppResult<()> {
        sqlx::query!(
            r#"
                INSERT INTO admin_audit_log
                    (admin_id, admin_email, action, target_user_id, target_email, metadata)
                VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            entry.admin_id,
            entry.admin_email,
            entry.action,
            entry.target_user_id,
            entry.target_email,
            entry.metadata,
        )
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(())
    }

    async fn list(&self, limit: i64, offset: i64) -> AppResult<Vec<AuditEntry>> {
        let rows = sqlx::query_as!(
            AuditRow,
            r#"
                SELECT id, admin_id, admin_email, action, target_user_id, target_email,
                       metadata as "metadata!: JsonValue", created_at
                FROM admin_audit_log
                ORDER BY created_at DESC
                LIMIT $1 OFFSET $2
            "#,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn count(&self) -> AppResult<i64> {
        let row = sqlx::query!("SELECT COUNT(*) as count FROM admin_audit_log")
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::from)?;
        Ok(row.count.unwrap_or(0))
    }
}
