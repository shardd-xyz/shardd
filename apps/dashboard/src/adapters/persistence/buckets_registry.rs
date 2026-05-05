use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    adapters::persistence::PostgresPersistence,
    app_error::{AppError, AppResult},
    use_cases::buckets_registry::{
        BucketRegistry, BucketStatusFilter, EvmBucketStatus, OwnedBucket, WhitelistedAddress,
    },
};

#[derive(FromRow)]
struct OwnedBucketRow {
    name: String,
    created_at: DateTime<Utc>,
    archived_at: Option<DateTime<Utc>>,
    evm_enabled: bool,
    evm_paused: bool,
    evm_whitelist_enabled: bool,
}

impl From<OwnedBucketRow> for OwnedBucket {
    fn from(r: OwnedBucketRow) -> Self {
        Self {
            name: r.name,
            created_at: r.created_at,
            archived_at: r.archived_at,
            evm_enabled: r.evm_enabled,
            evm_paused: r.evm_paused,
            evm_whitelist_enabled: r.evm_whitelist_enabled,
        }
    }
}

#[derive(FromRow)]
struct WhitelistAddressRow {
    address: String,
    added_at: DateTime<Utc>,
}

#[async_trait]
impl BucketRegistry for PostgresPersistence {
    async fn create(&self, user_id: Uuid, name: &str) -> AppResult<OwnedBucket> {
        let row: Option<OwnedBucketRow> = sqlx::query_as(
            "INSERT INTO developer_buckets (user_id, name) \
             VALUES ($1, $2) \
             ON CONFLICT (user_id, name) DO NOTHING \
             RETURNING name, created_at, archived_at, evm_enabled, evm_paused, evm_whitelist_enabled",
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
        let sql = match filter {
            BucketStatusFilter::Active => {
                "SELECT name, created_at, archived_at, evm_enabled, evm_paused, evm_whitelist_enabled \
                 FROM developer_buckets \
                 WHERE user_id = $1 AND archived_at IS NULL \
                 ORDER BY created_at DESC"
            }
            BucketStatusFilter::Archived | BucketStatusFilter::Nuked => {
                "SELECT name, created_at, archived_at, evm_enabled, evm_paused, evm_whitelist_enabled \
                 FROM developer_buckets \
                 WHERE user_id = $1 AND archived_at IS NOT NULL \
                 ORDER BY created_at DESC"
            }
            BucketStatusFilter::All => {
                "SELECT name, created_at, archived_at, evm_enabled, evm_paused, evm_whitelist_enabled \
                 FROM developer_buckets \
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

    // ── EVM settings ─────────────────────────────────────────────

    async fn get_evm_status(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> AppResult<Option<EvmBucketStatus>> {
        let bucket: Option<OwnedBucketRow> = sqlx::query_as(
            "SELECT name, created_at, archived_at, evm_enabled, evm_paused, evm_whitelist_enabled \
             FROM developer_buckets \
             WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)?;

        let Some(b) = bucket else {
            return Ok(None);
        };

        let addresses: Vec<WhitelistAddressRow> = sqlx::query_as(
            "SELECT w.address, w.added_at \
             FROM bucket_evm_whitelist w \
             JOIN developer_buckets b ON w.bucket_id = b.id \
             WHERE b.user_id = $1 AND b.name = $2 \
             ORDER BY w.added_at",
        )
        .bind(user_id)
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(AppError::from)?;

        Ok(Some(EvmBucketStatus {
            enabled: b.evm_enabled,
            paused: b.evm_paused,
            whitelist_enabled: b.evm_whitelist_enabled,
            addresses: addresses
                .into_iter()
                .map(|r| WhitelistedAddress {
                    address: r.address,
                    added_at: r.added_at,
                })
                .collect(),
        }))
    }

    async fn set_evm_enabled(&self, user_id: Uuid, name: &str, enabled: bool) -> AppResult<bool> {
        let result = sqlx::query(
            "UPDATE developer_buckets SET evm_enabled = $3 WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_evm_paused(&self, user_id: Uuid, name: &str, paused: bool) -> AppResult<bool> {
        let result = sqlx::query(
            "UPDATE developer_buckets SET evm_paused = $3 WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .bind(paused)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_evm_whitelist_enabled(
        &self,
        user_id: Uuid,
        name: &str,
        enabled: bool,
    ) -> AppResult<bool> {
        let result = sqlx::query(
            "UPDATE developer_buckets SET evm_whitelist_enabled = $3 WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(result.rows_affected() > 0)
    }

    async fn add_whitelist_address(
        &self,
        user_id: Uuid,
        name: &str,
        address: &str,
    ) -> AppResult<WhitelistedAddress> {
        let row: WhitelistAddressRow = sqlx::query_as(
            "INSERT INTO bucket_evm_whitelist (bucket_id, address) \
             SELECT b.id, $3 \
             FROM developer_buckets b \
             WHERE b.user_id = $1 AND b.name = $2 \
             ON CONFLICT (bucket_id, address) DO UPDATE SET address = EXCLUDED.address \
             RETURNING address, added_at",
        )
        .bind(user_id)
        .bind(name)
        .bind(address)
        .fetch_one(&self.pool)
        .await
        .map_err(AppError::from)?;

        Ok(WhitelistedAddress {
            address: row.address,
            added_at: row.added_at,
        })
    }

    async fn remove_whitelist_address(
        &self,
        user_id: Uuid,
        name: &str,
        address: &str,
    ) -> AppResult<bool> {
        let result = sqlx::query(
            "DELETE FROM bucket_evm_whitelist \
             WHERE bucket_id = (SELECT b.id FROM developer_buckets b WHERE b.user_id = $1 AND b.name = $2) \
             AND address = $3",
        )
        .bind(user_id)
        .bind(name)
        .bind(address)
        .execute(&self.pool)
        .await
        .map_err(AppError::from)?;
        Ok(result.rows_affected() > 0)
    }
}
