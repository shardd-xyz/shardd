use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    adapters::persistence::PostgresPersistence,
    app_error::{AppError, AppResult},
    use_cases::buckets_registry::{BucketRegistry, BucketStatusFilter, OwnedBucket},
};

#[derive(FromRow)]
struct OwnedBucketRow {
    name: String,
    created_at: DateTime<Utc>,
    archived_at: Option<DateTime<Utc>>,
}

impl From<OwnedBucketRow> for OwnedBucket {
    fn from(r: OwnedBucketRow) -> Self {
        Self {
            name: r.name,
            created_at: r.created_at,
            archived_at: r.archived_at,
        }
    }
}

#[async_trait]
impl BucketRegistry for PostgresPersistence {
    async fn create(&self, user_id: Uuid, name: &str) -> AppResult<OwnedBucket> {
        let row: Option<OwnedBucketRow> = sqlx::query_as(
            "INSERT INTO developer_buckets (user_id, name) \
             VALUES ($1, $2) \
             ON CONFLICT (user_id, name) DO NOTHING \
             RETURNING name, created_at, archived_at",
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;

        match row {
            Some(r) => Ok(r.into()),
            None => Err(AppError::Conflict(format!(
                "bucket '{name}' already exists"
            ))),
        }
    }

    async fn exists(&self, user_id: Uuid, name: &str) -> AppResult<bool> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT true FROM developer_buckets \
             WHERE user_id = $1 AND name = $2 AND archived_at IS NULL",
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(row.is_some())
    }

    async fn list(&self, user_id: Uuid, filter: BucketStatusFilter) -> AppResult<Vec<OwnedBucket>> {
        // `Archived` and `Nuked` share the same SQL predicate — the
        // caller distinguishes them using the mesh `deleted_buckets` set.
        let sql = match filter {
            BucketStatusFilter::Active => {
                "SELECT name, created_at, archived_at FROM developer_buckets \
                 WHERE user_id = $1 AND archived_at IS NULL \
                 ORDER BY created_at DESC"
            }
            BucketStatusFilter::Archived | BucketStatusFilter::Nuked => {
                "SELECT name, created_at, archived_at FROM developer_buckets \
                 WHERE user_id = $1 AND archived_at IS NOT NULL \
                 ORDER BY created_at DESC"
            }
            BucketStatusFilter::All => {
                "SELECT name, created_at, archived_at FROM developer_buckets \
                 WHERE user_id = $1 \
                 ORDER BY created_at DESC"
            }
        };
        let rows: Vec<OwnedBucketRow> = sqlx::query_as(sql)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(AppError::from)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn archive(&self, user_id: Uuid, name: &str) -> AppResult<bool> {
        let result = sqlx::query(
            "UPDATE developer_buckets \
             SET archived_at = COALESCE(archived_at, NOW()) \
             WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(result.rows_affected() > 0)
    }

    async fn count(&self, user_id: Uuid) -> AppResult<i64> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM developer_buckets \
             WHERE user_id = $1 AND archived_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(n)
    }
}
